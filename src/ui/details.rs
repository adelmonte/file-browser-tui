use chrono::{DateTime, Local};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Cell, Row, Table},
};

use crate::app::App;
use crate::ui::theme;

pub fn render_details(f: &mut Frame, app: &mut App, area: Rect) {
    f.render_widget(Block::default(), area);

    // One padding column on each side, matching the grid views
    let area = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    app.grid_cols = 1;
    app.grid_area = area;

    let searching = app.search.as_ref().is_some_and(|s| s.has_query());
    let header_style = Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Cell::from(" Name"),
        Cell::from(Line::from("Size").alignment(Alignment::Right)),
        Cell::from("Modified"),
        Cell::from(if searching { "Location" } else { "Type" }),
    ]).style(header_style).height(1);

    // Rows start at scroll_offset so the table, scrolling and click mapping
    // (details_row_at_pos) all agree on which entry is on which line.
    let visible = area.height.saturating_sub(1) as usize;
    let rows: Vec<Row> = app.listing.entries.iter().enumerate()
        .skip(app.scroll_offset)
        .take(visible)
        .map(|(i, e)| {
            let icon = if e.is_dir {
                "\u{f07b}"
            } else {
                crate::ui::grid::file_placeholder_icon_pub(
                    e.path.extension().and_then(|ex| ex.to_str()).unwrap_or("")
                )
            };
            let name = format!(" {} {}", icon, e.name);
            let size = format_size(e.size, e.is_dir);
            let modified = e.modified
                .map(|t| { let dt: DateTime<Local> = t.into(); dt.format("%Y-%m-%d %H:%M").to_string() })
                .unwrap_or_default();
            let type_str = if searching {
                e.path.parent().map(crate::ui::statusbar::display_path).unwrap_or_default()
            } else if e.is_dir {
                "folder".to_string()
            } else {
                e.mime_type.clone()
                    .or_else(|| new_mime_guess::from_path(&e.path).first_raw().map(str::to_string))
                    .unwrap_or_else(|| "file".to_string())
            };

            let is_selected = app.selection == Some(i) || app.multi_selection.contains(&i);
            let row_style = if is_selected {
                Style::default().bg(theme::SEL_BG).fg(Color::White)
            } else {
                Style::default()
            };
            let muted = if is_selected { Style::default() } else { Style::default().fg(theme::MUTED) };

            Row::new(vec![
                Cell::from(name),
                Cell::from(Line::from(size).alignment(Alignment::Right)),
                Cell::from(modified).style(muted),
                Cell::from(type_str).style(muted),
            ])
            .style(row_style)
            .height(1)
        }).collect();

    let widths = [
        Constraint::Fill(3),
        Constraint::Length(10),
        Constraint::Length(16),
        Constraint::Fill(1),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2);

    f.render_widget(table, area);
}

pub fn format_size(size: Option<u64>, is_dir: bool) -> String {
    if is_dir { return "\u{2014}".to_string(); }
    match size {
        None => "\u{2014}".to_string(),
        Some(b) if b < 1024 => format!("{b} B"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        Some(b) if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        Some(b) => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}
