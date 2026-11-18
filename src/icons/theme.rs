//! Freedesktop icon theme lookup: the user's theme (from kdeglobals), its
//! Inherits chain, then hicolor. Only the index.theme files are read up
//! front; each icon name is looked up on demand (a few stats) and cached,
//! since listing every icon in large themes takes half a second.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// One icon directory of a theme.
struct IconDir {
    path: PathBuf,
    /// index.theme's Context (Places, Apps, ...), lowercased.
    context: String,
}

/// Each theme's icon directories (existing ones, all base dirs), best
/// first: big rasters and vectors win. Themes in inheritance order.
fn chain() -> &'static Vec<Vec<IconDir>> {
    static CHAIN: OnceLock<Vec<Vec<IconDir>>> = OnceLock::new();
    CHAIN.get_or_init(build_chain)
}

/// An icon name the theme (or a theme it inherits) has.
pub struct Listed {
    pub name: String,
    /// The file it resolves to, as `lookup` would find it.
    pub path: PathBuf,
    pub context: String,
}

/// Every icon name in the theme chain, each with the file it resolves to.
/// Lists every directory, which takes a while on big themes: run it off
/// the UI thread.
pub fn list_all() -> Vec<Listed> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dirs in chain() {
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(&dir.path) else { continue; };
            for entry in rd.flatten() {
                let file = entry.file_name();
                let Some((name, ext)) = file.to_str().and_then(|f| f.rsplit_once('.')) else { continue; };
                if !matches!(ext, "png" | "svg" | "svgz" | "xpm") || !seen.insert(name.to_string()) {
                    continue;
                }
                out.push(Listed { name: name.to_string(), path: entry.path(), context: dir.context.clone() });
            }
        }
    }
    out
}

/// Read the theme files in the background so the first lookup doesn't wait.
pub fn prewarm() {
    std::thread::spawn(|| { chain(); });
}

/// Icon file for a theme icon name (or a path, absolute or from ~/), as
/// (path, is_svg).
pub fn lookup(name: &str) -> Option<(PathBuf, bool)> {
    static CACHE: Mutex<Option<HashMap<String, Option<PathBuf>>>> = Mutex::new(None);
    let path = if name.starts_with('/') || name.starts_with("~/") {
        let p = match name.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => PathBuf::from(name),
        };
        p.exists().then_some(p)?
    } else {
        if let Some(hit) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(name) {
            return hit.clone().map(with_kind);
        }
        let found = find(name);
        CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(name.to_string(), found.clone());
        found?
    };
    Some(with_kind(path))
}

fn with_kind(path: PathBuf) -> (PathBuf, bool) {
    let is_svg = path.extension().is_some_and(|e| e == "svg" || e == "svgz");
    (path, is_svg)
}

fn find(name: &str) -> Option<PathBuf> {
    for dirs in chain() {
        for dir in dirs {
            for ext in ["png", "svg", "svgz", "xpm"] {
                let p = dir.path.join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn base_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = dirs::data_dir() { out.push(d.join("icons")); }
    if let Some(h) = dirs::home_dir() { out.push(h.join(".icons")); }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    out.extend(system.split(':').filter(|s| !s.is_empty()).map(|d| Path::new(d).join("icons")));
    out
}

/// `icon_theme` from the config: used instead of the desktop's theme.
static OVERRIDE: OnceLock<String> = OnceLock::new();

/// Use this icon theme rather than the desktop's (empty: the desktop's).
/// Must be called before the first lookup.
pub fn set_theme(name: &str) {
    if !name.trim().is_empty() {
        let _ = OVERRIDE.set(name.trim().to_string());
    }
}

/// The theme icons come from (see `user_theme`).
pub fn theme_name() -> String {
    user_theme()
}

/// The icon theme to use: the config's, then the desktop's (KDE's
/// kdeglobals, GTK's settings.ini, GNOME's gsettings), then the first of
/// a few common themes that's installed, else hicolor.
fn user_theme() -> String {
    if let Some(name) = OVERRIDE.get() {
        return name.clone();
    }
    let config = dirs::config_dir().unwrap_or_default();
    let ini_value = |file: &str, section: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(config.join(file)).ok()?;
        let mut in_section = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_section = line == section;
            } else if in_section
                && let Some((k, v)) = line.split_once('=')
                && k.trim() == key
            {
                return Some(v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty());
            }
        }
        None
    };
    let gsettings = || -> Option<String> {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .stderr(std::process::Stdio::null())
            .output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string();
        (out.status.success() && !v.is_empty()).then_some(v)
    };
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("KDE"));
    let gtk = || ini_value("gtk-4.0/settings.ini", "[Settings]", "gtk-icon-theme-name")
        .or_else(|| ini_value("gtk-3.0/settings.ini", "[Settings]", "gtk-icon-theme-name"));
    let kdeglobals = || ini_value("kdeglobals", "[Icons]", "Theme");
    let found = if kde { kdeglobals().or_else(gtk) } else { gtk().or_else(kdeglobals) }
        .or_else(gsettings);
    let installed = |name: &str| base_dirs().iter().any(|d| d.join(name).join("index.theme").exists());
    found.filter(|t| installed(t))
        .or_else(|| ["breeze", "Papirus", "Adwaita"].into_iter().find(|t| installed(t)).map(str::to_string))
        .unwrap_or_else(|| "hicolor".into())
}

/// index.theme sections: directory -> (Size, Scale), plus Inherits.
/// A theme's icon directories with their preference score and context,
/// and its parents.
type ThemeIndex = (Vec<(String, u32, String)>, Vec<String>);

fn read_index(theme_dir: &Path) -> Option<ThemeIndex> {
    let text = std::fs::read_to_string(theme_dir.join("index.theme")).ok()?;
    let mut inherits = Vec::new();
    let mut dirs: Vec<(String, u32, String)> = Vec::new();
    let mut section = String::new();
    let (mut size, mut scale, mut scalable, mut context) = (0u32, 1u32, false, String::new());
    let flush = |section: &str, size: u32, scale: u32, scalable: bool, context: &str, dirs: &mut Vec<(String, u32, String)>| {
        if !section.is_empty() && section != "Icon Theme" {
            // Prefer the biggest raster, but a vector over anything smaller
            // than 256px (a theme's 256px PNGs are its hand-tuned art)
            let score = if scalable { 255 } else { size * scale };
            dirs.push((section.to_string(), score, context.to_lowercase()));
        }
    };
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&section, size, scale, scalable, &context, &mut dirs);
            section = name.to_string();
            (size, scale, scalable) = (0, 1, false);
            context.clear();
        } else if let Some((k, v)) = line.split_once('=') {
            match (section.as_str(), k.trim()) {
                ("Icon Theme", "Inherits") => inherits = v.split(',').map(|s| s.trim().to_string()).collect(),
                (_, "Size") => size = v.trim().parse().unwrap_or(0),
                (_, "Scale") => scale = v.trim().parse().unwrap_or(1),
                (_, "Type") => scalable = v.trim() == "Scalable",
                (_, "Context") => context = v.trim().to_string(),
                _ => {}
            }
        }
    }
    flush(&section, size, scale, scalable, &context, &mut dirs);
    Some((dirs, inherits))
}

fn build_chain() -> Vec<Vec<IconDir>> {
    let bases = base_dirs();
    let mut chain = Vec::new();
    let mut queue = vec![user_theme()];
    let mut seen = HashSet::new();

    while !queue.is_empty() || !seen.contains("hicolor") {
        let theme = if queue.is_empty() { "hicolor".to_string() } else { queue.remove(0) };
        if !seen.insert(theme.clone()) {
            continue;
        }
        let mut inherits = Vec::new();
        let mut dirs: Vec<(IconDir, u32)> = Vec::new();
        for base in &bases {
            let theme_dir = base.join(&theme);
            let Some((sub, inh)) = read_index(&theme_dir) else { continue; };
            if inherits.is_empty() { inherits = inh; }
            dirs.extend(sub.into_iter()
                .map(|(d, score, context)| (IconDir { path: theme_dir.join(d), context }, score))
                .filter(|(d, _)| d.path.is_dir()));
        }
        // Stable: equal scores keep base-dir order (user dirs first)
        dirs.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        chain.push(dirs.into_iter().map(|(d, _)| d).collect());
        queue.extend(inherits.into_iter().filter(|t| !seen.contains(t)));
    }
    chain
}

