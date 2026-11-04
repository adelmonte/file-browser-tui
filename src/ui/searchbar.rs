use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;
use crate::search::{SearchMode, SearchScope};
use crate::ui::theme;

/// One-row search bar under the toolbar:
///  <icon> query█             Name  Content  │  This Folder  Home
pub fn render_searchbar(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(search) = &mut app.search else { return; };

    let chip = |label: &str, on: bool| {
        let style = if on {
            Style::default().bg(theme::SEL_BG).fg(Color::White)
        } else {
            Style::default().fg(theme::TEXT)
        };
        Span::styled(format!(" {label} "), style)
    };
    let chips = [
        ("Name", search.mode == SearchMode::Name),
        ("Content", search.mode == SearchMode::Content),
        ("This Folder", search.scope == SearchScope::Here),
        ("Home", search.scope == SearchScope::Home),
    ];

    // Lay chips out from the right edge, recording their rects for clicks
    let sep = " \u{2502} ";
    let chips_w: u16 = chips.iter().map(|(l, _)| l.chars().count() as u16 + 2).sum::<u16>()
        + 1 + sep.chars().count() as u16 + 1 + 1;
    let mut x = area.right().saturating_sub(chips_w);
    let mut spans = Vec::new();
    for (i, (label, on)) in chips.iter().enumerate() {
        if i == 2 {
            spans.push(Span::styled(sep, Style::default().fg(theme::BORDER)));
            x += sep.chars().count() as u16;
        } else {
            spans.push(Span::raw(" "));
            x += 1;
        }
        let w = label.chars().count() as u16 + 2;
        let rect = Rect { x, y: area.y, width: w, height: 1 };
        if i < 2 { search.mode_rects[i] = rect; } else { search.scope_rects[i - 2] = rect; }
        spans.push(chip(label, *on));
        x += w;
    }
    let chips_x = area.right().saturating_sub(chips_w);
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x: chips_x, y: area.y, width: area.right() - chips_x, height: 1 },
    );

    // Query field fills the rest, a rounded recessed field like the path bar
    let outer = Rect { x: area.x + 1, y: area.y, width: chips_x.saturating_sub(area.x + 2), height: 1 };
    if outer.width >= 4 {
        let cap = Style::default().fg(theme::FIELD_BG);
        f.render_widget(Paragraph::new(Span::styled("\u{e0b6}", cap)), Rect { width: 1, ..outer });
        f.render_widget(Paragraph::new(Span::styled("\u{e0b4}", cap)), Rect { x: outer.right() - 1, width: 1, ..outer });
    }
    let field = Rect { x: outer.x + 1, width: outer.width.saturating_sub(2), ..outer };
    search.field_rect = field;
    let mut line = vec![Span::styled("\u{f002}  ", Style::default().fg(theme::ACCENT))];
    if search.focused {
        line.extend(crate::lineedit::spans(&search.query, search.cursor, Style::default().fg(Color::White)));
    } else {
        line.push(Span::styled(search.query.clone(), Style::default().fg(Color::White)));
    }
    if search.query.is_empty() && !search.focused {
        line.push(Span::styled("Search\u{2026}", Style::default().fg(theme::MUTED)));
    } else if search.query.is_empty() {
        let hint = match search.mode {
            SearchMode::Name => "file name",
            SearchMode::Content => "text inside files",
        };
        line.push(Span::styled(format!(" {hint}  (Tab: name/content, Shift+Tab: scope)"), Style::default().fg(theme::MUTED)));
    }
    let field_style = if search.focused {
        Style::default().bg(theme::FIELD_BG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(theme::FIELD_BG)
    };
    f.render_widget(Paragraph::new(Line::from(line)).style(field_style), field);
}
