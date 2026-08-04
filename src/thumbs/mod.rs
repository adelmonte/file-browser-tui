pub mod generator;

use std::{
    collections::HashSet,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::mpsc,
};

use lru::LruCache;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::icons::flatten_onto;

pub struct ThumbReady {
    pub path: PathBuf,
    pub image: image::DynamicImage,
}

struct ThumbRequest {
    path: PathBuf,
    mime_type: String,
}

type ProtoKey = (PathBuf, u16, u16, Option<(u8, u8, u8)>);

pub struct ThumbCache {
    images: LruCache<PathBuf, image::DynamicImage>,
    protocols: LruCache<ProtoKey, StatefulProtocol>,
    tx_work: mpsc::SyncSender<ThumbRequest>,
    rx_image: mpsc::Receiver<ThumbReady>,
    pending: HashSet<PathBuf>,
}

impl ThumbCache {
    pub fn new() -> Self {
        let (tx_work, rx_work) = mpsc::sync_channel::<ThumbRequest>(64);
        let (tx_image, rx_image) = mpsc::channel::<ThumbReady>();

        std::thread::spawn(move || {
            while let Ok(req) = rx_work.recv() {
                let tx = tx_image.clone();
                rayon::spawn(move || {
                    match generate_or_load_thumb(&req.path, &req.mime_type) {
                        Ok(img) => { let _ = tx.send(ThumbReady { path: req.path, image: img }); }
                        Err(e) => tracing::debug!("thumb {:?}: {e}", req.path),
                    }
                });
            }
        });

        Self {
            images: LruCache::new(NonZeroUsize::new(400).unwrap()),
            protocols: LruCache::new(NonZeroUsize::new(200).unwrap()),
            tx_work,
            rx_image,
            pending: HashSet::new(),
        }
    }

    pub fn request(&mut self, path: &Path, mime_type: &str) {
        if self.images.contains(path) || self.pending.contains(path) {
            return;
        }
        self.pending.insert(path.to_path_buf());
        let _ = self.tx_work.try_send(ThumbRequest {
            path: path.to_path_buf(),
            mime_type: mime_type.to_string(),
        });
    }

    pub fn drain_ready(&mut self) {
        while let Ok(ready) = self.rx_image.try_recv() {
            self.pending.remove(&ready.path);
            self.images.put(ready.path, ready.image);
        }
    }

    pub fn get_mut(
        &mut self,
        picker: &Picker,
        path: &Path,
        cell_w: u16,
        cell_h: u16,
        bg: Option<(u8, u8, u8)>,
    ) -> Option<&mut StatefulProtocol> {
        let key = (path.to_path_buf(), cell_w, cell_h, bg);
        if !self.protocols.contains(&key) {
            let img = self.images.get(path)?;
            let img = match bg {
                Some(rgb) => flatten_onto(img, rgb),
                None => img.clone(),
            };
            self.protocols.put(key.clone(), picker.new_resize_protocol(img));
        }
        self.protocols.get_mut(&key)
    }

    pub fn invalidate(&mut self, path: &Path) {
        let keys: Vec<ProtoKey> = self.protocols.iter()
            .filter(|(k, _)| k.0 == path)
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.protocols.pop(&k);
        }
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.protocols.clear();
        self.pending.clear();
    }

    pub fn clear_protocols(&mut self) {
        self.protocols.clear();
    }
}

fn thumbnail_cache_path(abs_path: &Path) -> PathBuf {
    // freedesktop spec: md5 of the file URI, no trailing newline
    let uri = format!("file://{}", abs_path.to_string_lossy());
    let hex = format!("{:x}", md5::compute(uri.as_bytes()));

    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("thumbnails/large")
        .join(format!("{hex}.png"))
}

fn is_thumb_valid(thumb_path: &Path, source_path: &Path) -> bool {
    if !thumb_path.exists() {
        return false;
    }
    let mtime = |p: &Path| std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(0);

    // Valid if thumb is newer than source (simplified mtime check)
    mtime(thumb_path) >= mtime(source_path)
}

fn generate_or_load_thumb(path: &Path, mime_type: &str) -> anyhow::Result<image::DynamicImage> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let thumb_path = thumbnail_cache_path(&abs);

    if is_thumb_valid(&thumb_path, &abs) {
        return Ok(image::open(&thumb_path)?);
    }

    // Ensure cache dir exists
    if let Some(parent) = thumb_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = thumb_path.with_extension("tmp.png");

    if mime_type.starts_with("video/") {
        generator::ffmpegthumbnailer(&abs, &tmp)?;
    } else if mime_type.starts_with("image/") {
        generator::image_thumb(&abs, &tmp)?;
    } else {
        anyhow::bail!("no thumbnailer for {mime_type}");
    }

    std::fs::rename(&tmp, &thumb_path)?;
    Ok(image::open(&thumb_path)?)
}
