pub mod folder_preview;
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
    /// `None` = resolved but no icon found (don't re-request).
    images: LruCache<PathBuf, Option<image::DynamicImage>>,
    /// Resolved mime types; independent of icon size, so survives `clear`.
    mimes: LruCache<PathBuf, Option<String>>,
    protocols: LruCache<ProtoKey, StatefulProtocol>,
    tx_work: mpsc::SyncSender<IconRequest>,
    rx_image: mpsc::Receiver<IconImageReady>,
    /// Paths for which a request is already in-flight
    pending: HashSet<PathBuf>,
}

impl IconCache {
    /// `folder_previews`: follow each folder icon with a version showing
    /// previews of its media (see `folder_preview`).
    pub fn new(folder_previews: bool) -> Self {
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
                    let wants_preview = folder_previews && req.is_dir && req.target_px >= folder_preview::MIN_PX;
                    let preview_base = if wants_preview { image.clone() } else { None };

                    // Always send back (even if no icon) so mime_type reaches the listing
                    let _ = tx.send(IconImageReady { path: req.path.clone(), image, mime_type: mime.clone() });

                    // Plain folder shows immediately; the preview replaces it when ready
                    if let Some(base) = preview_base
                        && let Some(preview) = folder_preview::build(&req.path, &base)
                    {
                        let _ = tx.send(IconImageReady { path: req.path, image: Some(preview), mime_type: mime });
                    }
                });
            }
        });

        Self {
            images: LruCache::new(NonZeroUsize::new(1000).unwrap()),
            protocols: LruCache::new(NonZeroUsize::new(500).unwrap()),
            mimes: LruCache::new(NonZeroUsize::new(20_000).unwrap()),
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
        let req = IconRequest { path: path.to_path_buf(), is_dir, target_px, icon_override };
        // Only mark pending if queued; a full queue is retried next frame
        if self.tx_work.try_send(req).is_ok() {
            self.pending.insert(path.to_path_buf());
        }
    }

    /// Mime type from a previous resolution of this path, if any.
    pub fn cached_mime(&self, path: &Path) -> Option<&Option<String>> {
        self.mimes.peek(path)
    }

    /// Drain completed icon loads; returns mime type updates for each path.
    pub fn drain_ready(&mut self) -> Vec<(PathBuf, Option<String>)> {
        let mut mime_updates = Vec::new();
        while let Ok(ready) = self.rx_image.try_recv() {
            self.pending.remove(&ready.path);
            // An upgrade (folder preview) replaces an icon already on screen
            if self.images.contains(&ready.path) {
                self.invalidate(&ready.path);
            }
            self.images.put(ready.path.clone(), ready.image);
            self.mimes.put(ready.path.clone(), ready.mime_type.clone());
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
            let img = self.images.get(path)?.as_ref()?;
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
