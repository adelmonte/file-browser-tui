use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::app::App;
use crate::ui::{theme, underlay};

/// Rows the picker's bar takes above the status bar.
pub const HEIGHT: u16 = 3;

/// The picker's bar, like a file dialog's bottom row:
///  Name  [report.pdf          ]   [ PDF Files ▾ ]   [ Cancel ] [ Save ]
pub fn render_pickbar(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(p) = &app.pick else { return; };
    let y = area.y + 1;
    // Buttons the same width, so the pair sits evenly at the right
    let accept = p.accept_label();
    let filter_label = format!("{}  \u{25be}", p.filter_label());
    let has_filter = !(p.req.filters.is_empty() || p.folders_only());
    let button_w = accept.chars().count().max("Cancel".len()) as u16 + 6;
    let cancel_rect = Rect { x: area.right().saturating_sub(2 + button_w), y, width: button_w, height: 1 };
    let accept_rect = Rect { x: cancel_rect.x.saturating_sub(2 + button_w), y, width: button_w, height: 1 };
    let mut right_edge = accept_rect.x;
    let filter_rect = if has_filter {
        let w = (filter_label.chars().count() as u16 + 4).min(area.width / 3);
        let r = Rect { x: right_edge.saturating_sub(3 + w), y, width: w, height: 1 };
        right_edge = r.x;
        r
    } else {
        Rect::default()
    };
    let input = theme::get().input;
    if has_filter {
        crate::ui::pill(f, app, 26, filter_rect, &filter_label, Some(input), Style::default().fg(theme::text()));
    }
    crate::ui::pill(f, app, 24, accept_rect, &accept, Some(theme::get().selection), Style::default().fg(theme::text_strong()).add_modifier(Modifier::BOLD));
    crate::ui::pill(f, app, 25, cancel_rect, "Cancel", Some(input), Style::default().fg(theme::text()));
    let Some(p) = &app.pick else { return; };

    let left = area.x + 2;
    let name_rect = if p.has_name_field() {
        let label = "Name ";
        f.render_widget(Paragraph::new(Span::styled(label, Style::default().fg(theme::muted()))), Rect { x: left, y, width: label.len() as u16, height: 1 });
        let x = left + label.len() as u16;
        Rect { x, y, width: right_edge.saturating_sub(x + 3), height: 1 }
    } else {
        Rect::default()
    };

    if name_rect.width > 2 {
        let style = Style::default().fg(theme::text_strong());
        let line = if p.name_focused {
            crate::lineedit::spans(&p.name, p.cursor, style)
        } else {
            vec![Span::styled(p.name.clone(), style)]
        };
        let focused = p.name_focused;
        if !underlay::field(app, 11, name_rect, theme::get().input) {
            f.render_widget(Block::default().style(Style::default().bg(theme::input())), name_rect);
        }
        let text = Rect { x: name_rect.x + 1, width: name_rect.width - 2, ..name_rect };
        let text_style = if focused { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
        f.render_widget(Paragraph::new(Line::from(line)).style(text_style), text);
    } else if !p.has_name_field() {
        // Opening: what Enter would pick
        let hint = what_picks(app);
        let w = right_edge.saturating_sub(left + 2);
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(theme::muted()))), Rect { x: left, y, width: w, height: 1 });
    }

    if let Some(p) = &mut app.pick {
        p.name_rect = name_rect;
        p.filter_rect = filter_rect;
        p.accept_rect = accept_rect;
        p.cancel_rect = cancel_rect;
    }
}

fn what_picks(app: &App) -> String {
    let Some(p) = &app.pick else { return String::new(); };
    let n = if app.multi_selection.is_empty() { usize::from(app.selection.is_some()) } else { app.multi_selection.len() };
    let one = app.selection.and_then(|i| app.listing.entries.get(i));
    if p.folders_only() {
        return match one {
            Some(e) if e.is_dir && n == 1 => format!("Folder \u{201c}{}\u{201d}", e.name),
            _ => format!("This folder, \u{201c}{}\u{201d}", app.cwd.file_name().map_or("/".into(), |n| n.to_string_lossy().to_string())),
        };
    }
    match (n, one) {
        (0, _) => if p.req.multiple { "Select one or more files".into() } else { "Select a file".into() },
        (1, Some(e)) => format!("\u{201c}{}\u{201d}", e.name),
        (n, _) => format!("{n} selected"),
    }
}

/// "Replace the existing file?" over the window.
pub fn render_replace_prompt(f: &mut Frame, app: &mut App) {
    let Some(paths) = app.pick.as_ref().and_then(|p| p.confirm_replace.as_ref()) else { return; };
    let existing: Vec<&std::path::PathBuf> = paths.iter().filter(|p| p.exists()).collect();
    let what = match existing.as_slice() {
        [one] => format!("\u{201c}{}\u{201d} already exists.", one.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string())),
        many => format!("{} of the files already exist.", many.len()),
    };
    let term = f.area();
    let w = 52u16.min(term.width);
    let h = 7u16.min(term.height);
    let area = Rect { x: term.width.saturating_sub(w) / 2, y: term.height.saturating_sub(h) / 2, width: w, height: h };
    let bg = Style::default().bg(theme::panel()).fg(theme::text());
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::border()))
            .title(" Replace File ")
            .style(bg),
        area,
    );
    let max = w.saturating_sub(6) as usize;
    let what: String = if what.chars().count() > max { what.chars().take(max.saturating_sub(1)).chain(['\u{2026}']).collect() } else { what };
    f.render_widget(
        Paragraph::new(vec![Line::raw(format!("  {what}")), Line::raw("  Replace it?")]).style(bg),
        Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: 2 },
    );
    let y = area.bottom().saturating_sub(2);
    let ok_label = " Replace ";
    let cancel_label = " Cancel ";
    let cancel = Rect { x: area.right().saturating_sub(3 + cancel_label.len() as u16), y, width: cancel_label.len() as u16, height: 1 };
    let ok = Rect { x: cancel.x.saturating_sub(2 + ok_label.len() as u16), y, width: ok_label.len() as u16, height: 1 };
    f.render_widget(Paragraph::new(ok_label).style(Style::default().bg(theme::selection()).fg(theme::text_strong()).add_modifier(Modifier::BOLD)), ok);
    f.render_widget(Paragraph::new(cancel_label).style(Style::default().bg(theme::input()).fg(theme::text())), cancel);
    app.prompt_buttons = Some((ok, cancel));
}
