use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Clear, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{StatefulImage, picker::ProtocolType};

use crate::app::App;
use crate::events::{ClipOp, DragState};
use crate::ui::{kitty, theme::{self, rgb}};


/// Inset a content pane for a grid of `cell_w`-wide cells: at least one
/// padding column per side, leftover width split evenly so the grid sits
/// centered instead of piling the slack on the right.
pub fn centered_grid_area(area: Rect, cell_w: u16) -> Rect {
    let usable = area.width.saturating_sub(2);
    let cols = (usable / cell_w).max(1);
    let width = (cols * cell_w).min(area.width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        width,
        ..area
    }
}

pub fn cell_at_pos(
    (col, row): (u16, u16),
    grid_area: Rect, (scroll_offset, sub): (usize, u16),
    cols: u16, entry_count: usize,
    (cell_w, cell_h): (u16, u16),
) -> Option<usize> {
    if col < grid_area.x || row < grid_area.y { return None; }
    if col >= grid_area.x + grid_area.width || row >= grid_area.y + grid_area.height { return None; }
    let cell_col = (col - grid_area.x) / cell_w;
    if cell_col >= cols { return None; }
    let cell_row = (row - grid_area.y + sub) / cell_h;
    let idx = (scroll_offset + cell_row as usize) * cols as usize + cell_col as usize;
    if idx < entry_count { Some(idx) } else { None }
}

pub fn details_row_at_pos(
    row: u16, grid_area: Rect, scroll_offset: usize, entry_count: usize,
) -> Option<usize> {
    if row < grid_area.y + 1 { return None; }
    if row >= grid_area.y + grid_area.height { return None; }
    let idx = scroll_offset + (row - grid_area.y - 1) as usize;
    if idx < entry_count { Some(idx) } else { None }
}

pub fn render_grid(f: &mut Frame, app: &mut App, area: Rect) {
    let cell_w = app.cell_w;
    let cell_h = app.cell_h;
    let icon_rows = app.icon_rows;

    f.render_widget(Block::default(), area);

    // Spread the leftover width between columns (Dolphin's layout) instead
    // of leaving it as margins; items keep their size within the pitch
    let usable = area.width.saturating_sub(2);
    let cols = (usable / cell_w).max(1);
    let pitch = (usable / cols).max(cell_w).min(area.width);
    let width = (cols * pitch).min(area.width);
    let area = Rect { x: area.x + area.width.saturating_sub(width) / 2, width, ..area };
    app.grid_cols = cols;
    app.grid_area = area;
    app.grid_pitch = pitch;
    let item_off = (pitch - cell_w) / 2;

    let entries_len = app.listing.entries.len();
    if entries_len == 0 {
        f.render_widget(
            Paragraph::new("Folder is empty").style(Style::default().fg(theme::MUTED)),
            Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: 1 },
        );
        return;
    }

    // Square-in-pixels icon box (see app::icon_dims)
    let img_w_cells = app.img_w.min(cell_w);
    let img_x_off = (cell_w.saturating_sub(img_w_cells)) / 2;

    let rows_visible = (area.height / cell_h + 2) as usize;
    // One column of breathing room on each side so neighbouring labels never touch
    let label_w = if cell_w >= 6 { cell_w as usize - 1 } else { cell_w as usize };

    let hovered = if app.menu.is_none() && matches!(app.drag_state, DragState::Idle) {
        app.mouse_pos.and_then(|p| cell_at_pos(p, area, (app.scroll_offset, app.scroll_sub), cols, entries_len, (pitch, cell_h)))
    } else {
        None
    };

    // Rows are offset by the smooth-scroll position; items cut by the top
    // or bottom edge are drawn whole off-screen and only their visible rows
    // copied in (Kitty placeholders address each cell, so images clip too)
    let geo = CellGeo { cell_w, cell_h, icon_rows, img_w_cells, img_x_off, label_w };
    let sub = app.scroll_sub as i32;
    for row_idx in 0..rows_visible {
        let abs_row = app.scroll_offset + row_idx;
        let y = area.y as i32 + row_idx as i32 * cell_h as i32 - sub;
        if y >= area.bottom() as i32 { break; }
        for col_idx in 0..cols as usize {
            let entry_idx = abs_row * cols as usize + col_idx;
            if entry_idx >= entries_len { break; }
            let cell_x = area.x + col_idx as u16 * pitch + item_off;

            if y >= area.y as i32 && y + cell_h as i32 <= area.bottom() as i32 {
                let cell_rect = Rect { x: cell_x, y: y as u16, width: cell_w, height: cell_h };
                draw_cell(f.buffer_mut(), app, entry_idx, cell_rect, hovered, &geo);
                continue;
            }
            let cell_rect = Rect { x: cell_x, y: 0, width: cell_w, height: cell_h };
            let mut scratch = Buffer::empty(cell_rect);
            draw_cell(&mut scratch, app, entry_idx, cell_rect, hovered, &geo);
            let buf = f.buffer_mut();
            for r in 0..cell_h {
                let sy = y + r as i32;
                if sy < area.y as i32 || sy >= area.bottom() as i32 { continue; }
                for x in cell_x..cell_x + cell_w {
                    if let (Some(src), Some(dst)) = (scratch.cell((x, r)), buf.cell_mut((x, sy as u16))) {
                        *dst = src.clone();
                    }
                }
            }
        }
    }
}

struct CellGeo {
    cell_w: u16,
    cell_h: u16,
    icon_rows: u16,
    img_w_cells: u16,
    img_x_off: u16,
    label_w: usize,
}

fn draw_cell(buf: &mut Buffer, app: &mut App, entry_idx: usize, cell_rect: Rect, hovered: Option<usize>, geo: &CellGeo) {
    let CellGeo { cell_w, cell_h, icon_rows, img_w_cells, img_x_off, label_w } = *geo;
    let (cell_x, cell_y) = (cell_rect.x, cell_rect.y);

    // Centered, square-ish image rect
    let icon_rect = Rect {
        x: cell_x + img_x_off,
        y: cell_y,
        width: img_w_cells,
        height: icon_rows,
    };

    let label_rect = Rect {
        x: cell_x,
        y: cell_y + icon_rows,
        width: cell_w,
        height: cell_h.saturating_sub(icon_rows),
    };

    let is_selected = app.selection == Some(entry_idx)
        || app.multi_selection.contains(&entry_idx);
    let is_drag_src = matches!(&app.drag_state,
        DragState::Armed { item_idx, .. } | DragState::Dragging { item_idx, .. }
        if *item_idx == entry_idx
    );
    let is_drag_target = matches!(&app.drag_state,
        DragState::Dragging { hover_target: Some(t), .. }
        if *t == entry_idx
    ) || app.dnd.hover_entry == Some(entry_idx);
    let is_cut = app.clipboard.as_ref()
        .map(|c| c.op == ClipOp::Cut
            && c.paths.contains(&app.listing.entries[entry_idx].path))
        .unwrap_or(false);

    // The icon image is composited onto the same bg color, so the
    // highlight runs behind the icon as one full-cell rectangle.
    let cell_bg: Option<(u8, u8, u8)> = if is_selected {
        Some(theme::SEL)
    } else if is_drag_target {
        Some(theme::NORD14)
    } else if hovered == Some(entry_idx) {
        Some(theme::HOVER)
    } else {
        None
    };

    if let Some(bg) = cell_bg {
        Block::default().style(Style::default().bg(rgb(bg))).render(cell_rect, buf);
    } else if is_drag_src || is_cut {
        Block::default().style(Style::default().add_modifier(Modifier::DIM)).render(cell_rect, buf);
    }

    // Render icon/thumbnail image (skipped + placement-deleted under overlays)
    render_cell_image(buf, app, entry_idx, icon_rect, cell_bg);

    let entry = &app.listing.entries[entry_idx];
    let label_rows = cell_h.saturating_sub(icon_rows) as usize;

    // Inline rename
    if let Some(ref rename) = app.rename_state
        && rename.item_idx == entry_idx {
            // Thin bar marks the cursor inside the wrapped name
            let mut shown = rename.input.clone();
            shown.insert(rename.cursor_pos.min(shown.len()), '\u{258f}');
            let lines = wrap_label(&shown, label_w, label_rows);
            let bg = if rename.invalid { theme::NORD11 } else { theme::SEL };
            Paragraph::new(lines_to_text_centered(&lines, cell_w as usize))
                .style(Style::default().bg(rgb(bg)).fg(Color::White).add_modifier(Modifier::BOLD))
                .render(label_rect, buf);
            return;
        }

    let label_fg = if is_selected {
        Style::default().bg(theme::SEL_BG).fg(Color::White)
    } else if is_drag_target {
        Style::default().bg(rgb(theme::NORD14)).fg(Color::Black)
    } else if hovered == Some(entry_idx) {
        Style::default().bg(theme::HOVER_BG)
    } else {
        Style::default()
    };

    let label_lines = wrap_label(&entry.name, label_w, label_rows);
    Paragraph::new(lines_to_text_styled_centered(&label_lines, label_fg, cell_w as usize)).render(label_rect, buf);
}

pub(crate) fn render_cell_image(
    buf: &mut Buffer,
    app: &mut App,
    entry_idx: usize,
    icon_rect: Rect,
    bg: Option<(u8, u8, u8)>,
) {
    if icon_rect.width == 0 || icon_rect.height == 0 { return; }

    let entry_path = app.listing.entries[entry_idx].path.clone();
    let is_media = app.listing.entries[entry_idx].mime_type.as_deref()
        .map(|m| m.starts_with("image/") || m.starts_with("video/"))
        .unwrap_or(false);

    let (cols, rows) = (icon_rect.width, icon_rect.height);

    let drew = {
        let proto = if is_media {
            app.thumb_cache.get_mut(&app.picker, &entry_path, cols, rows, bg)
        } else {
            app.icon_cache.get_mut(&app.picker, &entry_path, cols, rows, bg)
        };
        if let Some(proto) = proto {
            Clear.render(icon_rect, buf);
            if let Some(c) = bg {
                Block::default().style(Style::default().bg(rgb(c))).render(icon_rect, buf);
            }
            StatefulImage::new().render(icon_rect, buf, proto);
            // Per-cell placeholders: overlays drawn later cover the image
            // cell by cell instead of the image having to be hidden.
            if app.picker.protocol_type() == ProtocolType::Kitty {
                kitty::split_placeholder_rows(buf, icon_rect, &mut app.kitty_transmits);
            }
            true
        } else {
            false
        }
    };

    if drew {
        app.frame_images.insert(entry_path, (icon_rect, bg));
    } else if std::time::Instant::now() >= app.icons_grace_until {
        // Placeholder nerd font icon, centered in the cell
        let entry = &app.listing.entries[entry_idx];
        let placeholder = if entry.is_dir {
            "\u{f07b}"
        } else {
            file_placeholder_icon(entry.path.extension().and_then(|e| e.to_str()).unwrap_or(""))
        };
        // Center 2-wide glyph horizontally and vertically within icon_rect
        let px = icon_rect.x + icon_rect.width.saturating_sub(2) / 2;
        let py = icon_rect.y + icon_rect.height / 2;
        if px + 2 <= icon_rect.x + icon_rect.width {
            Paragraph::new(placeholder).style(Style::default().add_modifier(Modifier::DIM)).render(Rect { x: px, y: py, width: 2, height: 1 }, buf);
        }
    }
}

/// Wrap a name into at most `max_lines` lines of `max_cols` display-columns each.
/// Last line gets an ellipsis if the name is still too long.
fn wrap_label(name: &str, max_cols: usize, max_lines: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if max_cols == 0 || max_lines == 0 { return vec![]; }
    if name.is_empty() { return vec![String::new()]; }

    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    let chars: Vec<char> = name.chars().collect();
    let total = chars.len();
    let mut i = 0;

    while i < total && lines.len() < max_lines {
        let ch = chars[i];
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        let is_last_line = lines.len() == max_lines - 1;

        if line_w + cw > max_cols {
            if is_last_line && i < total {
                // Truncate with ellipsis: replace last char with '…'
                while line_w + 1 > max_cols.saturating_sub(1) && !line.is_empty() {
                    if let Some(last) = line.pop() {
                        line_w -= UnicodeWidthChar::width(last).unwrap_or(1);
                    }
                }
                line.push('\u{2026}');
                lines.push(line);
                return lines;
            }
            lines.push(line.clone());
            line.clear();
            line_w = 0;
        }

        if line_w + cw <= max_cols {
            line.push(ch);
            line_w += cw;
            i += 1;
        }
    }

    if !line.is_empty() && lines.len() < max_lines {
        lines.push(line);
    }
    lines
}

/// Pad a string to be visually centered in `col_width` display columns.
fn center_line(s: &str, col_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(s);
    if w >= col_width { return s.to_string(); }
    let pad = (col_width - w) / 2;
    format!("{:pad$}{s}", "")
}

fn lines_to_text_centered<'a>(lines: &'a [String], col_width: usize) -> Text<'a> {
    lines.iter()
        .map(|l| Line::raw(center_line(l, col_width)))
        .collect::<Vec<_>>().into()
}

fn lines_to_text_styled_centered<'a>(lines: &'a [String], style: Style, col_width: usize) -> Text<'a> {
    lines.iter()
        .map(|l| Line::styled(center_line(l, col_width), style))
        .collect::<Vec<_>>().into()
}

pub fn file_placeholder_icon_pub(ext: &str) -> &'static str {
    file_placeholder_icon(ext)
}

fn file_placeholder_icon(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "rs"                                           => "\u{e7a8}",
        "py"                                           => "\u{e73c}",
        "js" | "ts"                                    => "\u{e74e}",
        "html" | "htm"                                 => "\u{e736}",
        "css" | "scss"                                 => "\u{e749}",
        "json" | "toml" | "yaml" | "yml"               => "\u{e60b}",
        "md"                                           => "\u{f48a}",
        "pdf"                                          => "\u{f1c1}",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "\u{f03e}",
        "mp4" | "mkv" | "avi" | "mov" | "webm"        => "\u{f03d}",
        "mp3" | "flac" | "ogg" | "wav" | "aac"        => "\u{f001}",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "zst"  => "\u{f1c6}",
        _                                              => "\u{f15b}",
    }
}

