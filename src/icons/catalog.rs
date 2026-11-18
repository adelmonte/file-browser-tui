//! The theme's icons, as a folder icon chooser offers them: one name per
//! distinct picture (themes ship many aliases of each), color variants
//! folded into one (Papirus has every folder in 25 colors), and states
//! (open, drag-accept, symbolic) left out unless asked for. Searching
//! matches words of the names, with stems and synonyms, and an empty
//! search suggests icons for the folder's own name.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::OnceLock,
};

/// Folder colors themes ship variants in (Papirus's, plus a few others').
const COLORS: &[&str] = &[
    "adwaita", "black", "blue", "bluegrey", "breeze", "brown", "carmine", "custom", "cyan",
    "darkcyan", "deeporange", "green", "grey", "indigo", "magenta", "nordic", "orange",
    "palebrown", "paleorange", "pink", "purple", "red", "teal", "violet", "white", "yaru", "yellow",
];

/// Words naming a state of an icon rather than what it shows.
const STATES: &[&str] = &["symbolic", "open", "drag", "accept", "visiting", "loading"];

/// Words that say nothing about what a folder holds.
const GENERIC: &[&str] = &["folder", "folders", "default", "user", "inode", "directory", "x"];

/// Words meaning the same thing for a folder, so "code" finds
/// folder-development and a folder named Photos suggests folder-pictures.
const SYNONYMS: &[&[&str]] = &[
    &["development", "code", "coding", "src", "source", "sources", "project", "projects", "programming", "repo", "repos", "repository", "workspace", "git", "github", "gitlab", "build"],
    &["picture", "pictures", "image", "images", "photo", "photos", "camera", "wallpaper", "wallpapers", "screenshot", "screenshots", "art", "drawings"],
    &["video", "videos", "movie", "movies", "film", "films", "recording", "recordings", "videocamera", "tv", "shows"],
    &["music", "audio", "sound", "sounds", "song", "songs", "podcast", "podcasts", "library"],
    &["document", "documents", "doc", "docs", "text", "txt", "paper", "papers", "wordprocessing", "office"],
    &["book", "books", "ebook", "ebooks", "reading", "stack", "comics", "library"],
    &["download", "downloads", "incoming"],
    &["game", "games", "gaming", "steam", "roms"],
    &["template", "templates", "creative", "design", "graphics"],
    &["public", "publicshare", "share", "shared", "sharing"],
    &["backup", "backups", "sync", "syncthing", "archive"],
    &["cloud", "nextcloud", "owncloud", "dropbox", "onedrive", "gdrive", "google", "drive", "mega", "pcloud", "yandex", "insync"],
    &["mail", "email", "inbox", "letters"],
    &["private", "locked", "encrypted", "secret", "secrets", "vault", "keys", "decrypted", "unlocked"],
    &["home"],
    &["desktop"],
    &["network", "remote", "server", "servers", "ssh", "ftp", "smb", "samba", "nfs"],
    &["script", "scripts", "bin", "tools", "utils", "terminal"],
    &["application", "applications", "apps", "programs", "software"],
    &["archive", "archives", "tar", "zip", "compressed", "packages"],
    &["people", "contacts", "personal", "family", "friends", "identity"],
    &["important", "favorite", "favorites", "favourite", "favourites", "starred", "bookmark", "bookmarks"],
    &["recent", "temp", "tmp", "cache"],
    &["vm", "vms", "virtualbox", "vbox", "vmware", "virtual", "wine"],
    &["android", "phone", "mobile"],
    &["config", "configs", "dotfiles", "settings", "system", "systemd", "root", "etc"],
    &["notes", "notebook", "obsidian", "journal", "diary", "wiki"],
    &["torrent", "torrents"],
    &["print", "printer", "printing", "scans", "scanned"],
    &["work", "office", "business", "job"],
    &["school", "university", "college", "study", "class", "courses"],
];

pub struct Icon {
    /// The name shown and saved: the plainest of the picture's aliases.
    pub name: String,
    /// Its names (the theme's aliases too, all searched) and their words.
    words: Vec<(String, Vec<String>)>,
    places: bool,
    /// The variant's color, if it's one of a set of colored copies.
    color: Option<&'static str>,
    /// A copy in a color other than the one shown by default.
    variant: bool,
    /// Shows a state (open, symbolic, ...) rather than a kind of folder.
    state: bool,
}

pub struct Catalog {
    icons: Vec<Icon>,
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// The catalog once built; `None` while it's being built (`prewarm`
/// starts that).
pub fn get() -> Option<&'static Catalog> {
    CATALOG.get()
}

/// Build the catalog in the background, if it isn't yet.
pub fn prewarm() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_ok() {
        std::thread::spawn(|| {
            let started = std::time::Instant::now();
            let catalog = Catalog::build(super::theme::list_all(), &super::theme_name());
            tracing::debug!("icon catalog: {} icons in {:?}", catalog.icons.len(), started.elapsed());
            let _ = CATALOG.set(catalog);
        });
    }
}

fn tokens(name: &str) -> Vec<String> {
    name.to_lowercase().split(['-', '_', '.', ' ']).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// "downloads" and "download" alike.
fn stem(word: &str) -> &str {
    if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        &word[..word.len() - 1]
    } else {
        word
    }
}

/// A searched word with the stems of its synonyms.
struct Word {
    text: String,
    synonyms: Vec<&'static str>,
}

impl Word {
    fn new(text: String) -> Self {
        let synonyms = SYNONYMS.iter()
            .filter(|g| g.iter().any(|w| stem(w) == stem(&text)))
            .flat_map(|g| g.iter().map(|w| stem(w)))
            .collect();
        Self { text, synonyms }
    }
}

/// How well a word matches a name's words: exact (or the same stem) best,
/// then a synonym, the start of a word, inside a word, inside the name.
fn word_score(word: &Word, name_tokens: &[String], name: &str) -> i32 {
    let q = word.text.as_str();
    let qs = stem(q);
    let mut best = 0;
    for t in name_tokens {
        let s = if t == q || stem(t) == qs {
            10
        } else if word.synonyms.contains(&stem(t)) {
            7
        } else if q.len() >= 2 && t.starts_with(q) {
            6
        } else if q.len() >= 3 && t.contains(q) {
            3
        } else {
            0
        };
        best = best.max(s);
    }
    if best == 0 && q.len() >= 3 && name.contains(q) {
        best = 2;
    }
    best
}

/// Lower is a plainer name for a picture with several: folder-music over
/// gnome-fs-music, stock_music-library or folder_music.
fn name_rank(name: &str) -> usize {
    const LEGACY: &[&str] = &[
        "gnome-", "gtk-", "stock_", "kfm", "gohome", "go-home", "default-", "folder_", "folders-",
        "other-", "cs-", "akonadi", "knetattach", "inode-", "gtg-", "neat", "library-", "system-",
        "application-", "repository", "network_", "trashcan", "gnome", "insync",
    ];
    let mut rank = name.len();
    if LEGACY.iter().any(|p| name.starts_with(p)) {
        rank += 40;
    }
    if name.contains('_') {
        rank += 20;
    }
    if name == "folder" || name.starts_with("folder-") {
        rank = rank.saturating_sub(4);
    } else if !name.starts_with("user-") {
        rank += 10;
    }
    rank
}

impl Catalog {
    /// From every icon the theme has: aliases of one picture merged, color
    /// copies folded, preferring the theme's own color if its name has one
    /// (Tela-nord: nordic).
    pub fn build(listed: Vec<super::theme::Listed>, theme: &str) -> Self {
        // Aliases are symlinks, or (in some themes) copies of the same file
        let mut groups: HashMap<(PathBuf, u64), Vec<(String, bool)>> = HashMap::new();
        let mut order = Vec::new();
        for item in listed {
            let places = item.context == "places";
            let real = std::fs::canonicalize(&item.path).unwrap_or(item.path);
            let key = if places {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::fs::read(&real).unwrap_or_default().hash(&mut h);
                (PathBuf::new(), h.finish())
            } else {
                (real, 0)
            };
            let group = groups.entry(key.clone()).or_default();
            if group.is_empty() {
                order.push(key);
            }
            group.push((item.name, places));
        }

        let theme_tokens = tokens(theme);
        let preferred = COLORS.iter().copied()
            .find(|c| theme_tokens.iter().any(|t| t == c || (*c == "nordic" && t == "nord")))
            .unwrap_or("blue");

        let mut icons: Vec<Icon> = order.into_iter().filter_map(|key| {
            let mut names = groups.remove(&key)?;
            names.sort_by_key(|(n, _)| (name_rank(n), n.clone()));
            let places = names.iter().any(|(_, p)| *p);
            let mut names = names.into_iter().map(|(n, _)| n);
            let name = names.next()?;
            let toks = tokens(&name);
            let color = toks.iter().skip(1).find_map(|t| COLORS.iter().copied().find(|c| c == t));
            let state = toks.iter().any(|t| STATES.contains(&t.as_str()));
            let words = std::iter::once(name.clone()).chain(names).map(|n| { let t = tokens(&n); (n, t) }).collect();
            Some(Icon { name, words, places, color, variant: false, state })
        }).collect();

        // A colored copy shows only if no plain one exists, and then in one
        // color: the theme's, else blue, else whichever there is
        let kind = |icon: &Icon| -> String {
            tokens(&icon.name).into_iter().filter(|t| Some(t.as_str()) != icon.color).collect::<Vec<_>>().join("-")
        };
        let plain: HashSet<String> = icons.iter().filter(|i| i.color.is_none())
            .flat_map(|i| i.words.iter().map(|(n, _)| n.clone()))
            .collect();
        let mut colors_of: HashMap<String, Vec<&'static str>> = HashMap::new();
        for icon in icons.iter().filter(|i| i.color.is_some()) {
            colors_of.entry(kind(icon)).or_default().extend(icon.color);
        }
        for icon in icons.iter_mut() {
            let Some(color) = icon.color else { continue; };
            let k = kind(icon);
            icon.variant = plain.contains(&k) || {
                let colors = &colors_of[&k];
                let shown = if colors.contains(&preferred) { preferred } else { colors.iter().min().copied().unwrap_or(color) };
                color != shown
            };
        }
        icons.sort_by(|a, b| a.name.cmp(&b.name));
        Self { icons }
    }

    /// Icon names to offer for `query`, best first. Empty query: folder
    /// icons, those that fit `folder_name` first. `all`: icons of every
    /// kind, not only folders and places. The bool is whether nothing
    /// matched among folders, so all were searched instead.
    pub fn search(&self, query: &str, folder_name: &str, all: bool) -> (Vec<&str>, bool) {
        let found = self.search_in(query, folder_name, all);
        if found.is_empty() && !all && !query.trim().is_empty() {
            return (self.search_in(query, folder_name, true), true);
        }
        (found, false)
    }

    fn search_in(&self, query: &str, folder_name: &str, all: bool) -> Vec<&str> {
        let q: Vec<Word> = tokens(query).into_iter().map(Word::new).collect();
        let colors: Vec<&str> = q.iter().filter_map(|w| COLORS.iter().copied().find(|c| *c == w.text)).collect();
        let states: Vec<&str> = q.iter().map(|w| w.text.as_str()).filter(|t| STATES.contains(t)).collect();
        let hint: Vec<Word> = tokens(folder_name).into_iter().filter(|t| !GENERIC.contains(&t.as_str())).map(Word::new).collect();

        // Each picture under the alias that matched best
        let mut scored: Vec<(i32, &str)> = self.icons.iter().filter_map(|icon| {
            if !(all || icon.places) {
                return None;
            }
            match icon.color {
                Some(c) if !colors.is_empty() => {
                    if !colors.contains(&c) { return None; }
                }
                _ if icon.variant => return None,
                _ => {}
            }
            if icon.state && !STATES.iter().any(|s| states.contains(s) && icon.name.contains(s)) {
                return None;
            }
            let alias_score = |(n, t): &(String, Vec<String>)| -> Option<i32> {
                if q.is_empty() {
                    return Some(hint.iter().map(|w| word_score(w, t, n)).max().unwrap_or(0) * 2);
                }
                q.iter().map(|w| Some(word_score(w, t, n)).filter(|s| *s > 0)).sum()
            };
            // Ties keep the plainest name (the first)
            let (mut score, name) = icon.words.iter()
                .filter_map(|w| Some((alias_score(w)?, w.0.as_str())))
                .reduce(|best, x| if x.0 > best.0 { x } else { best })?;
            if icon.places {
                score += 4;
            }
            if name == "folder" || name.starts_with("folder-") {
                score += 2;
            }
            Some((score, name))
        }).collect();
        scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(name_rank(a).cmp(&name_rank(b))).then(a.cmp(b)));
        scored.into_iter().map(|(_, n)| n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(names: &[(&str, &str, &str)]) -> (Catalog, PathBuf) {
        let root = std::env::temp_dir().join(format!("fbt-catalog-{}-{}", std::process::id(), names.len()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let listed = names.iter().map(|(name, art, context)| {
            let path = root.join(format!("{name}.svg"));
            std::fs::write(&path, art).unwrap();
            super::super::theme::Listed { name: name.to_string(), path, context: context.to_string() }
        }).collect();
        (Catalog::build(listed, "Tela-nord-dark"), root)
    }

    #[test]
    fn folds_aliases_colors_and_states() {
        let (c, root) = catalog(&[
            ("folder-music", "m", "places"),
            ("gnome-fs-music", "m", "places"),
            ("folder-blue-music", "bm", "places"),
            ("folder-nordic-obsidian", "no", "places"),
            ("folder-blue-obsidian", "bo", "places"),
            ("folder-open", "o", "places"),
            ("folder-development", "d", "places"),
            ("accessories-calculator", "calc", "applications"),
        ]);
        let (names, _) = c.search("", "", false);
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"folder-music") && names.contains(&"folder-nordic-obsidian"));
        // An alias is found under the name searched for
        assert_eq!(c.search("gnome", "", false).0, vec!["gnome-fs-music"]);
        // Asking for a color or a state shows those
        assert_eq!(c.search("blue", "", false).0, vec!["folder-blue-music", "folder-blue-obsidian"]);
        assert_eq!(c.search("open", "", false).0, vec!["folder-open"]);
        // Not a folder: found only when none are
        assert_eq!(c.search("calc", "", false), (vec!["accessories-calculator"], true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn suggests_for_the_folder_name_and_synonyms() {
        let (c, root) = catalog(&[
            ("folder-music", "m", "places"),
            ("folder-development", "d", "places"),
            ("folder-pictures", "p", "places"),
            ("folder-download", "dl", "places"),
        ]);
        assert_eq!(c.search("", "Projects", false).0[0], "folder-development");
        assert_eq!(c.search("", "Photos 2024", false).0[0], "folder-pictures");
        assert_eq!(c.search("", "Downloads", false).0[0], "folder-download");
        assert_eq!(c.search("code", "", false).0, vec!["folder-development"]);
        assert_eq!(c.search("pict", "", false).0, vec!["folder-pictures"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
