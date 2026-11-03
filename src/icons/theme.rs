//! Freedesktop icon theme lookup: the user's theme (from kdeglobals), its
//! Inherits chain, then hicolor. Indexed once so lookups are a map hit.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};

struct Found {
    /// Position in the theme chain (lower wins).
    theme: usize,
    /// Preference within a theme: big rasters and vectors win.
    score: u32,
    path: PathBuf,
}

fn index() -> &'static HashMap<String, Found> {
    static INDEX: OnceLock<HashMap<String, Found>> = OnceLock::new();
    INDEX.get_or_init(build_index)
}

/// Build the index in the background so the first lookups don't wait.
pub fn prewarm() {
    std::thread::spawn(|| { index(); });
}

/// Icon file for a theme icon name (or an absolute path), as (path, is_svg).
pub fn lookup(name: &str) -> Option<(PathBuf, bool)> {
    let path = if name.starts_with('/') {
        let p = PathBuf::from(name);
        p.exists().then_some(p)?
    } else {
        index().get(name)?.path.clone()
    };
    let is_svg = path.extension().is_some_and(|e| e == "svg" || e == "svgz");
    Some((path, is_svg))
}

fn base_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = dirs::data_dir() { out.push(d.join("icons")); }
    if let Some(h) = dirs::home_dir() { out.push(h.join(".icons")); }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    out.extend(system.split(':').filter(|s| !s.is_empty()).map(|d| Path::new(d).join("icons")));
    out
}

/// The configured theme: KDE's [Icons] Theme, else hicolor.
fn user_theme() -> String {
    let kdeglobals = dirs::config_dir().map(|d| d.join("kdeglobals")).unwrap_or_default();
    let text = std::fs::read_to_string(kdeglobals).unwrap_or_default();
    let mut in_icons = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_icons = line == "[Icons]";
        } else if in_icons && let Some(v) = line.strip_prefix("Theme=") {
            return v.trim().to_string();
        }
    }
    "hicolor".into()
}

/// index.theme sections: directory -> (Size, Scale), plus Inherits.
/// A theme's icon directories with their preference score, and its parents.
type ThemeIndex = (Vec<(String, u32)>, Vec<String>);

fn read_index(theme_dir: &Path) -> Option<ThemeIndex> {
    let text = std::fs::read_to_string(theme_dir.join("index.theme")).ok()?;
    let mut inherits = Vec::new();
    let mut dirs: Vec<(String, u32)> = Vec::new();
    let mut section = String::new();
    let (mut size, mut scale, mut scalable) = (0u32, 1u32, false);
    let flush = |section: &str, size: u32, scale: u32, scalable: bool, dirs: &mut Vec<(String, u32)>| {
        if !section.is_empty() && section != "Icon Theme" {
            // Prefer the biggest raster, but a vector over anything smaller
            // than 256px (a theme's 256px PNGs are its hand-tuned art)
            let score = if scalable { 255 } else { size * scale };
            dirs.push((section.to_string(), score));
        }
    };
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&section, size, scale, scalable, &mut dirs);
            section = name.to_string();
            (size, scale, scalable) = (0, 1, false);
        } else if let Some((k, v)) = line.split_once('=') {
            match (section.as_str(), k.trim()) {
                ("Icon Theme", "Inherits") => inherits = v.split(',').map(|s| s.trim().to_string()).collect(),
                (_, "Size") => size = v.trim().parse().unwrap_or(0),
                (_, "Scale") => scale = v.trim().parse().unwrap_or(1),
                (_, "Type") => scalable = v.trim() == "Scalable",
                _ => {}
            }
        }
    }
    flush(&section, size, scale, scalable, &mut dirs);
    Some((dirs, inherits))
}

fn build_index() -> HashMap<String, Found> {
    let bases = base_dirs();
    let mut index: HashMap<String, Found> = HashMap::new();
    let mut queue = vec![user_theme()];
    let mut seen = HashSet::new();
    let mut rank = 0;

    while !queue.is_empty() || !seen.contains("hicolor") {
        let theme = if queue.is_empty() { "hicolor".to_string() } else { queue.remove(0) };
        if !seen.insert(theme.clone()) {
            continue;
        }
        let mut inherits = Vec::new();
        for base in &bases {
            let theme_dir = base.join(&theme);
            let Some((dirs, inh)) = read_index(&theme_dir) else { continue; };
            if inherits.is_empty() { inherits = inh; }
            for (dir, score) in dirs {
                let Ok(rd) = std::fs::read_dir(theme_dir.join(&dir)) else { continue; };
                for path in rd.filter_map(|e| e.ok().map(|e| e.path())) {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !matches!(ext, "png" | "svg" | "svgz" | "xpm") { continue; }
                    let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue; };
                    let better = index.get(&name).is_none_or(|f| rank < f.theme || (rank == f.theme && score > f.score));
                    if better {
                        index.insert(name, Found { theme: rank, score, path });
                    }
                }
            }
        }
        queue.extend(inherits.into_iter().filter(|t| !seen.contains(t)));
        rank += 1;
    }
    tracing::info!("icon theme index: {} icons", index.len());
    index
}
