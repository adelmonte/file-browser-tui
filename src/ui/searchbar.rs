use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::App;
use crate::search::{SearchMode, SearchScope};
use crate::ui::{theme, underlay};

/// One-row search bar under the toolbar:
///  <icon> query█             Name  Content  │  This Folder  Home
pub fn render_searchbar(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(search) = &mut app.search else { return; };

    let chips = [
        ("Name", search.mode == SearchMode::Name),
        ("Content", search.mode == SearchMode::Content),
        ("This Folder", search.scope == SearchScope::Here),
        ("Home", search.scope == SearchScope::Home),
    ];

    // Lay chips out from the right edge, recording their rects for clicks:
    // rounded boxes like the field, the chosen ones filled
    const PAD: u16 = 2;
    const GAP: u16 = 1;
    let sep_w = 5;
    let chips_w: u16 = chips.iter().map(|(l, _)| l.chars().count() as u16 + 2 * PAD).sum::<u16>()
        + 2 * GAP + sep_w + 3;
    let chips_x = area.right().saturating_sub(chips_w);
    let mut x = chips_x + 1;
    let mut placed = Vec::new();
    for (i, (label, on)) in chips.iter().enumerate() {
        if i == 2 {
            f.render_widget(
                Paragraph::new(Span::styled("\u{2502}", Style::default().fg(theme::border()))),
                Rect { x: x + sep_w / 2, y: area.y, width: 1, height: 1 },
            );
            x += sep_w;
        } else if i > 0 {
            x += GAP;
        }
        let w = label.chars().count() as u16 + 2 * PAD;
        let rect = Rect { x, y: area.y, width: w, height: 1 };
        if i < 2 { search.mode_rects[i] = rect; } else { search.scope_rects[i - 2] = rect; }
        placed.push((rect, *label, *on));
        x += w;
    }

    // Query field fills the rest, a recessed field like the path bar
    let outer = Rect { x: area.x + 1, y: area.y, width: chips_x.saturating_sub(area.x + 2), height: 1 };
    search.field_rect = outer;
    let field = Rect { x: outer.x + 1, width: outer.width.saturating_sub(2), ..outer };
    // Laid out like the location bar: a 2-column icon, a space, the text
    let mut line = Vec::new();
    if search.focused {
        line.extend(crate::lineedit::spans(&search.query, search.cursor, Style::default().fg(theme::text_strong())));
    } else {
        line.push(Span::styled(search.query.clone(), Style::default().fg(theme::text_strong())));
    }
    if search.query.is_empty() && !search.focused {
        line.push(Span::styled("Search\u{2026}", Style::default().fg(theme::muted())));
    } else if search.query.is_empty() {
        let hint = match search.mode {
            SearchMode::Name => "file name",
            SearchMode::Content => "text inside files",
        };
        line.push(Span::styled(format!("{hint} (Tab: name/content, Shift+Tab: scope)"), Style::default().fg(theme::muted())));
    }
    let field_style = if search.focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    // A rounded box a little taller than the text row (Kitty), else cell bg
    if !underlay::field(app, 1, outer, theme::get().input) {
        f.render_widget(Block::default().style(Style::default().bg(theme::input())), outer);
    }
    for (i, (rect, label, on)) in placed.into_iter().enumerate() {
        let (fill, style) = if on {
            (Some(theme::get().selection), Style::default().fg(theme::text_strong()))
        } else {
            (None, Style::default().fg(theme::text()))
        };
        crate::ui::pill(f, app, 20 + i as u32, rect, label, fill, style);
    }
    let icon_rect = Rect { width: 2.min(field.width), ..field };
    let text_rect = Rect { x: field.x + 3, width: field.width.saturating_sub(3), ..field };
    f.render_widget(Paragraph::new(Line::from(line)).style(field_style), text_rect);
    // The glyph stands in until (or unless) the theme's icon loads over it
    f.render_widget(Paragraph::new(Span::styled("\u{f002}", Style::default().fg(theme::accent()))), icon_rect);
    crate::ui::sidebar::draw_icon(f, app, "edit-find", icon_rect, None);
}
