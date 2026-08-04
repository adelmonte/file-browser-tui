use chrono::{DateTime, Local};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Block, Cell, Row, Table, TableState},
};

use crate::app::App;

pub fn render_details(f: &mut Frame, app: &mut App, area: Rect) {
    app.grid_cols = 1;
    app.grid_area = area;

    let entries = &app.listing.entries;

    let header = Row::new(vec![
        Cell::from("Name").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Size").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Modified").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Type").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
    ]).height(1);

    let rows: Vec<Row> = entries.iter().enumerate().map(|(i, e)| {
        let icon = if e.is_dir {
            "\u{f07b}".to_string()
        } else {
            crate::ui::grid::file_placeholder_icon_pub(
                e.path.extension().and_then(|ex| ex.to_str()).unwrap_or("")
            ).to_string()
        };
        let name = format!("{} {}", icon, e.name);
        let size = format_size(e.size, e.is_dir);
        let modified = e.modified
            .map(|t| { let dt: DateTime<Local> = t.into(); dt.format("%Y-%m-%d %H:%M").to_string() })
            .unwrap_or_default();
        let type_str = e.mime_type.as_deref()
            .unwrap_or(if e.is_dir { "folder" } else { "file" })
            .to_string();

        let is_selected = app.selection == Some(i);
        let row_style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        let name_style = if e.is_dir {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        Row::new(vec![
            Cell::from(name).style(name_style),
            Cell::from(size),
            Cell::from(modified),
            Cell::from(type_str),
        ])
        .style(row_style)
        .height(1)
    }).collect();

    let selected_row = app.selection.map(|s| s.saturating_sub(app.scroll_offset));
    let mut table_state = TableState::default().with_selected(selected_row);

    let widths = [
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Min(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default())
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(table, area, &mut table_state);
}

fn format_size(size: Option<u64>, is_dir: bool) -> String {
    if is_dir { return "\u{2014}".to_string(); }
    match size {
        None => "\u{2014}".to_string(),
        Some(b) if b < 1024 => format!("{b} B"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        Some(b) if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        Some(b) => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}
