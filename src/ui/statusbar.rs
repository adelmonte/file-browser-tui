use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::App;
use crate::ui::{details::format_size, theme};

pub fn render_statusbar(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Block::default().style(Style::default().bg(theme::BAR_BG)), area);

    let muted = Style::default().fg(theme::MUTED);
    let sep = || Span::styled("  ·  ", muted);
    let entries = &app.listing.entries;

    let mut left: Vec<Span> = vec![Span::raw(" ")];
    let selected: Vec<usize> = if !app.multi_selection.is_empty() {
        app.multi_selection.iter().copied().collect()
    } else {
        app.selection.into_iter().collect()
    };

    match selected.as_slice() {
        [] => {
            let dirs = entries.iter().filter(|e| e.is_dir).count();
            let files = entries.len() - dirs;
            left.push(Span::raw(plural(entries.len(), "item")));
            if !entries.is_empty() {
                left.push(Span::styled(
                    format!(" ({}, {})", plural(dirs, "folder"), plural(files, "file")),
                    muted,
                ));
            }
        }
        [i] => {
            if let Some(e) = entries.get(*i) {
                left.push(Span::raw(format!("\u{201c}{}\u{201d}", e.name)));
                if !e.is_dir {
                    left.push(sep());
                    left.push(Span::raw(format_size(e.size, e.is_dir)));
                }
                if let Some(mime) = e.mime_type.as_deref() {
                    left.push(sep());
                    left.push(Span::styled(mime.to_string(), muted));
                }
            }
        }
        many => {
            let total: u64 = many.iter()
                .filter_map(|&i| entries.get(i))
                .filter_map(|e| e.size)
                .sum();
            left.push(Span::raw(format!("{} selected", many.len())));
            left.push(sep());
            left.push(Span::raw(format_size(Some(total), false)));
        }
    }

    let mut right: Vec<Span> = Vec::new();
    if app.show_hidden {
        right.push(Span::styled("hidden files shown", muted));
        right.push(sep());
    }
    if let Some(free) = app.free_space {
        right.push(Span::raw(format!("{} free", format_size(Some(free), false))));
    }
    right.push(Span::raw(" "));

    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}
