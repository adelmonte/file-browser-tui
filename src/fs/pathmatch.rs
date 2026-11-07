//! Loose matching for typed paths: "/home/me/.cac" finds .cache,
//! "~/dcuments" finds Documents.

use std::path::{Component, Path, PathBuf};

/// How well a folder name matches what was typed: lower is better, None
/// for no match. Exact (ignoring case), then prefix, then contained, then
/// the letters in order, then a typo or two.
fn score(name: &str, typed: &str) -> Option<(u8, usize)> {
    let (n, t) = (name.to_lowercase(), typed.to_lowercase());
    // Hidden folders only when asked for with a leading dot
    let hidden_penalty = u8::from(n.starts_with('.') && !t.starts_with('.'));
    let rank = if n == t {
        0
    } else if n.starts_with(&t) {
        1
    } else if n.contains(&t) {
        2
    } else if in_order(&n, &t) {
        3
    } else if t.chars().count() >= 3 && distance(&n, &t) <= (t.chars().count() / 4).max(1) {
        4
    } else {
        return None;
    };
    Some((rank * 2 + hidden_penalty, name.len()))
}

fn in_order(name: &str, typed: &str) -> bool {
    let mut chars = name.chars();
    typed.chars().all(|c| chars.any(|n| n == c))
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == *cb { prev } else { 1 + prev.min(cur).min(row[j]) };
            prev = cur;
        }
    }
    row[b.len()]
}

/// Folders in `dir` matching `typed`, best first, at most `max`.
pub fn candidates(dir: &Path, typed: &str, max: usize) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new(); };
    let mut found: Vec<((u8, usize), String, PathBuf)> = rd.flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let s = if typed.is_empty() {
                (u8::from(name.starts_with('.')), 0)
            } else {
                score(&name, typed)?
            };
            Some((s, name.to_lowercase(), e.path()))
        })
        .collect();
    found.sort();
    found.into_iter().take(max).map(|(_, _, p)| p).collect()
}

/// The folder `typed` most likely means: `~` expanded, each part that
/// doesn't exist replaced by its best match. None if some part matches
/// nothing.
/// A typed path as it stands: `~` is home, a relative one is under `base`,
/// `..` and `.` taken out.
pub fn expand(typed: &Path, base: &Path) -> PathBuf {
    let full = match typed.strip_prefix("~") {
        Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        Err(_) => base.join(typed),
    };
    let mut out = PathBuf::new();
    for part in full.components() {
        match part {
            Component::ParentDir => { out.pop(); }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn resolve(typed: &Path) -> Option<PathBuf> {
    let typed = match typed.strip_prefix("~") {
        Ok(rest) => dirs::home_dir()?.join(rest),
        Err(_) => typed.to_path_buf(),
    };
    if !typed.is_absolute() {
        return None;
    }
    let mut at = PathBuf::from("/");
    for part in typed.components() {
        match part {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => { at.pop(); }
            Component::Normal(name) => {
                let next = at.join(name);
                at = if next.is_dir() {
                    next
                } else {
                    candidates(&at, &name.to_string_lossy(), 1).into_iter().next()?
                };
            }
        }
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_names_match() {
        let root = std::env::temp_dir().join(format!("fbt-pathmatch-{}", std::process::id()));
        for d in [".cache", ".config", "Documents", "Downloads", "music", "cache-old"] {
            std::fs::create_dir_all(root.join("home").join(d)).unwrap();
        }
        let home = root.join("home");
        let r = |s: &str| resolve(&root.join(s));
        assert_eq!(r("home/.cac"), Some(home.join(".cache")));
        assert_eq!(r("home/.CONF"), Some(home.join(".config")));
        assert_eq!(r("home/doc"), Some(home.join("Documents")));
        assert_eq!(r("home/dcuments"), Some(home.join("Documents")));
        assert_eq!(r("home/dwnlds"), Some(home.join("Downloads")));
        // On a case-insensitive filesystem (macOS default) "Music" already exists as typed
        if std::fs::metadata(home.join("MUSIC")).is_err() {
            assert_eq!(r("home/Music"), Some(home.join("music")));
        }
        // Not hidden unless the dot is typed
        assert_eq!(r("home/cache"), Some(home.join("cache-old")));
        assert_eq!(r("hme/.cac"), Some(home.join(".cache")));
        assert_eq!(r("home/zzzz"), None);
        assert_eq!(r("home/Documents/.."), Some(home.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
