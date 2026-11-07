//! Picking files for another program: the file chooser portal
//! (file-browser-portal) starts the app with `--pick REQUEST`, a TOML file
//! saying what to pick, and reads the answer from the file it names.

use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use crate::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Open,
    /// One file, named in the name field.
    Save,
    /// A folder to save the request's `files` in.
    SaveFiles,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Filter {
    pub name: String,
    pub globs: Vec<String>,
    pub mimes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Request {
    pub mode: Mode,
    pub title: String,
    pub accept_label: String,
    pub multiple: bool,
    pub directory: bool,
    pub filters: Vec<Filter>,
    pub current_filter: Option<usize>,
    pub current_name: String,
    pub current_folder: Option<PathBuf>,
    pub current_file: Option<PathBuf>,
    pub files: Vec<String>,
    /// Where the answer goes. No answer written means cancelled.
    pub answer: PathBuf,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Answer {
    pub paths: Vec<PathBuf>,
    pub filter: Option<usize>,
}

pub struct Picking {
    pub req: Request,
    pub filter: Option<usize>,
    pub name: String,
    pub cursor: usize,
    pub name_focused: bool,
    /// Picked paths waiting on "replace the existing file?".
    pub confirm_replace: Option<Vec<PathBuf>>,
    pub done: bool,
    pub name_rect: Rect,
    pub filter_rect: Rect,
    pub accept_rect: Rect,
    pub cancel_rect: Rect,
}

impl Picking {
    pub fn new(req: Request) -> Self {
        let filter = req.current_filter.filter(|&i| i < req.filters.len())
            .or(if req.filters.is_empty() { None } else { Some(0) });
        let name = req.current_name.clone();
        // The cursor before the extension, ready to rename the file
        let cursor = match name.rfind('.') {
            Some(i) if i > 0 => i,
            _ => name.len(),
        };
        let name_focused = req.mode == Mode::Save;
        Picking {
            req, filter, name, cursor, name_focused,
            confirm_replace: None, done: false,
            name_rect: Rect::default(), filter_rect: Rect::default(),
            accept_rect: Rect::default(), cancel_rect: Rect::default(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let req: Request = toml::from_str(&std::fs::read_to_string(path)?)?;
        Ok(Self::new(req))
    }

    /// Only folders are picked (Open with `directory`, or SaveFiles).
    pub fn folders_only(&self) -> bool {
        self.req.directory || self.req.mode == Mode::SaveFiles
    }

    pub fn has_name_field(&self) -> bool {
        self.req.mode == Mode::Save
    }

    pub fn accept_label(&self) -> String {
        let label = self.req.accept_label.replace('_', "");
        if !label.is_empty() {
            return label;
        }
        match (self.req.mode, self.req.directory) {
            (Mode::Open, false) => "Open".into(),
            (Mode::Open, true) => "Select Folder".into(),
            _ => "Save".into(),
        }
    }

    pub fn filter_label(&self) -> String {
        match self.filter.and_then(|i| self.req.filters.get(i)) {
            Some(f) if !f.name.is_empty() => f.name.clone(),
            Some(f) => f.globs.join(", "),
            None => "All Files".into(),
        }
    }

    /// Whether the listing shows this entry.
    pub fn shows(&self, path: &Path, is_dir: bool) -> bool {
        if is_dir {
            return true;
        }
        if self.folders_only() {
            return false;
        }
        let Some(f) = self.filter.and_then(|i| self.req.filters.get(i)) else { return true; };
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if f.globs.iter().any(|g| glob(&g.to_lowercase(), &name)) {
            return true;
        }
        if f.mimes.is_empty() {
            return false;
        }
        let Some(mime) = crate::mime::detect(path) else { return false; };
        f.mimes.iter().any(|m| match m.strip_suffix("/*") {
            Some(kind) => mime.split('/').next() == Some(kind),
            None => *m == mime,
        })
    }
}

/// `*` and `?` wildcards, the patterns the portal hands over.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn last_pick_path() -> PathBuf {
    dirs::state_dir().or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("file-browser-tui/last_pick_folder")
}

/// Where the picker opens: the folder asked for, else the one last picked
/// in, else home.
pub fn start_folder(p: &Picking) -> PathBuf {
    if let Some(f) = p.req.current_file.as_ref().and_then(|f| f.parent()).filter(|d| d.is_dir()) {
        return f.to_path_buf();
    }
    if let Some(d) = p.req.current_folder.as_ref().filter(|d| d.is_dir()) {
        return d.clone();
    }
    std::fs::read_to_string(last_pick_path()).ok()
        .map(|s| PathBuf::from(s.trim_end_matches('\n')))
        .filter(|d| d.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn save_last_folder(dir: &Path) {
    let path = last_pick_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{}\n", dir.display()));
}

/// The selected entries, or the one under the cursor.
fn chosen(app: &App) -> Vec<(PathBuf, bool)> {
    let mut idx: Vec<usize> = if app.multi_selection.is_empty() {
        app.selection.into_iter().collect()
    } else {
        app.multi_selection.iter().copied().collect()
    };
    idx.sort_unstable();
    idx.iter().filter_map(|&i| app.listing.entries.get(i)).map(|e| (e.path.clone(), e.is_dir)).collect()
}

/// The accept button (or Enter): pick what's selected or named.
pub fn accept(app: &mut App) {
    let Some(p) = &app.pick else { return; };
    let cwd = app.cwd.clone();
    let paths = match (p.req.mode, p.folders_only()) {
        (Mode::Open, false) => {
            let chosen = chosen(app);
            // One folder chosen: go in, as a double click would
            if let [(dir, true)] = chosen.as_slice() {
                app.navigate_to(dir.clone());
                return;
            }
            let mut files: Vec<PathBuf> = chosen.into_iter().filter(|(_, d)| !d).map(|(p, _)| p).collect();
            if files.is_empty() {
                app.notify("Select a file first".into());
                return;
            }
            if !p.req.multiple {
                files.truncate(1);
            }
            files
        }
        (Mode::Open, true) => {
            let dirs: Vec<PathBuf> = chosen(app).into_iter().filter(|(_, d)| *d).map(|(p, _)| p).collect();
            match dirs.len() {
                0 => vec![cwd],
                _ if p.req.multiple => dirs,
                _ => dirs.into_iter().take(1).collect(),
            }
        }
        (Mode::Save, _) => {
            let name = p.name.trim();
            if name.is_empty() {
                app.notify("Type a name for the file".into());
                return;
            }
            let path = crate::fs::pathmatch::expand(Path::new(name), &cwd);
            if path.is_dir() {
                // A folder typed into the name field: go there
                app.navigate_to(path);
                if let Some(p) = &mut app.pick {
                    p.name.clear();
                    p.cursor = 0;
                }
                return;
            }
            if !path.parent().is_some_and(|d| d.is_dir()) {
                app.notify(format!("No folder {}", path.parent().unwrap_or(&path).display()));
                return;
            }
            vec![path]
        }
        (Mode::SaveFiles, _) => {
            let dir = chosen(app).into_iter().find(|(_, d)| *d).map(|(p, _)| p).unwrap_or(cwd);
            p.req.files.iter().map(|f| dir.join(f)).collect()
        }
    };
    let saving = p.req.mode != Mode::Open;
    if saving && paths.iter().any(|p| p.exists()) {
        if let Some(p) = &mut app.pick {
            p.confirm_replace = Some(paths);
        }
        return;
    }
    finish(app, paths);
}

/// Write the answer and close.
pub fn finish(app: &mut App, paths: Vec<PathBuf>) {
    let Some(p) = &mut app.pick else { return; };
    let answer = Answer { paths, filter: p.filter };
    match toml::to_string(&answer) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&p.req.answer, s) {
                tracing::error!("pick: writing {}: {e}", p.req.answer.display());
            }
        }
        Err(e) => tracing::error!("pick: {e}"),
    }
    p.done = true;
    save_last_folder(&app.cwd);
}

pub fn cancel(app: &mut App) {
    if let Some(p) = &mut app.pick {
        p.done = true;
    }
}

pub fn set_filter(app: &mut App, filter: Option<usize>) {
    if let Some(p) = &mut app.pick {
        p.filter = filter;
        // Saving with a filter's extension: the name takes it
        if p.req.mode == Mode::Save
            && let Some(ext) = filter.and_then(|i| p.req.filters.get(i))
                .and_then(|f| f.globs.first())
                .and_then(|g| g.strip_prefix("*."))
                .filter(|e| !e.contains(['*', '?', '[']))
            && !p.name.is_empty()
        {
            let stem = match p.name.rfind('.') {
                Some(i) if i > 0 => &p.name[..i],
                _ => &p.name,
            };
            p.name = format!("{stem}.{ext}");
            p.cursor = p.cursor.min(p.name.len());
        }
    }
    app.reload_dir();
}

/// Clicking a file while saving puts its name in the field.
pub fn clicked(app: &mut App, idx: usize) {
    let Some(e) = app.listing.entries.get(idx) else { return; };
    if e.is_dir {
        return;
    }
    let name = e.name.clone();
    if let Some(p) = &mut app.pick
        && p.has_name_field()
    {
        p.cursor = name.len();
        p.name = name;
    }
}

pub fn filter_menu(p: &Picking) -> Vec<crate::ui::menu::MenuItem> {
    use crate::ui::menu::{MenuAction, MenuItem};
    let mark = |on: bool| if on { "\u{2713} " } else { "  " };
    let mut items: Vec<MenuItem> = p.req.filters.iter().enumerate().map(|(i, f)| {
        let name = if f.name.is_empty() { f.globs.join(", ") } else { f.name.clone() };
        MenuItem::new(format!("{}{name}", mark(p.filter == Some(i))), MenuAction::PickFilter(Some(i)))
    }).collect();
    // Programs usually list their own "All files"; else offer one
    let has_all = p.req.filters.iter().any(|f| f.globs.iter().any(|g| g == "*"));
    if !has_all {
        items.push(MenuItem::new(format!("{}All Files", mark(p.filter.is_none())), MenuAction::PickFilter(None)));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_and_filters() {
        assert!(glob("*.png", "a.png"));
        assert!(glob("*", "anything"));
        assert!(glob("img?.*", "img1.jpeg"));
        assert!(!glob("*.png", "a.png.txt"));
        let p = Picking::new(Request {
            filters: vec![Filter { name: "Images".into(), globs: vec!["*.PNG".into()], mimes: vec!["image/*".into()] }],
            ..Default::default()
        });
        assert!(p.shows(Path::new("/x/A.png"), false));
        assert!(p.shows(Path::new("/x/b.jpg"), false));
        assert!(!p.shows(Path::new("/x/c.txt"), false));
        assert!(p.shows(Path::new("/x/dir"), true));
    }

    #[test]
    fn request_round_trip() {
        let req = Request { mode: Mode::Save, current_name: "report.pdf".into(), ..Default::default() };
        let back: Request = toml::from_str(&toml::to_string(&req).unwrap()).unwrap();
        let p = Picking::new(back);
        assert_eq!((p.req.mode, p.name.as_str(), p.cursor, p.name_focused), (Mode::Save, "report.pdf", 6, true));
    }
}
