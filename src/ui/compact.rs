use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::App;

const COMPACT_CELL_W: u16 = 20;
const COMPACT_CELL_H: u16 = 2;

pub fn render_compact(f: &mut Frame, app: &mut App, area: Rect) {
    f.render_widget(Block::default(), area);

    let cols = (area.width / COMPACT_CELL_W).max(1);
    app.grid_cols = cols;
    app.grid_area = area;

    let entries_len = app.listing.entries.len();
    let rows_visible = (area.height / COMPACT_CELL_H + 2) as usize;

    for row_idx in 0..rows_visible {
        let abs_row = app.scroll_offset + row_idx;
        for col_idx in 0..cols as usize {
            let entry_idx = abs_row * cols as usize + col_idx;
            if entry_idx >= entries_len { break; }

            let cell_x = area.x + col_idx as u16 * COMPACT_CELL_W;
            let cell_y = area.y + row_idx as u16 * COMPACT_CELL_H;
            if cell_y >= area.y + area.height { break; }

            let entry = &app.listing.entries[entry_idx];
            let is_selected = app.selection == Some(entry_idx);

            let icon = if entry.is_dir {
                "\u{f07b}"
            } else {
                crate::ui::grid::file_placeholder_icon_pub(
                    entry.path.extension().and_then(|e| e.to_str()).unwrap_or("")
                )
            };

            let name = truncate_name(&entry.name, COMPACT_CELL_W as usize - 3);
            let text = format!("{} {}", icon, name);

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            f.render_widget(
                Paragraph::new(Line::from(Span::styled(text, style)))
                    .alignment(Alignment::Left),
                Rect { x: cell_x + 1, y: cell_y, width: COMPACT_CELL_W.saturating_sub(1), height: 1 },
            );
        }
    }
}

fn truncate_name(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max {
        name.to_string()
    } else {
        let mut s: String = chars[..max.saturating_sub(1)].iter().collect();
        s.push('\u{2026}');
        s
    }
}
