use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::events::SidebarTarget;

pub fn render_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(ratatui::style::Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.sidebar_area = inner;

    let cwd = app.cwd.to_string_lossy();
    let mut lines: Vec<Line> = Vec::new();
    let mut rows: Vec<Option<SidebarTarget>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " PLACES",
        Style::default().add_modifier(Modifier::BOLD | Modifier::DIM),
    )));
    rows.push(None);

    for (i, place) in app.places.iter().enumerate() {
        let is_current = place.path == cwd.as_ref();
        let icon = place_icon_for_path(&place.path, place.icon.as_deref());
        let style = if is_current {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {}", icon, place.name),
            style,
        )));
        rows.push(Some(SidebarTarget::Place(i)));
    }

    lines.push(Line::raw(""));
    rows.push(None);
    lines.push(Line::from(Span::styled(
        " DEVICES",
        Style::default().add_modifier(Modifier::BOLD | Modifier::DIM),
    )));
    rows.push(None);

    for (i, device) in app.devices.iter().enumerate() {
        let is_current = device.path == cwd.as_ref();
        let style = if is_current {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!(" \u{f0a0} {}", device.name),
            style,
        )));
        rows.push(Some(SidebarTarget::Device(i)));
    }

    app.sidebar_rows = rows;
    f.render_widget(Paragraph::new(lines), inner);
}

/// Map a mouse position to a sidebar target using the rendered row map.
pub fn target_at(app: &App, col: u16, row: u16) -> Option<SidebarTarget> {
    let area = app.sidebar_area;
    if col < area.x || col >= area.x + area.width { return None; }
    if row < area.y || row >= area.y + area.height { return None; }
    app.sidebar_rows.get((row - area.y) as usize).copied().flatten()
}

/// Whether a position is anywhere inside the sidebar (for empty-area menus).
pub fn contains(app: &App, col: u16, row: u16) -> bool {
    let area = app.sidebar_area;
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Pick a nerd-font glyph for a place, preferring an explicit icon hint, then
/// falling back to XDG directory detection, then the path name, then generic folder.
fn place_icon_for_path(path: &str, icon_hint: Option<&str>) -> &'static str {
    // Explicit icon name in config takes priority
    if let Some(hint) = icon_hint {
        let mapped = icon_name_to_glyph(hint);
        if mapped != "\u{f07b}" { return mapped; }
    }

    // Auto-detect from XDG dirs
    let p = std::path::Path::new(path);
    if dirs::home_dir().as_deref()     == Some(p) { return "\u{f015}"; }
    if dirs::desktop_dir().as_deref()  == Some(p) { return "\u{f108}"; }
    if dirs::download_dir().as_deref() == Some(p) { return "\u{f019}"; }
    if dirs::audio_dir().as_deref()    == Some(p) { return "\u{f001}"; }
    if dirs::picture_dir().as_deref()  == Some(p) { return "\u{f03e}"; }
    if dirs::video_dir().as_deref()    == Some(p) { return "\u{f03d}"; }
    if dirs::document_dir().as_deref() == Some(p) { return "\u{f0f6}"; }
    if dirs::template_dir().as_deref() == Some(p) { return "\u{f0c5}"; }
    if dirs::public_dir().as_deref()   == Some(p) { return "\u{f07c}"; }

    // Trash by path
    if path.contains("Trash") || path.contains(".local/share/Trash") { return "\u{f1f8}"; }

    "\u{f07b}"
}

fn icon_name_to_glyph(name: &str) -> &'static str {
    match name {
        "user-home"        => "\u{f015}",
        "user-desktop"     => "\u{f108}",
        "folder-documents" => "\u{f0f6}",
        "folder-download" | "folder-downloads" => "\u{f019}",
        "folder-music"     => "\u{f001}",
        "folder-pictures"  => "\u{f03e}",
        "folder-videos"    => "\u{f03d}",
        "user-trash"       => "\u{f1f8}",
        _                  => "\u{f07b}",
    }
}
