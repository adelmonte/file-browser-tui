pub mod raster;
pub mod resolver;

use std::{
    collections::HashSet,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::mpsc,
};

use lru::LruCache;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

pub struct IconImageReady {
    pub path: PathBuf,
    pub image: Option<image::DynamicImage>,
    pub mime_type: Option<String>,
}

struct IconRequest {
    path: PathBuf,
    is_dir: bool,
    target_px: u32,
    /// Custom icon name (config folder_icons) tried before normal resolution.
    icon_override: Option<String>,
}

/// Flatten an RGBA image onto a solid background color so the highlight shows
/// behind the icon (Kitty composites alpha over the terminal bg, not cell bg).
pub fn flatten_onto(img: &image::DynamicImage, rgb: (u8, u8, u8)) -> image::DynamicImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([rgb.0, rgb.1, rgb.2, 255]));
    image::imageops::overlay(&mut canvas, &rgba, 0, 0);
    image::DynamicImage::ImageRgba8(canvas)
}

/// Protocol cache key: (path, cell_w, cell_h, bg color variant).
type ProtoKey = (PathBuf, u16, u16, Option<(u8, u8, u8)>);

pub struct IconCache {
    /// Raw rasterized icons keyed by path; protocols are rebuilt from these.
    images: LruCache<PathBuf, image::DynamicImage>,
    protocols: LruCache<ProtoKey, StatefulProtocol>,
    tx_work: mpsc::SyncSender<IconRequest>,
    rx_image: mpsc::Receiver<IconImageReady>,
    /// Paths for which a request is already in-flight
    pending: HashSet<PathBuf>,
}

impl IconCache {
    pub fn new() -> Self {
        let (tx_work, rx_work) = mpsc::sync_channel::<IconRequest>(128);
        let (tx_image, rx_image) = mpsc::channel::<IconImageReady>();

        std::thread::spawn(move || {
            while let Ok(req) = rx_work.recv() {
                let tx = tx_image.clone();
                rayon::spawn(move || {
                    let (mut icon_names, mime) = resolver::resolve_names_and_mime(&req.path, req.is_dir);
                    if let Some(name) = req.icon_override {
                        icon_names.insert(0, name);
                    }
                    let resolved = resolver::resolve_icon_path(&icon_names, req.is_dir);
                    let image = resolved.and_then(|(icon_path, is_svg)| {
                        raster::load_icon(&icon_path, is_svg, req.target_px)
                            .map_err(|e| { tracing::warn!("load_icon {:?}: {e}", icon_path); e })
                            .ok()
                    });
                    // Always send back (even if no icon) so mime_type reaches the listing
                    let _ = tx.send(IconImageReady { path: req.path, image, mime_type: mime });
                });
            }
        });

        Self {
            images: LruCache::new(NonZeroUsize::new(1000).unwrap()),
            protocols: LruCache::new(NonZeroUsize::new(500).unwrap()),
            tx_work,
            rx_image,
            pending: HashSet::new(),
        }
    }

    /// Queue an icon load if not already loaded or pending.
    pub fn request(&mut self, path: &Path, is_dir: bool, target_px: u32, icon_override: Option<String>) {
        if self.images.contains(path) || self.pending.contains(path) {
            return;
        }
        self.pending.insert(path.to_path_buf());
        let _ = self.tx_work.try_send(IconRequest {
            path: path.to_path_buf(),
            is_dir,
            target_px,
            icon_override,
        });
    }

    /// Drain completed icon loads; returns mime type updates for each path.
    pub fn drain_ready(&mut self) -> Vec<(PathBuf, Option<String>)> {
        let mut mime_updates = Vec::new();
        while let Ok(ready) = self.rx_image.try_recv() {
            self.pending.remove(&ready.path);
            if let Some(img) = ready.image {
                self.images.put(ready.path.clone(), img);
            }
            mime_updates.push((ready.path, ready.mime_type));
        }
        mime_updates
    }

    /// Get (building lazily from the raw image) a protocol for rendering.
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

    /// Drop all protocol variants for a path (raw image stays cached).
    /// Used when a Kitty placement was deleted and must be re-transmitted.
    pub fn invalidate(&mut self, path: &Path) {
        let keys: Vec<ProtoKey> = self.protocols.iter()
            .filter(|(k, _)| k.0 == path)
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.protocols.pop(&k);
        }
    }

    /// Clear all cached protocols and raw images (e.g. on icon size change).
    pub fn clear(&mut self) {
        self.images.clear();
        self.protocols.clear();
        self.pending.clear();
    }

    /// Clear only encoded protocols, keeping raw images (e.g. terminal resize).
    pub fn clear_protocols(&mut self) {
        self.protocols.clear();
    }
}
