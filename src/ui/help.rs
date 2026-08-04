use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

const HELP_LINES: &[(&str, &str)] = &[
    ("Navigation", ""),
    ("  ←↑↓→ / hjkl", "Move selection"),
    ("  Enter",        "Open file or enter dir"),
    ("  Backspace",    "Previous dir (history)"),
    ("  Shift+Bksp",   "Go up a dir"),
    ("  [ / ]",        "Back / Forward"),
    ("  Ctrl+L, Alt+D, /", "Edit path bar"),
    ("  .",            "Toggle hidden files"),
    ("  r / F5",       "Reload directory"),
    ("", ""),
    ("Files", ""),
    ("  c / x / p",    "Copy / Cut / Paste"),
    ("  F2",           "Rename"),
    ("  Delete",       "Move to trash"),
    ("", ""),
    ("View", ""),
    ("  + / =",        "Larger icons (per-dir)"),
    ("  -",            "Smaller icons (per-dir)"),
    ("  Alt+[ / ]",    "Narrow / widen sidebar"),
    ("", ""),
    ("Mouse", ""),
    ("  Click",        "Select item"),
    ("  Double-click", "Open / enter dir"),
    ("  Right-click",  "Context menu (file/dir/empty/places)"),
    ("  Drag",         "Move file to dir"),
    ("  Ctrl+Drag",    "Copy file to dir"),
    ("  Drag border",  "Resize sidebar"),
    ("", ""),
    ("  q / Ctrl+C",   "Quit"),
    ("  ?",            "Toggle this help"),
];

/// Geometry of the help popup (shared with the occlusion pre-pass).
pub fn help_rect(term: Rect) -> Rect {
    let width: u16 = 52u16.min(term.width);
    let height: u16 = (HELP_LINES.len() as u16 + 2).min(term.height);
    let x = term.width.saturating_sub(width) / 2;
    let y = term.height.saturating_sub(height) / 2;
    Rect { x, y, width, height }
}

pub fn render_help(f: &mut Frame) {
    let area = help_rect(f.area());
    f.render_widget(Clear, area);

    let lines: Vec<Line> = HELP_LINES.iter().map(|(key, desc)| {
        if key.is_empty() {
            Line::raw("")
        } else if desc.is_empty() {
            // Section header
            Line::from(Span::styled(*key, Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)))
        } else {
            Line::from(vec![
                Span::styled(format!("{:<20}", key), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(*desc),
            ])
        }
    }).collect();

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Help (? to close) ")),
        area,
    );
}
