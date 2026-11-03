pub mod compact;
pub mod details;
pub mod drag_ghost;
pub mod grid;
pub mod help;
pub mod kitty;
pub mod menu;
pub mod properties;
pub mod sidebar;
pub mod statusbar;
pub mod theme;
pub mod topbar;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use std::io::Write;

use crate::app::{App, TOPBAR_H, ViewMode};

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(TOPBAR_H),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let topbar_area = vertical[0];
    let content_area = vertical[1];
    let status_area = vertical[2];

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(app.sidebar_w),
            Constraint::Min(0),
        ])
        .split(content_area);

    let sidebar_area = horizontal[0];
    let main_area = horizontal[1];

    app.main_area = main_area;

    // Render base layers
    topbar::render_topbar(f, app, topbar_area);
    sidebar::render_sidebar(f, app, sidebar_area);
    statusbar::render_statusbar(f, app, status_area);

    match app.view_mode {
        ViewMode::Icons   => grid::render_grid(f, app, main_area),
        ViewMode::Compact => compact::render_compact(f, app, main_area),
        ViewMode::Details => details::render_details(f, app, main_area),
    }

    // Overlays rendered AFTER the grid so they appear on top of everything
    if let Some(mut m) = app.menu.take() {
        menu::render_menu(f, &mut m);
        app.menu = Some(m);
    }
    drag_ghost::render_drag_ghost(f, app);
    if app.path_bar_editing {
        topbar::render_path_completions(f, app);
    } else {
        app.completions_rect = Rect::default();
    }
    if let Some(ref props) = app.properties {
        properties::render_properties(f, props);
    }
    if app.show_help {
        help::render_help(f);
    }
    if app.confirm_delete.is_some() {
        render_confirm_delete(f, app);
    }

    // Image uploads lifted out of placeholder cells go out before this
    // frame's cell updates, so the placeholders always have an image.
    if !app.kitty_transmits.is_empty() {
        let mut out = std::io::stdout().lock();
        for t in app.kitty_transmits.drain(..) {
            let _ = out.write_all(t.as_bytes());
        }
        let _ = out.flush();
    }
}

fn confirm_delete_rect(term: Rect) -> Rect {
    let w = 44u16.min(term.width);
    let h = 7u16.min(term.height);
    Rect {
        x: term.width.saturating_sub(w) / 2,
        y: term.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

fn render_confirm_delete(f: &mut Frame, app: &App) {
    let Some(ref cd) = app.confirm_delete else { return; };
    let area = confirm_delete_rect(f.area());

    let name = if cd.display_name.chars().count() > 28 {
        format!("{}…", cd.display_name.chars().take(27).collect::<String>())
    } else {
        cd.display_name.clone()
    };

    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::rgb(theme::NORD11)))
            .title(" Permanently Delete ")
            .style(Style::default().bg(Color::Rgb(59, 20, 20)).fg(Color::White)),
        area,
    );

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let lines = vec![
        Line::raw(format!("  Delete \"{}\"?", name)),
        Line::raw(""),
        Line::raw("  This cannot be undone."),
        Line::raw(""),
        Line::styled(
            "  [Enter / Y]  Yes, delete     [Esc / N]  Cancel",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Rgb(59, 20, 20)).fg(Color::White)),
        inner,
    );
}
