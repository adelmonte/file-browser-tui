//! Choosing a folder's icon, from Properties: a search field over the
//! theme's icons (see icons::catalog) and a grid of them. A path typed
//! instead (/ or ~) lists the image files there.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use ratatui_image::{StatefulImage, picker::ProtocolType};

use crate::app::App;
use crate::icons::catalog;
use crate::ui::{kitty, theme};

const TILE_W: u16 = 12;
/// Icon rows, the name under it, and a gap.
const TILE_ICON_H: u16 = 3;
const TILE_H: u16 = TILE_ICON_H + 2;
const IMAGE_EXTS: &[&str] = &["png", "svg", "svgz", "jpg", "jpeg", "webp", "gif", "bmp", "ico"];

pub struct IconPicker {
    pub dir: PathBuf,
    folder_name: String,
    /// The custom icon set now, if any.
    current: Option<String>,
    /// The icon the folder has without one.
    usual: String,
    pub query: String,
    pub cursor: usize,
    /// Every kind of icon, not only folders.
    all: bool,
    results: Vec<Choice>,
    /// Nothing among folders matched, so these are from all icons.
    fallback: bool,
    /// The query and scope `results` were made for; `None` until the
    /// theme's catalog is ready.
    built_for: Option<(String, bool)>,
    first_build: bool,
    selected: usize,
    /// First grid row shown.
    scroll: usize,
    /// Columns and rows of the grid last drawn.
    grid: (usize, usize),
    hits: RefCell<Vec<(Rect, usize)>>,
    toggle_hit: Option<Rect>,
}

/// What a tile offers.
#[derive(Clone, PartialEq)]
enum Choice {
    /// The folder's usual icon (no custom one).
    Usual,
    /// A theme icon name or an image file.
    Icon(String),
    /// A folder, while typing a path: opens it.
    Folder(PathBuf),
}

pub enum Outcome {
    Stay,
    Close,
    /// Set this icon (`None`: back to the usual one).
    Choose(Option<String>),
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// Folders and image files a typed path names or starts: those in the
/// folder it ends with, or in its parent starting with its last part.
fn path_matches(query: &str) -> Vec<Choice> {
    let path = expand(query);
    if path.is_file() {
        return vec![Choice::Icon(path.to_string_lossy().into_owned())];
    }
    let (dir, prefix) = if query.ends_with('/') || path.is_dir() {
        (path.clone(), String::new())
    } else {
        let prefix = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        (path.parent().map(Path::to_path_buf).unwrap_or_default(), prefix)
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            name.starts_with(&prefix) && !name.starts_with('.') && (p.is_dir() || IMAGE_EXTS.contains(&ext.as_str()) && p.is_file())
        })
        .take(2000)
        .collect();
    // Folders first
    found.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
    found.into_iter().map(|p| if p.is_dir() {
        Choice::Folder(p)
    } else {
        Choice::Icon(p.to_string_lossy().into_owned())
    }).collect()
}

impl IconPicker {
    pub fn new(dir: &Path) -> Self {
        catalog::prewarm();
        let usual = crate::icons::resolver::xdg_icon(dir).unwrap_or("folder").to_string();
        Self {
            dir: dir.to_path_buf(),
            folder_name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            current: crate::icons::resolver::folder_icon(dir),
            usual,
            query: String::new(),
            cursor: 0,
            all: false,
            results: Vec::new(),
            fallback: false,
            built_for: None,
            first_build: true,
            selected: 0,
            scroll: 0,
            grid: (1, 1),
            hits: Default::default(),
            toggle_hit: None,
        }
    }

    fn is_path(&self) -> bool {
        let q = self.query.trim_start();
        q.starts_with('/') || q.starts_with('~')
    }

    /// Bring `results` up to date with the query.
    fn refresh(&mut self) {
        let wanted = (self.query.clone(), self.all);
        if self.built_for.as_ref() == Some(&wanted) {
            return;
        }
        let q = self.query.trim();
        let mut results = Vec::new();
        self.fallback = false;
        if self.is_path() {
            results = path_matches(q);
        } else if let Some(cat) = catalog::get() {
            if q.is_empty() {
                results.push(Choice::Usual);
                results.extend(self.current.clone().map(Choice::Icon));
            }
            let (found, fallback) = cat.search(q, &self.folder_name, self.all);
            self.fallback = fallback;
            results.extend(found.into_iter()
                .filter(|n| !(q.is_empty() && self.current.as_deref() == Some(*n)))
                .map(|n| Choice::Icon(n.to_string())));
        } else {
            // Theme still being read: the usual and current icons meanwhile
            results.push(Choice::Usual);
            results.extend(self.current.clone().map(Choice::Icon));
            self.results = results;
            return;
        }
        self.results = results;
        self.built_for = Some(wanted);
        // At first the current icon is selected; after a search, the best match
        self.selected = match (self.first_build, &self.current) {
            (true, Some(_)) => 1.min(self.results.len().saturating_sub(1)),
            _ => 0,
        };
        self.first_build = false;
        self.scroll = 0;
    }

    /// A tile's name: a theme name without the word folder (default-git
    /// for default-folder-git), a file's name without its extension.
    fn label(choice: &Choice) -> String {
        match choice {
            Choice::Usual => "Default".into(),
            Choice::Folder(p) => p.file_name().map(|s| format!("{}/", s.to_string_lossy())).unwrap_or_default(),
            Choice::Icon(n) if n.starts_with('/') => Path::new(n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            Choice::Icon(n) if n == "folder" => n.clone(),
            Choice::Icon(n) => n.split(['-', '_']).filter(|w| *w != "folder").collect::<Vec<_>>().join("-"),
        }
    }

    fn icon_name<'a>(&'a self, choice: &'a Choice) -> &'a str {
        match choice {
            Choice::Usual => &self.usual,
            Choice::Icon(n) => n,
            Choice::Folder(_) => "folder",
        }
    }

    /// Take a tile: a folder opens, an icon is the answer.
    fn take(&mut self, i: usize) -> Outcome {
        match self.results.get(i).cloned() {
            Some(Choice::Usual) => Outcome::Choose(None),
            Some(Choice::Icon(n)) => Outcome::Choose(Some(n)),
            Some(Choice::Folder(p)) => {
                let mut shown = p.to_string_lossy().into_owned();
                if let Some(home) = dirs::home_dir()
                    && let Ok(rest) = p.strip_prefix(&home) {
                        shown = format!("~/{}", rest.display());
                    }
                self.query = format!("{}/", shown.trim_end_matches('/'));
                self.cursor = self.query.len();
                Outcome::Stay
            }
            None => Outcome::Stay,
        }
    }

    fn step(&mut self, by: isize) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + by).clamp(0, n as isize - 1) as usize;
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> Outcome {
        // Keys move over what the typing so far found
        self.refresh();
        let cols = self.grid.0.max(1) as isize;
        let page = cols * self.grid.1.max(1) as isize;
        match ev.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => return self.take(self.selected),
            KeyCode::Left => self.step(-1),
            KeyCode::Right => self.step(1),
            KeyCode::Up => self.step(-cols),
            KeyCode::Down => self.step(cols),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Tab | KeyCode::BackTab => self.all = !self.all,
            _ => {
                crate::lineedit::edit_undoable("icon-picker", &mut self.query, &mut self.cursor, ev);
            }
        }
        Outcome::Stay
    }

    /// A left click: picks a tile, or sets it if already picked.
    pub fn click(&mut self, col: u16, row: u16) -> Outcome {
        if self.toggle_hit.is_some_and(|r| contains(r, col, row)) {
            self.all = !self.all;
            return Outcome::Stay;
        }
        let hit = self.hits.borrow().iter().find(|(r, _)| contains(*r, col, row)).map(|h| h.1);
        if let Some(i) = hit {
            if i == self.selected {
                return self.take(i);
            }
            self.selected = i;
        }
        Outcome::Stay
    }

    pub fn scroll_rows(&mut self, rows: isize) {
        let total = self.results.len().div_ceil(self.grid.0.max(1));
        let max = total.saturating_sub(self.grid.1) as isize;
        self.scroll = (self.scroll as isize + rows).clamp(0, max.max(0)) as usize;
        // Keep the selection on screen, as the wheel moves past it
        let cols = self.grid.0.max(1);
        let row = self.selected / cols;
        if row < self.scroll {
            self.selected = self.scroll * cols + self.selected % cols;
        } else if row >= self.scroll + self.grid.1 {
            self.selected = (self.scroll + self.grid.1 - 1) * cols + self.selected % cols;
        }
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }
}

fn contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

pub fn rect(term: Rect) -> Rect {
    let (w, h) = (term.width.saturating_sub(4).min(98), term.height.saturating_sub(2).min(36));
    Rect { x: term.width.saturating_sub(w) / 2, y: term.height.saturating_sub(h) / 2, width: w, height: h }
}

fn draw_icon(f: &mut Frame, app: &mut App, name: &str, rect: Rect, bg: (u8, u8, u8)) {
    let key = PathBuf::from(format!("picker-icon/{name}"));
    let px = (app.picker.font_size().height as u32 * TILE_ICON_H as u32).max(32);
    app.picker_icons.request(&key, true, px, Some(name.to_string()));
    let Some(proto) = app.picker_icons.get_mut(&app.picker, &key, rect.width, rect.height, Some(bg), false) else { return; };
    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(Style::default().bg(theme::rgb(bg))), rect);
    f.render_stateful_widget(StatefulImage::new(), rect, proto);
    if app.picker.protocol_type() == ProtocolType::Kitty {
        kitty::split_placeholder_rows(f.buffer_mut(), rect, &mut app.kitty_transmits);
    }
}

pub fn render(f: &mut Frame, app: &mut App, p: &mut IconPicker) {
    p.refresh();
    let area = rect(f.area());
    let panel = theme::get().panel;
    let bg = Style::default().bg(theme::panel()).fg(theme::text());
    let dim = bg.fg(theme::muted());
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::border()))
            .title(format!(" Icon for {} ", p.folder_name))
            .style(bg),
        area,
    );
    let inner = Rect { x: area.x + 2, y: area.y + 1, width: area.width.saturating_sub(4), height: area.height.saturating_sub(2) };
    if inner.height < TILE_H + 3 || inner.width < TILE_W {
        return;
    }

    // Search field, and the Folders / All switch
    let scope = [("Folders", !p.all), ("All icons", p.all)];
    let scope_w = scope.iter().map(|(s, _)| s.len() as u16 + 2).sum::<u16>() + 1;
    let field_w = inner.width.saturating_sub(scope_w + 1) as usize;
    let mut shown = p.query.clone();
    let cursor = p.cursor.min(shown.len());
    shown.insert(cursor, crate::lineedit::bar_cursor());
    let chars: Vec<char> = shown.chars().collect();
    let cur = shown[..cursor].chars().count();
    let start = (cur + 1).saturating_sub(field_w);
    let (visible, field_fg) = if p.query.is_empty() {
        (format!("{}Search, or type a path to an image", crate::lineedit::bar_cursor()), theme::muted())
    } else {
        (chars.iter().skip(start).take(field_w).collect(), theme::text_strong())
    };
    let visible: String = visible.chars().take(field_w).collect();
    let mut spans = vec![
        Span::styled(format!("{visible:<field_w$}"), Style::default().bg(theme::input()).fg(field_fg)),
        Span::styled("  ", bg),
    ];
    for (label, on) in scope {
        let st = if on { bg.fg(theme::text_strong()).bg(theme::selection()).add_modifier(Modifier::BOLD) } else { dim };
        spans.push(Span::styled(format!(" {label} "), st));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { height: 1, ..inner });
    p.toggle_hit = Some(Rect { x: inner.x + field_w as u16 + 2, y: inner.y, width: scope_w, height: 1 });

    // The grid
    let grid = Rect { x: inner.x, y: inner.y + 2, width: inner.width, height: inner.height - 3 };
    let cols = (grid.width / TILE_W).max(1) as usize;
    let rows = (grid.height / TILE_H).max(1) as usize;
    p.grid = (cols, rows);
    let sel_row = p.selected / cols;
    if sel_row < p.scroll {
        p.scroll = sel_row;
    } else if sel_row >= p.scroll + rows {
        p.scroll = sel_row + 1 - rows;
    }
    let left = grid.x + (grid.width - cols as u16 * TILE_W) / 2;
    let font = app.picker.font_size();
    let icon_w = ((TILE_ICON_H as f32 * font.height as f32 / font.width.max(1) as f32).ceil() as u16).min(TILE_W - 2);
    let mut hits = Vec::new();
    let first = p.scroll * cols;
    for (i, choice) in p.results.iter().enumerate().skip(first).take(cols * rows) {
        let (c, r) = ((i - first) % cols, (i - first) / cols);
        let tile = Rect { x: left + c as u16 * TILE_W, y: grid.y + r as u16 * TILE_H, width: TILE_W - 1, height: TILE_H - 1 };
        let selected = i == p.selected;
        let tile_bg = if selected { theme::get().selection } else { panel };
        if selected {
            f.render_widget(Block::default().style(Style::default().bg(theme::rgb(tile_bg))), tile);
        }
        let icon = Rect { x: tile.x + (tile.width - icon_w) / 2, y: tile.y, width: icon_w, height: TILE_ICON_H };
        let name = p.icon_name(choice).to_string();
        draw_icon(f, app, &name, icon, tile_bg);
        let mut label = IconPicker::label(choice);
        let w = tile.width as usize;
        if label.chars().count() > w {
            label = label.chars().take(w - 1).collect::<String>() + "\u{2026}";
        }
        let mut st = Style::default().bg(theme::rgb(tile_bg)).fg(if selected { theme::text_strong() } else { theme::text() });
        if *choice == Choice::Usual || matches!(choice, Choice::Icon(n) if Some(n) == p.current.as_ref()) {
            st = st.add_modifier(Modifier::BOLD);
        }
        f.render_widget(Paragraph::new(Span::styled(format!("{label:^w$}"), st)), Rect { y: tile.y + TILE_ICON_H, height: 1, ..tile });
        hits.push((tile, i));
    }
    *p.hits.borrow_mut() = hits;

    // What's selected, and how to use it
    let status = if p.built_for.is_none() && !p.is_path() {
        "Reading the icon theme\u{2026}".to_string()
    } else if p.results.is_empty() {
        if p.is_path() { "No folders or images there".into() } else { "No icons match".into() }
    } else {
        let what = match p.results.get(p.selected) {
            Some(Choice::Usual) => format!("Default ({})", p.usual),
            Some(Choice::Icon(n)) if Some(n) == p.current.as_ref() => format!("{n} (current)"),
            Some(Choice::Icon(n)) => n.clone(),
            Some(Choice::Folder(d)) => format!("{}/ (Enter opens)", d.display()),
            None => String::new(),
        };
        let note = if p.fallback { " \u{b7} no folder icons match, showing all" } else { "" };
        let icons = p.results.iter().filter(|c| matches!(c, Choice::Icon(_))).count();
        format!("{what}{note} \u{b7} {icons} icons")
    };
    let hint = "Enter sets \u{b7} Tab folders/all \u{b7} Esc back";
    let status_w = (inner.width as usize).saturating_sub(hint.chars().count() + 2);
    let status: String = status.chars().take(status_w).collect();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{status:<status_w$}  "), bg),
            Span::styled(hint, dim),
        ])),
        Rect { y: inner.y + inner.height - 1, height: 1, ..inner },
    );
}
