pub mod generator;

use std::{
    collections::HashSet,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::mpsc,
};

use lru::LruCache;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::icons::{GRID_FIT, fit_to_cells};

pub struct ThumbReady {
    pub path: PathBuf,
    /// None when no thumbnail could be made (the file shows its icon).
    pub image: Option<image::DynamicImage>,
}

struct ThumbRequest {
    path: PathBuf,
    mime_type: String,
}

type ProtoKey = (PathBuf, u16, u16, Option<(u8, u8, u8)>, bool);

pub struct ThumbCache {
    images: LruCache<PathBuf, image::DynamicImage>,
    protocols: LruCache<ProtoKey, StatefulProtocol>,
    tx_work: mpsc::SyncSender<ThumbRequest>,
    rx_image: mpsc::Receiver<ThumbReady>,
    pending: HashSet<PathBuf>,
    failed: HashSet<PathBuf>,
}

impl ThumbCache {
    pub fn new() -> Self {
        let (tx_work, rx_work) = mpsc::sync_channel::<ThumbRequest>(64);
        let (tx_image, rx_image) = mpsc::channel::<ThumbReady>();

        std::thread::spawn(move || {
            while let Ok(req) = rx_work.recv() {
                let tx = tx_image.clone();
                rayon::spawn(move || {
                    let image = generate_or_load_thumb(&req.path, &req.mime_type)
                        .map_err(|e| tracing::debug!("thumb {:?}: {e}", req.path))
                        .ok();
                    let _ = tx.send(ThumbReady { path: req.path, image });
                });
            }
        });

        Self {
            images: LruCache::new(NonZeroUsize::new(400).unwrap()),
            protocols: LruCache::new(NonZeroUsize::new(200).unwrap()),
            tx_work,
            rx_image,
            pending: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    /// Whether making a thumbnail for this path already failed.
    pub fn failed(&self, path: &Path) -> bool {
        self.failed.contains(path)
    }

    pub fn request(&mut self, path: &Path, mime_type: &str) {
        if self.images.contains(path) || self.pending.contains(path) || self.failed.contains(path) {
            return;
        }
        let req = ThumbRequest { path: path.to_path_buf(), mime_type: mime_type.to_string() };
        // Only mark pending if queued; a full queue is retried next frame
        if self.tx_work.try_send(req).is_ok() {
            self.pending.insert(path.to_path_buf());
        }
    }

    pub fn drain_ready(&mut self) {
        while let Ok(ready) = self.rx_image.try_recv() {
            self.pending.remove(&ready.path);
            match ready.image {
                Some(img) => { self.images.put(ready.path, img); }
                None => { self.failed.insert(ready.path); }
            }
        }
    }

    pub fn get_mut(
        &mut self,
        picker: &Picker,
        path: &Path,
        cols: u16,
        rows: u16,
        bg: Option<(u8, u8, u8)>,
        dim: bool,
    ) -> Option<&mut StatefulProtocol> {
        let key = (path.to_path_buf(), cols, rows, bg, dim);
        if !self.protocols.contains(&key) {
            let img = self.images.get(path)?;
            let mut img = fit_to_cells(img, picker, cols, rows, bg, GRID_FIT);
            if dim {
                crate::icons::dim_image(&mut img, bg);
            }
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

    pub fn clear_protocols(&mut self) {
        self.protocols.clear();
    }
}

/// Freedesktop thumbnail dirs, in lookup order. New thumbnails go in `large`.
const SIZE_DIRS: [&str; 4] = ["large", "x-large", "xx-large", "normal"];

fn thumbnails_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("thumbnails")
}

/// file:// URI as the thumbnail spec (and Dolphin/GLib) hash it: path bytes
/// percent-encoded except unreserved and path-safe characters.
pub(crate) fn file_uri(abs_path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in abs_path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

fn mtime_secs(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
}

/// A cached thumbnail is valid if its Thumb::MTime matches the source, or,
/// lacking that tag, if it is newer than the source.
fn is_thumb_valid(thumb_path: &Path, source_mtime: Option<u64>) -> bool {
    let Ok(file) = std::fs::File::open(thumb_path) else { return false; };
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let Ok(reader) = decoder.read_info() else { return false; };
    let tagged = reader.info().uncompressed_latin1_text.iter()
        .find(|t| t.keyword == "Thumb::MTime")
        .and_then(|t| t.text.parse::<u64>().ok());
    match (tagged, source_mtime) {
        (Some(tag), Some(src)) => tag == src,
        _ => mtime_secs(thumb_path) >= source_mtime,
    }
}

/// Save as PNG with the spec's Thumb::URI / Thumb::MTime tags so other
/// thumbnail consumers (Dolphin, Nautilus) accept and reuse it.
fn save_thumb(img: &image::DynamicImage, out: &Path, uri: &str, mtime: Option<u64>) -> anyhow::Result<()> {
    let rgba = img.to_rgba8();
    let file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut enc = png::Encoder::new(file, rgba.width(), rgba.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.add_text_chunk("Thumb::URI".into(), uri.into())?;
    if let Some(m) = mtime {
        enc.add_text_chunk("Thumb::MTime".into(), m.to_string())?;
    }
    enc.add_text_chunk("Software".into(), "file-browser-tui".into())?;
    enc.write_header()?.write_image_data(rgba.as_raw())?;
    Ok(())
}

/// Enabled preview plugins (Dolphin's names, see `Config::preview_plugins`).
#[derive(Clone)]
pub struct Previews(HashSet<String>);

static GLOBAL: std::sync::OnceLock<Previews> = std::sync::OnceLock::new();

/// The startup preview settings, for background workers (folder previews).
pub fn previews() -> Option<&'static Previews> {
    GLOBAL.get()
}

/// Dolphin's defaults when it has never been configured.
const DEFAULT_PLUGINS: &[&str] = &[
    "imagethumbnail", "jpegthumbnail", "svgthumbnail", "ffmpegthumbs",
    "audiothumbnail", "ebookthumbnail", "opendocumentthumbnail", "comicbookthumbnail",
];

impl Previews {
    pub fn new(config: &crate::config::Config) -> Self {
        let names: Vec<String> = config.preview_plugins.clone()
            .or_else(dolphin_plugins)
            .unwrap_or_else(|| DEFAULT_PLUGINS.iter().map(|s| s.to_string()).collect());
        let p = Self(names.into_iter().collect());
        let _ = GLOBAL.set(p.clone());
        p
    }

    fn on(&self, plugin: &str) -> bool {
        self.0.contains(plugin)
    }

    /// Whether files of this type are shown as a thumbnail of their content.
    pub fn covers(&self, mime: &str) -> bool {
        match mime {
            "image/svg+xml" | "image/svg+xml-compressed" => self.on("svgthumbnail"),
            "image/jpeg" => self.on("jpegthumbnail") || self.on("imagethumbnail"),
            "application/epub+zip" => self.on("ebookthumbnail"),
            "application/pdf" => self.on("gsthumbnail"),
            "application/vnd.comicbook+zip" => self.on("comicbookthumbnail"),
            m if m.starts_with("image/") => self.on("imagethumbnail"),
            m if m.starts_with("video/") => self.on("ffmpegthumbs"),
            m if m.starts_with("audio/") => self.on("audiothumbnail"),
            m if m.starts_with("application/vnd.oasis.opendocument.") => self.on("opendocumentthumbnail"),
            _ => false,
        }
    }
}

/// `[PreviewSettings] Plugins=` from dolphinrc, if Dolphin was configured.
fn dolphin_plugins() -> Option<Vec<String>> {
    let text = std::fs::read_to_string(dirs::config_dir()?.join("dolphinrc")).ok()?;
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[PreviewSettings]";
        } else if in_section && let Some(v) = line.strip_prefix("Plugins=") {
            return Some(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
        }
    }
    None
}

pub(crate) fn generate_or_load_thumb(path: &Path, mime_type: &str) -> anyhow::Result<image::DynamicImage> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let uri = file_uri(&abs);
    let name = format!("{:x}.png", md5::compute(uri.as_bytes()));
    let root = thumbnails_root();
    let src_mtime = mtime_secs(&abs);

    // Reuse any valid thumbnail another app (or we) already made, any size
    for dir in SIZE_DIRS {
        let candidate = root.join(dir).join(&name);
        if is_thumb_valid(&candidate, src_mtime)
            && let Ok(img) = image::open(&candidate)
        {
            return Ok(img);
        }
    }
    // Failed before and unchanged since: don't run the thumbnailer again
    let fail_dir = root.join("fail/file-browser-tui");
    let fail_path = fail_dir.join(&name);
    if src_mtime.is_some() && is_thumb_valid(&fail_path, src_mtime) {
        anyhow::bail!("thumbnailing failed before");
    }

    let large = root.join("large");
    std::fs::create_dir_all(&large)?;
    let thumb_path = large.join(&name);
    // Unique per request so concurrent workers never share a temp file
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = large.join(format!("{name}.{}-{seq}.tmp.png", std::process::id()));

    let generated = if mime_type.starts_with("video/") {
        generator::video_thumb(&abs, &tmp)
    } else if mime_type.starts_with("image/svg") {
        crate::icons::raster::load_icon(&abs, true, 256)
    } else if mime_type.starts_with("image/") {
        generator::image_thumb(&abs)
    } else if mime_type.starts_with("audio/") {
        generator::audio_thumb(&abs, &tmp)
    } else if mime_type == "application/epub+zip" {
        generator::epub_thumb(&abs)
    } else if mime_type == "application/pdf" {
        generator::pdf_thumb(&abs, &tmp)
    } else if mime_type == "application/vnd.comicbook+zip" {
        generator::comic_thumb(&abs)
    } else if mime_type.starts_with("application/vnd.oasis.opendocument.") {
        generator::opendocument_thumb(&abs)
    } else {
        anyhow::bail!("no thumbnailer for {mime_type}");
    };
    let img = match generated {
        Ok(img) => img,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // Remember it (as the spec's fail marker) unless the tool itself
            // is missing, so installing it later still works
            let tool_missing = e.chain().any(|c| c.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound));
            if !tool_missing && std::fs::create_dir_all(&fail_dir).is_ok() {
                let marker = fail_dir.join(format!("{name}.{}-{seq}.tmp.png", std::process::id()));
                let blank = image::DynamicImage::new_rgba8(1, 1);
                if save_thumb(&blank, &marker, &uri, src_mtime).is_ok() {
                    let _ = std::fs::rename(&marker, &fail_path);
                }
            }
            return Err(e);
        }
    };

    save_thumb(&img, &tmp, &uri, src_mtime)?;
    std::fs::rename(&tmp, &thumb_path)?;
    Ok(img)
}


