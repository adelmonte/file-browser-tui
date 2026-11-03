use std::path::PathBuf;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use ratatui_image::{StatefulImage, picker::ProtocolType};

use crate::app::App;
use crate::events::SidebarTarget;
use crate::ui::{kitty, theme};

/// Columns for the mini icon (one row tall, roughly square).
const ICON_W: u16 = 2;

/// Places panel in Dolphin's layout: muted section headers, a theme icon
/// per row, and a usage bar under each device.
pub fn render_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(1) };
    app.sidebar_area = inner;

    let cwd = app.cwd.clone();
    let hover = app.mouse_pos.and_then(|(c, r)| target_at(app, c, r));

    struct Row {
        target: SidebarTarget,
        icon: String,
        name: String,
        current: bool,
        dim: bool,
        usage: Option<f32>,
    }
    let mut sections: Vec<(&str, Vec<Row>)> = Vec::new();

    let places = app.places.iter().enumerate().map(|(i, p)| Row {
        target: SidebarTarget::Place(i),
        icon: place_icon(&p.path, p.icon.as_deref()),
        name: p.name.clone(),
        current: std::path::Path::new(&p.path) == cwd,
        dim: false,
        usage: None,
    }).collect();
    sections.push(("Places", places));

    let mut devices: Vec<Row> = app.devices.iter().enumerate().map(|(i, d)| Row {
        target: SidebarTarget::Device(i),
        icon: if d.path == "/" { "drive-harddisk-root".into() } else { "drive-harddisk".into() },
        name: d.name.clone(),
        current: std::path::Path::new(&d.path) == cwd,
        dim: false,
        usage: d.usage,
    }).collect();
    devices.extend(app.kdeconnect_devices.iter().enumerate().map(|(i, d)| Row {
        target: SidebarTarget::Phone(i),
        icon: if d.reachable { "smartphone".into() } else { "smartphonedisconnected".into() },
        name: d.name.clone(),
        current: app.kdeconnect_mounts.get(&d.id).is_some_and(|m| cwd.starts_with(m)),
        dim: !d.reachable,
        usage: None,
    }));
    if !devices.is_empty() {
        sections.push(("Devices", devices));
    }

    let width = inner.width;
    let bottom = inner.y + inner.height;
    let mut y = inner.y;
    let mut row_map: Vec<Option<SidebarTarget>> = Vec::new();

    for (si, (title, rows)) in sections.iter().enumerate() {
        if si > 0 && y < bottom {
            y += 1;
            row_map.push(None);
        }
        if y >= bottom { break; }
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {title}"), Style::default().fg(theme::MUTED))),
            Rect { x: inner.x, y, width, height: 1 },
        );
        y += 1;
        row_map.push(None);

        for row in rows {
            let h = if row.usage.is_some() { 2 } else { 1 };
            if y + h > bottom { break; }
            let rect = Rect { x: inner.x, y, width, height: h };
            let bg = if row.current {
                Some(theme::SEL)
            } else if hover == Some(row.target) {
                Some(theme::HOVER)
            } else {
                None
            };
            if let Some(c) = bg {
                f.render_widget(Block::default().style(Style::default().bg(theme::rgb(c))), rect);
            }
            let fg = if row.dim { theme::MUTED } else if row.current { Color::White } else { theme::TEXT };

            draw_icon(f, app, &row.icon, Rect { x: inner.x + 1, y, width: ICON_W, height: 1 }, bg);
            let text_x = inner.x + 1 + ICON_W + 1;
            let text_w = (inner.x + width).saturating_sub(text_x + 1);
            let name: String = row.name.chars().take(text_w as usize).collect();
            f.render_widget(Paragraph::new(Span::styled(name, Style::default().fg(fg))), Rect { x: text_x, y, width: text_w, height: 1 });

            if let Some(usage) = row.usage {
                f.render_widget(Paragraph::new(usage_bar(usage, text_w)), Rect { x: text_x, y: y + 1, width: text_w, height: 1 });
            }
            for _ in 0..h {
                row_map.push(Some(row.target));
            }
            y += h;
        }
    }
    app.sidebar_rows = row_map;
}

/// Thin two-tone bar (Dolphin's free-space indicator); red when nearly full.
fn usage_bar(usage: f32, width: u16) -> Line<'static> {
    let filled = ((usage.clamp(0.0, 1.0) * width as f32).round() as usize).min(width as usize);
    let color = if usage > 0.9 { theme::rgb(theme::NORD11) } else { theme::ACCENT };
    Line::from(vec![
        Span::styled("\u{2501}".repeat(filled), Style::default().fg(color)),
        Span::styled("\u{2501}".repeat(width as usize - filled), Style::default().fg(theme::BORDER)),
    ])
}

/// Draw a theme icon by name (cached separately from file icons).
pub fn draw_icon(f: &mut Frame, app: &mut App, name: &str, rect: Rect, bg: Option<(u8, u8, u8)>) {
    // Sidebar icons are keyed by theme name, in their own cache
    let key = PathBuf::from(format!("sidebar-icon/{name}"));
    let px = app.picker.font_size().height as u32;
    app.sidebar_icons.request(&key, true, px.max(16), Some(name.to_string()));
    let Some(proto) = app.sidebar_icons.get_mut(&app.picker, &key, rect.width, rect.height, bg) else { return; };
    f.render_widget(Clear, rect);
    if let Some(c) = bg {
        f.render_widget(Block::default().style(Style::default().bg(theme::rgb(c))), rect);
    }
    f.render_stateful_widget(StatefulImage::new(), rect, proto);
    if app.picker.protocol_type() == ProtocolType::Kitty {
        kitty::split_placeholder_rows(f.buffer_mut(), rect, &mut app.kitty_transmits);
    }
}

/// Map a mouse position to a sidebar target using the rendered row map.
pub fn target_at(app: &App, col: u16, row: u16) -> Option<SidebarTarget> {
    let area = app.sidebar_area;
    if col + 1 < area.x || col > area.x + area.width { return None; }
    if row < area.y || row >= area.y + area.height { return None; }
    app.sidebar_rows.get((row - area.y) as usize).copied().flatten()
}

/// Whether a position is anywhere inside the sidebar (for empty-area menus).
pub fn contains(app: &App, col: u16, row: u16) -> bool {
    let area = app.sidebar_area;
    col + 1 >= area.x && col < area.x + area.width + 1 && row + 1 >= area.y && row < area.y + area.height
}

/// Theme icon name for a place: the configured hint, then XDG folders.
fn place_icon(path: &str, hint: Option<&str>) -> String {
    if let Some(h) = hint {
        return h.to_string();
    }
    let p = std::path::Path::new(path);
    let xdg = [
        (dirs::home_dir(), "user-home"),
        (dirs::desktop_dir(), "user-desktop"),
        (dirs::download_dir(), "folder-download"),
        (dirs::audio_dir(), "folder-music"),
        (dirs::picture_dir(), "folder-pictures"),
        (dirs::video_dir(), "folder-videos"),
        (dirs::document_dir(), "folder-documents"),
        (dirs::template_dir(), "folder-templates"),
        (dirs::public_dir(), "folder-publicshare"),
    ];
    if let Some((_, name)) = xdg.iter().find(|(d, _)| d.as_deref() == Some(p)) {
        return name.to_string();
    }
    if path.contains("Trash") { "user-trash".into() } else { "folder".into() }
}
