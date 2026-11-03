use ratatui::{
    Frame,
    crossterm::event::KeyCode,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use strum::IntoEnumIterator;

use crate::app::{App, SortField, ViewMode};
use crate::ui::theme;

const MENU_BG: Color = theme::FIELD_BG;
const MENU_SEL: Color = theme::rgb(theme::NORD9);

// ---------------------------------------------------------------------------
// Menu data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    None,
    Open,
    OpenTerminal,
    Rename,
    Cut,
    Copy,
    CopyLocation,
    Paste,
    Trash,
    EmptyTrash,
    NewFolder,
    NewFile,
    CompressTarGz,
    CompressZip,
    Properties,
    AddToPlaces,
    RemovePlace,
    SetView(ViewMode),
    SetSortField(SortField),
    SetAscending(bool),
    ToggleFoldersFirst,
    ToggleShowHidden,
    Custom(usize),
    /// (group, action) index into `App::user_actions`.
    UserAction(usize, usize),
    /// Index into `App::kdeconnect_devices`.
    SendToDevice(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTarget {
    Entry(usize),
    Empty,
    Place(usize),
    Device(usize),
    SidebarEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Context,
    View,
    Sort,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
    pub enabled: bool,
    pub submenu: Vec<MenuItem>,
    pub separator: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: MenuAction) -> Self {
        Self { label: label.into(), action, enabled: true, submenu: Vec::new(), separator: false }
    }

    pub fn sub(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self { label: label.into(), action: MenuAction::None, enabled: true, submenu: items, separator: false }
    }

    pub fn sep() -> Self {
        Self { label: String::new(), action: MenuAction::None, enabled: true, submenu: Vec::new(), separator: true }
    }

    pub fn enabled_if(mut self, cond: bool) -> Self {
        self.enabled = cond;
        self
    }

    fn selectable(&self) -> bool {
        !self.separator && self.enabled
    }
}

#[derive(Debug, Clone)]
pub struct MenuState {
    pub kind: MenuKind,
    pub target: MenuTarget,
    pub items: Vec<MenuItem>,
    pub pos: (u16, u16),
    pub selected: usize,
    pub submenu_open: bool,
    pub submenu_selected: usize,
    /// Actual rendered rects, used for hit-testing and occlusion.
    pub rect: Rect,
    pub submenu_rect: Rect,
}

impl MenuState {
    pub fn new(kind: MenuKind, target: MenuTarget, items: Vec<MenuItem>, pos: (u16, u16)) -> Self {
        let selected = items.iter().position(|i| i.selectable()).unwrap_or(0);
        Self {
            kind,
            target,
            items,
            pos,
            selected,
            submenu_open: false,
            submenu_selected: 0,
            rect: Rect::default(),
            submenu_rect: Rect::default(),
        }
    }
}

pub enum MenuOutcome {
    Consumed,
    Close,
    /// Close the menu and run the action. Bool = click landed outside (close only).
    Action(MenuAction),
    /// Click landed outside the menu.
    Outside,
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Build the context menu items for a right-click target.
pub fn build_context_menu(app: &App, target: MenuTarget) -> Vec<MenuItem> {
    let has_clip = app.clipboard.is_some();
    let mut items = Vec::new();

    match target {
        MenuTarget::Entry(idx) => {
            let Some(entry) = app.listing.entries.get(idx) else { return items; };
            let is_dir = entry.is_dir;

            items.push(MenuItem::new("\u{f07c}  Open", MenuAction::Open));
            if is_dir {
                items.push(MenuItem::new("\u{f120}  Open in Terminal", MenuAction::OpenTerminal));
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f0c4}  Cut", MenuAction::Cut));
            items.push(MenuItem::new("\u{f0c5}  Copy", MenuAction::Copy));
            items.push(MenuItem::new("\u{f0c1}  Copy Location", MenuAction::CopyLocation));
            if is_dir {
                items.push(MenuItem::new("\u{f0ea}  Paste Into Folder", MenuAction::Paste).enabled_if(has_clip));
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f044}  Rename", MenuAction::Rename));
            items.push(MenuItem::new("\u{f1f8}  Move to Trash", MenuAction::Trash));
            items.push(MenuItem::sep());
            items.push(MenuItem::sub("\u{f1c6}  Compress", vec![
                MenuItem::new("tar.gz", MenuAction::CompressTarGz),
                MenuItem::new("zip", MenuAction::CompressZip),
            ]));
            push_custom_items(app, &mut items, if is_dir { "dir" } else { "file" });
            let mime = if is_dir {
                "inode/directory".to_string()
            } else {
                entry.mime_type.clone()
                    .or_else(|| crate::mime::guess_by_name(&entry.path))
                    .unwrap_or_else(|| "application/octet-stream".into())
            };
            push_user_actions(app, &mut items, &mime);
            if !is_dir {
                push_send_to_device(app, &mut items);
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f05a}  Properties", MenuAction::Properties));
        }

        MenuTarget::Empty => {
            items.push(MenuItem::sub("\u{f067}  Create New", vec![
                MenuItem::new("\u{f07b}  Folder", MenuAction::NewFolder),
                MenuItem::new("\u{f15b}  File", MenuAction::NewFile),
            ]));
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f0ea}  Paste", MenuAction::Paste).enabled_if(has_clip));
            items.push(MenuItem::new("\u{f120}  Open Terminal Here", MenuAction::OpenTerminal));
            push_custom_items(app, &mut items, "empty");
            push_user_actions(app, &mut items, "inode/directory");
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f05a}  Properties", MenuAction::Properties));
        }

        MenuTarget::Place(idx) => {
            items.push(MenuItem::new("\u{f07c}  Open", MenuAction::Open));
            let is_trash = app.places.get(idx)
                .map(|p| p.icon.as_deref() == Some("user-trash") || p.path.ends_with("Trash/files"))
                .unwrap_or(false);
            if is_trash {
                items.push(MenuItem::new("\u{f1f8}  Empty Trash", MenuAction::EmptyTrash));
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new("\u{f068}  Remove from Places", MenuAction::RemovePlace));
            items.push(MenuItem::new("\u{f067}  Add Current Dir to Places", MenuAction::AddToPlaces));
        }

        MenuTarget::Device(_) => {
            items.push(MenuItem::new("\u{f07c}  Open", MenuAction::Open));
        }

        MenuTarget::SidebarEmpty => {
            items.push(MenuItem::new("\u{f067}  Add Current Dir to Places", MenuAction::AddToPlaces));
        }
    }

    items
}

fn push_custom_items(app: &App, items: &mut Vec<MenuItem>, target_kind: &str) {
    let custom = &app.config.context_menu.custom;
    let mut added_sep = false;
    for (i, entry) in custom.iter().enumerate() {
        let matches = entry.targets.is_empty()
            || entry.targets.iter().any(|t| t == target_kind);
        if matches {
            if !added_sep {
                items.push(MenuItem::sep());
                added_sep = true;
            }
            items.push(MenuItem::new(format!("\u{f085}  {}", entry.label), MenuAction::Custom(i)));
        }
    }
}

/// Actions from ~/.config/file-browser-tui/actions matching `mime`: top-level
/// ones inline, the rest grouped by submenu name (in file order).
fn push_user_actions(app: &App, items: &mut Vec<MenuItem>, mime: &str) {
    let mut top: Vec<MenuItem> = Vec::new();
    let mut subs: Vec<(String, Vec<MenuItem>)> = Vec::new();
    for (gi, group) in app.user_actions.iter().enumerate() {
        if !group.applies_to(mime) {
            continue;
        }
        let entries = group.actions.iter().enumerate().map(|(ai, a)| {
            MenuItem::new(
                format!("{}  {}", action_glyph(a.icon.as_deref()), a.label),
                MenuAction::UserAction(gi, ai),
            )
        });
        match &group.submenu {
            None => top.extend(entries),
            Some(name) => match subs.iter_mut().find(|(n, _)| n == name) {
                Some((_, v)) => v.extend(entries),
                None => subs.push((name.clone(), entries.collect())),
            },
        }
    }
    if top.is_empty() && subs.is_empty() {
        return;
    }
    items.push(MenuItem::sep());
    items.extend(top);
    for (name, sub) in subs {
        items.push(MenuItem::sub(format!("\u{f0ae}  {name}"), sub));
    }
}

/// "Send to Device" submenu listing reachable KDE Connect devices.
fn push_send_to_device(app: &App, items: &mut Vec<MenuItem>) {
    if app.kdeconnect_devices.is_empty() {
        return;
    }
    let devices = app.kdeconnect_devices.iter().enumerate()
        .map(|(i, d)| MenuItem::new(format!("\u{f10b}  {}", d.name), MenuAction::SendToDevice(i)))
        .collect();
    items.push(MenuItem::sub("\u{f1d8}  Send to Device", devices));
}

/// Nerd-font glyph for a freedesktop icon name used by an action.
fn action_glyph(icon: Option<&str>) -> &'static str {
    let icon = icon.unwrap_or("");
    match icon {
        i if i.contains("rename") => "\u{f044}",
        i if i.contains("terminal") || i.contains("konsole") => "\u{f120}",
        i if i.contains("wallpaper") => "\u{f03e}",
        i if i.starts_with("video") => "\u{f03d}",
        i if i.starts_with("image") => "\u{f03e}",
        i if i.starts_with("audio") => "\u{f001}",
        i if i.contains("file-manager") || i.contains("folder") => "\u{f07c}",
        i if i.contains("archive") || i.contains("compress") => "\u{f1c6}",
        _ => "\u{f085}",
    }
}

/// Topbar "view" dropdown as a menu.
pub fn build_view_menu(app: &App) -> Vec<MenuItem> {
    ViewMode::iter().map(|v| {
        let check = if v == app.view_mode { "\u{f00c} " } else { "  " };
        let icon = view_mode_icon(v);
        MenuItem::new(format!("{check}{icon} {v}"), MenuAction::SetView(v))
    }).collect()
}

pub fn view_mode_icon(v: ViewMode) -> &'static str {
    match v {
        ViewMode::Icons   => "\u{f009}",
        ViewMode::Compact => "\u{f00b}",
        ViewMode::Details => "\u{f0ca}",
    }
}

/// Topbar "sort" dropdown as a menu.
pub fn build_sort_menu(app: &App) -> Vec<MenuItem> {
    let radio = |on: bool| if on { "\u{f111} " } else { "\u{f10c} " };
    let check = |on: bool| if on { "\u{f14a} " } else { "\u{f096} " };

    let mut items: Vec<MenuItem> = SortField::iter().map(|sf| {
        MenuItem::new(format!("{}{sf}", radio(sf == app.sort.field)), MenuAction::SetSortField(sf))
    }).collect();

    items.push(MenuItem::sep());
    items.push(MenuItem::new(format!("{}A\u{2192}Z", radio(app.sort.ascending)), MenuAction::SetAscending(true)));
    items.push(MenuItem::new(format!("{}Z\u{2192}A", radio(!app.sort.ascending)), MenuAction::SetAscending(false)));
    items.push(MenuItem::sep());
    items.push(MenuItem::new(format!("{}Folders first", check(app.folders_first)), MenuAction::ToggleFoldersFirst));
    items.push(MenuItem::new(format!("{}Show hidden", check(app.show_hidden)), MenuAction::ToggleShowHidden));
    items
}

// ---------------------------------------------------------------------------
// Geometry (shared by render, occlusion pre-pass, and hit-testing)
// ---------------------------------------------------------------------------

fn menu_size(items: &[MenuItem]) -> (u16, u16) {
    let w = items.iter()
        .map(|i| i.label.chars().count() + if i.submenu.is_empty() { 0 } else { 2 })
        .max()
        .unwrap_or(8) as u16 + 4;
    let h = items.len() as u16 + 2;
    (w, h)
}

pub fn menu_rect(menu: &MenuState, term: Rect) -> Rect {
    let (w, h) = menu_size(&menu.items);
    let w = w.min(term.width);
    let h = h.min(term.height);
    let x = menu.pos.0.min(term.width.saturating_sub(w));
    let y = menu.pos.1.min(term.height.saturating_sub(h));
    Rect { x, y, width: w, height: h }
}

pub fn submenu_rect(menu: &MenuState, term: Rect) -> Rect {
    let parent = menu_rect(menu, term);
    let items = &menu.items[menu.selected].submenu;
    let (w, h) = menu_size(items);
    let w = w.min(term.width);
    let h = h.min(term.height);
    let mut x = parent.x + parent.width;
    if x + w > term.width {
        x = parent.x.saturating_sub(w);
    }
    let y = (parent.y + 1 + menu.selected as u16).min(term.height.saturating_sub(h));
    Rect { x, y, width: w, height: h }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub fn render_menu(f: &mut Frame, menu: &mut MenuState) {
    let term = f.area();
    let rect = menu_rect(menu, term);
    menu.rect = rect;
    render_item_list(f, rect, &menu.items, if menu.submenu_open { None } else { Some(menu.selected) });

    if menu.submenu_open {
        let sub_rect = submenu_rect(menu, term);
        menu.submenu_rect = sub_rect;
        let items = menu.items[menu.selected].submenu.clone();
        render_item_list(f, sub_rect, &items, Some(menu.submenu_selected));
    } else {
        menu.submenu_rect = Rect::default();
    }
}

fn render_item_list(f: &mut Frame, rect: Rect, items: &[MenuItem], selected: Option<usize>) {
    f.render_widget(Clear, rect);

    let inner_w = rect.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = items.iter().enumerate().map(|(i, item)| {
        if item.separator {
            return Line::styled(
                "\u{2500}".repeat(inner_w),
                Style::default().fg(theme::BORDER).bg(MENU_BG),
            );
        }
        let arrow = if item.submenu.is_empty() { "" } else { " \u{f0da}" };
        let pad = inner_w
            .saturating_sub(item.label.chars().count() + arrow.chars().count() + 1);
        let text = format!(" {}{}{}", item.label, " ".repeat(pad), arrow);

        let mut style = Style::default().bg(MENU_BG);
        if !item.enabled {
            style = style.add_modifier(Modifier::DIM);
        } else if selected == Some(i) {
            style = style.bg(MENU_SEL).fg(Color::White).add_modifier(Modifier::BOLD);
        }
        Line::from(Span::styled(text, style))
    }).collect();

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER).bg(MENU_BG))
                .style(Style::default().bg(MENU_BG)),
        ),
        rect,
    );
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

fn step_selection(items: &[MenuItem], current: usize, delta: i32) -> usize {
    let len = items.len();
    if len == 0 { return 0; }
    let mut idx = current as i32;
    for _ in 0..len {
        idx += delta;
        if idx < 0 || idx >= len as i32 {
            return current;
        }
        if items[idx as usize].selectable() {
            return idx as usize;
        }
    }
    current
}

pub fn handle_menu_key(menu: &mut MenuState, code: KeyCode) -> MenuOutcome {
    if menu.submenu_open {
        let sub = menu.items[menu.selected].submenu.clone();
        match code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('q') => {
                menu.submenu_open = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                menu.submenu_selected = step_selection(&sub, menu.submenu_selected, -1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                menu.submenu_selected = step_selection(&sub, menu.submenu_selected, 1);
            }
            KeyCode::Enter => {
                if let Some(item) = sub.get(menu.submenu_selected)
                    && item.selectable() {
                        return MenuOutcome::Action(item.action.clone());
                    }
            }
            _ => {}
        }
        return MenuOutcome::Consumed;
    }

    match code {
        KeyCode::Esc | KeyCode::Char('q') => return MenuOutcome::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            menu.selected = step_selection(&menu.items, menu.selected, -1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            menu.selected = step_selection(&menu.items, menu.selected, 1);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if !menu.items[menu.selected].submenu.is_empty() {
                open_submenu(menu);
            }
        }
        KeyCode::Enter => {
            let item = &menu.items[menu.selected];
            if !item.submenu.is_empty() {
                open_submenu(menu);
            } else if item.selectable() {
                return MenuOutcome::Action(item.action.clone());
            }
        }
        _ => {}
    }
    MenuOutcome::Consumed
}

fn open_submenu(menu: &mut MenuState) {
    let first = menu.items[menu.selected].submenu.iter()
        .position(|i| i.selectable())
        .unwrap_or(0);
    menu.submenu_open = true;
    menu.submenu_selected = first;
}

fn item_at(rect: Rect, items: &[MenuItem], col: u16, row: u16) -> Option<usize> {
    if !rect.contains(ratatui::layout::Position { x: col, y: row }) {
        return None;
    }
    if row <= rect.y || row >= rect.y + rect.height.saturating_sub(1) {
        return None; // border rows
    }
    let idx = (row - rect.y - 1) as usize;
    if idx < items.len() { Some(idx) } else { None }
}

/// Left-click routing while a menu is open.
pub fn handle_menu_click(menu: &mut MenuState, col: u16, row: u16) -> MenuOutcome {
    if menu.submenu_open {
        let sub = menu.items[menu.selected].submenu.clone();
        if let Some(idx) = item_at(menu.submenu_rect, &sub, col, row) {
            if sub[idx].selectable() {
                return MenuOutcome::Action(sub[idx].action.clone());
            }
            return MenuOutcome::Consumed;
        }
        // Click in the parent menu falls through to parent handling
        if !menu.rect.contains(ratatui::layout::Position { x: col, y: row }) {
            return MenuOutcome::Outside;
        }
        menu.submenu_open = false;
    }

    if let Some(idx) = item_at(menu.rect, &menu.items, col, row) {
        let item = &menu.items[idx];
        if item.separator || !item.enabled {
            return MenuOutcome::Consumed;
        }
        if !item.submenu.is_empty() {
            menu.selected = idx;
            open_submenu(menu);
            return MenuOutcome::Consumed;
        }
        return MenuOutcome::Action(item.action.clone());
    }

    if menu.rect.contains(ratatui::layout::Position { x: col, y: row }) {
        return MenuOutcome::Consumed; // border click
    }
    MenuOutcome::Outside
}

/// Mouse-move hover highlighting.
pub fn handle_menu_hover(menu: &mut MenuState, col: u16, row: u16) {
    if menu.submenu_open {
        let sub = &menu.items[menu.selected].submenu;
        if let Some(idx) = item_at(menu.submenu_rect, sub, col, row)
            && sub[idx].selectable() {
                menu.submenu_selected = idx;
            }
        return;
    }
    let items = menu.items.clone();
    if let Some(idx) = item_at(menu.rect, &items, col, row)
        && items[idx].selectable() {
            menu.selected = idx;
        }
}
