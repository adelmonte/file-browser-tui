use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, TOPBAR_H};
use crate::ui::menu::{self, MenuKind};

/// nord1 — distinguishes the bar from the panels beneath it.
const BAR_BG: Color = Color::Rgb(59, 66, 82);
/// nord0 — recessed path field.
const PATH_BG: Color = Color::Rgb(46, 52, 64);

pub fn render_topbar(f: &mut Frame, app: &mut App, area: Rect) {
    f.render_widget(Block::default().style(Style::default().bg(BAR_BG)), area);

    // Center content on the middle row of the padded topbar area
    let content_area = Rect {
        x: area.x,
        y: area.y + area.height / 2,
        width: area.width,
        height: 1,
    };

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),   // left pad
            Constraint::Length(3),   // back
            Constraint::Length(1),   // gap
            Constraint::Length(3),   // forward
            Constraint::Length(1),   // gap
            Constraint::Length(3),   // up
            Constraint::Length(2),   // gap
            Constraint::Length(12),  // view mode button
            Constraint::Length(1),   // gap
            Constraint::Min(1),      // path bar
            Constraint::Length(1),   // gap
            Constraint::Length(4),   // sort button
        ])
        .split(content_area);

    app.topbar_back_rect = chunks[1];
    app.topbar_fwd_rect = chunks[3];
    app.topbar_up_rect = chunks[5];
    app.topbar_view_rect = chunks[7];
    app.topbar_path_rect = chunks[9];
    app.topbar_sort_rect = chunks[11];

    let btn = |glyph: &str, enabled: bool| {
        let style = if enabled {
            Style::default().bg(BAR_BG)
        } else {
            Style::default().bg(BAR_BG).add_modifier(Modifier::DIM)
        };
        Paragraph::new(Line::from(Span::styled(format!(" {glyph} "), style)))
    };

    f.render_widget(btn("\u{f053}", !app.history_back.is_empty()), chunks[1]);
    f.render_widget(btn("\u{f054}", !app.history_forward.is_empty()), chunks[3]);
    f.render_widget(btn("\u{f062}", app.cwd.parent().is_some()), chunks[5]);

    let menu_kind = app.menu.as_ref().map(|m| m.kind);

    // View mode button
    let view_label = format!(" {} {} \u{f0d7}", menu::view_mode_icon(app.view_mode), app.view_mode);
    let view_style = if menu_kind == Some(MenuKind::View) {
        Style::default().bg(BAR_BG).add_modifier(Modifier::REVERSED)
    } else {
        Style::default().bg(BAR_BG)
    };
    f.render_widget(Paragraph::new(view_label).style(view_style), chunks[7]);

    // Path bar — recessed field
    let path_str = if app.path_bar_editing {
        format!(" {}\u{2588}", app.path_bar_input)
    } else {
        format!(" {}", app.cwd.display())
    };
    let path_style = if app.path_bar_editing {
        Style::default().bg(PATH_BG).fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(PATH_BG)
    };
    f.render_widget(Paragraph::new(path_str).style(path_style), chunks[9]);

    // Sort button — icon only
    let sort_style = if menu_kind == Some(MenuKind::Sort) {
        Style::default().bg(BAR_BG).add_modifier(Modifier::REVERSED)
    } else {
        Style::default().bg(BAR_BG)
    };
    f.render_widget(Paragraph::new(" \u{f0dc} ").style(sort_style), chunks[11]);
    // NOTE: menus/completions rendered in ui/mod.rs AFTER the grid (z-order)
}

/// Geometry of the path completion dropdown (shared with hit-testing/occlusion).
pub fn completions_rect(app: &App, term: Rect) -> Option<Rect> {
    if !app.path_bar_editing || app.path_completions.is_empty() {
        return None;
    }
    let anchor = app.topbar_path_rect;
    let max_show = app.path_completions.len().min(8) as u16;
    let height = max_show + 2;
    let width = anchor.width.max(20).min(term.width);
    // Drop below the full topbar (not just the content row)
    let y = TOPBAR_H.min(term.height.saturating_sub(height));
    let x = anchor.x.min(term.width.saturating_sub(width));
    Some(Rect { x, y, width, height })
}

/// Called from ui/mod.rs after the grid renders when path bar is active.
pub fn render_path_completions(f: &mut Frame, app: &mut App) {
    let Some(area) = completions_rect(app, f.area()) else {
        app.completions_rect = Rect::default();
        return;
    };
    app.completions_rect = area;

    let max_show = (area.height.saturating_sub(2)) as usize;
    let items: Vec<ListItem> = app.path_completions.iter()
        .take(max_show)
        .enumerate()
        .map(|(i, p)| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(p.to_str().unwrap_or(""));
            let label = format!(" \u{f07b} {}", name);
            let style = if app.path_completion_sel == Some(i) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    f.render_widget(Clear, area);
    let mut state = ListState::default().with_selected(app.path_completion_sel);
    f.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Path "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}
