//! Emacs/readline-style editing for one-line text fields (path bar, rename,
//! search): C-a/C-e, C-b/C-f, M-b/M-f, C-u/C-k, C-w, M-d, C-d, C-y, C-t,
//! plus Ctrl+Z / Ctrl+Shift+Z to undo and redo.

use std::sync::Mutex;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    style::{Modifier, Style},
    text::Span,
};

/// What a key did to the field.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    /// Text changed.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// Not an editing key; the caller may handle it.
    Ignored,
}

/// Last killed text, shared by all fields (C-y pastes it).
static KILL_RING: Mutex<String> = Mutex::new(String::new());

fn kill(text: &mut String, range: std::ops::Range<usize>) {
    if range.is_empty() {
        return;
    }
    let killed: String = text.drain(range).collect();
    *KILL_RING.lock().unwrap() = killed;
}

fn prev_char(text: &str, i: usize) -> usize {
    text[..i].char_indices().next_back().map_or(0, |(p, _)| p)
}

fn next_char(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Start of the word before `i` (skipping separators first), like M-b.
fn word_start(text: &str, mut i: usize) -> usize {
    while i > 0 && !text[..i].chars().next_back().is_some_and(is_word) {
        i = prev_char(text, i);
    }
    while i > 0 && text[..i].chars().next_back().is_some_and(is_word) {
        i = prev_char(text, i);
    }
    i
}

/// End of the word after `i`, like M-f.
fn word_end(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text[i..].chars().next().is_some_and(is_word) {
        i = next_char(text, i);
    }
    while i < text.len() && text[i..].chars().next().is_some_and(is_word) {
        i = next_char(text, i);
    }
    i
}

/// Apply a key to `text` with the cursor at byte offset `cursor`.
pub fn edit(text: &mut String, cursor: &mut usize, ev: KeyEvent) -> Edit {
    *cursor = (*cursor).min(text.len());
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let c = *cursor;

    match ev.code {
        // Movement
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = text.len(),
        KeyCode::Char('a') if ctrl => *cursor = 0,
        KeyCode::Char('e') if ctrl => *cursor = text.len(),
        KeyCode::Left if ctrl || alt => *cursor = word_start(text, c),
        KeyCode::Right if ctrl || alt => *cursor = word_end(text, c),
        KeyCode::Left => *cursor = prev_char(text, c),
        KeyCode::Right => *cursor = next_char(text, c),
        KeyCode::Char('b') if ctrl => *cursor = prev_char(text, c),
        KeyCode::Char('f') if ctrl => *cursor = next_char(text, c),
        KeyCode::Char('b') if alt => *cursor = word_start(text, c),
        KeyCode::Char('f') if alt => *cursor = word_end(text, c),

        // Deletion
        KeyCode::Char('u') if ctrl => {
            kill(text, 0..c);
            *cursor = 0;
            return Edit::Changed;
        }
        KeyCode::Char('k') if ctrl => {
            kill(text, c..text.len());
            return Edit::Changed;
        }
        KeyCode::Char('w') if ctrl => {
            let s = word_start(text, c);
            kill(text, s..c);
            *cursor = s;
            return Edit::Changed;
        }
        KeyCode::Backspace | KeyCode::Char('h') if alt || (ctrl && ev.code == KeyCode::Backspace) => {
            let s = word_start(text, c);
            kill(text, s..c);
            *cursor = s;
            return Edit::Changed;
        }
        KeyCode::Char('d') if alt => {
            let e = word_end(text, c);
            kill(text, c..e);
            return Edit::Changed;
        }
        KeyCode::Backspace | KeyCode::Char('h') if ctrl || ev.code == KeyCode::Backspace => {
            if c == 0 {
                return Edit::Moved;
            }
            let p = prev_char(text, c);
            text.drain(p..c);
            *cursor = p;
            return Edit::Changed;
        }
        KeyCode::Delete | KeyCode::Char('d') if ctrl || ev.code == KeyCode::Delete => {
            let n = next_char(text, c);
            text.drain(c..n);
            return Edit::Changed;
        }

        // Yank and transpose
        KeyCode::Char('y') if ctrl => {
            let yank = KILL_RING.lock().unwrap().clone();
            text.insert_str(c, &yank);
            *cursor = c + yank.len();
            return Edit::Changed;
        }
        KeyCode::Char('t') if ctrl => {
            // Swap the two chars before the cursor (or around it mid-line)
            let end = if c < text.len() { next_char(text, c) } else { c };
            let mid = prev_char(text, end);
            let start = prev_char(text, mid);
            if start == mid {
                return Edit::Moved;
            }
            let (a, b) = (text[start..mid].to_string(), text[mid..end].to_string());
            text.replace_range(start..end, &(b + &a));
            *cursor = end;
            return Edit::Changed;
        }

        KeyCode::Char(ch) if !ctrl && !alt => {
            text.insert(c, ch);
            *cursor = c + ch.len_utf8();
            return Edit::Changed;
        }
        _ => return Edit::Ignored,
    }
    Edit::Moved
}

/// Undo history of the field being edited. A field opening with other text
/// than the history last saw starts a new one, so undo never reaches into
/// an earlier field's typing.
struct History {
    field: &'static str,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// The text after the last recorded change.
    last: String,
    /// The last change was typing a character: the next one joins it.
    typing: bool,
}

static HISTORY: Mutex<Option<History>> = Mutex::new(None);

/// Drop the undo history (a field opening afresh).
pub fn forget() {
    *HISTORY.lock().unwrap() = None;
}

/// Note a change to `field` from `before` (text, cursor) to `after`.
/// Consecutive typing is one step, ended by a `/` or space.
pub fn record(field: &'static str, before: (String, usize), after: &str, typed: Option<char>) {
    if before.0 == after {
        return;
    }
    let mut guard = HISTORY.lock().unwrap();
    let h = guard.get_or_insert_with(|| History { field, undo: Vec::new(), redo: Vec::new(), last: String::new(), typing: false });
    if h.field != field || h.last != before.0 {
        *h = History { field, undo: Vec::new(), redo: Vec::new(), last: String::new(), typing: false };
    }
    if !(h.typing && typed.is_some()) {
        h.undo.push(before);
    }
    h.redo.clear();
    h.typing = typed.is_some_and(|c| c != '/' && c != ' ');
    h.last = after.to_string();
}

/// Ctrl+Z (undo) or Ctrl+Shift+Z (redo) on `field`; true if it was one of
/// those keys.
pub fn undo_key(field: &'static str, text: &mut String, cursor: &mut usize, ev: KeyEvent) -> bool {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let redo = match ev.code {
        KeyCode::Char('z') if ctrl => shift,
        KeyCode::Char('Z') if ctrl => true,
        _ => return false,
    };
    let mut guard = HISTORY.lock().unwrap();
    let Some(h) = guard.as_mut().filter(|h| h.field == field && h.last == *text) else { return true; };
    let (from, to) = if redo { (&mut h.redo, &mut h.undo) } else { (&mut h.undo, &mut h.redo) };
    if let Some((t, c)) = from.pop() {
        to.push((std::mem::replace(text, t), *cursor));
        *cursor = c.min(text.len());
        h.last = text.clone();
        h.typing = false;
    }
    true
}

/// `edit`, with undo: Ctrl+Z and Ctrl+Shift+Z, and changes recorded.
pub fn edit_undoable(field: &'static str, text: &mut String, cursor: &mut usize, ev: KeyEvent) -> Edit {
    if undo_key(field, text, cursor, ev) {
        return Edit::Changed;
    }
    let before = (text.clone(), *cursor);
    let result = edit(text, cursor, ev);
    if result == Edit::Changed {
        let typed = match ev.code {
            KeyCode::Char(c) if !ev.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => Some(c),
            _ => None,
        };
        record(field, before, text, typed);
    }
    result
}

/// The text as spans with a block cursor at `cursor`.
/// Last keypress/click, so text cursors stay solid while typing and blink
/// when idle (milliseconds since START).
static LAST_INPUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

pub fn bump_cursor() {
    LAST_INPUT.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// Text cursors blink at Qt's default rate (on/off 500 ms each).
pub fn cursor_visible() -> bool {
    (now_ms().saturating_sub(LAST_INPUT.load(std::sync::atomic::Ordering::Relaxed)) / 500).is_multiple_of(2)
}

/// The thin bar cursor, or an invisible same-width stand-in.
pub fn bar_cursor() -> char {
    if cursor_visible() { '\u{258f}' } else { '\u{2800}' }
}

/// `text` with the blinking bar cursor at byte offset `cursor`, the same
/// cursor as every other text field.
pub fn spans(text: &str, cursor: usize, style: Style) -> Vec<Span<'static>> {
    let cursor = cursor.min(text.len());
    let (before, after) = text.split_at(cursor);
    vec![
        Span::styled(before.to_string(), style),
        Span::styled(bar_cursor().to_string(), style.remove_modifier(Modifier::BOLD)),
        Span::styled(after.to_string(), style),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn emacs_keys() {
        let (ctrl, alt, none) = (KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::NONE);
        let mut t = "/home/user/Documents".to_string();
        let mut c = t.len();
        edit(&mut t, &mut c, key(KeyCode::Char('w'), ctrl));
        assert_eq!((t.as_str(), c), ("/home/user/", 11));
        edit(&mut t, &mut c, key(KeyCode::Char('b'), alt));
        assert_eq!(c, 6);
        edit(&mut t, &mut c, key(KeyCode::Char('k'), ctrl));
        assert_eq!(t, "/home/");
        edit(&mut t, &mut c, key(KeyCode::Char('y'), ctrl));
        assert_eq!(t, "/home/user/");
        edit(&mut t, &mut c, key(KeyCode::Char('a'), ctrl));
        edit(&mut t, &mut c, key(KeyCode::Char('x'), none));
        assert_eq!((t.as_str(), c), ("x/home/user/", 1));
        edit(&mut t, &mut c, key(KeyCode::Char('e'), ctrl));
        edit(&mut t, &mut c, key(KeyCode::Char('u'), ctrl));
        assert_eq!((t.as_str(), c), ("", 0));
    }

    #[test]
    fn undo_and_redo() {
        let (ctrl, none) = (KeyModifiers::CONTROL, KeyModifiers::NONE);
        let mut t = "/home/".to_string();
        let mut c = t.len();
        for ch in "me/Docs".chars() {
            edit_undoable("test", &mut t, &mut c, key(KeyCode::Char(ch), none));
        }
        edit_undoable("test", &mut t, &mut c, key(KeyCode::Char('w'), ctrl));
        assert_eq!(t, "/home/me/");
        let z = key(KeyCode::Char('z'), ctrl);
        edit_undoable("test", &mut t, &mut c, z);
        assert_eq!(t, "/home/me/Docs");
        // Typing is undone a word at a time, each ended by its slash
        edit_undoable("test", &mut t, &mut c, z);
        assert_eq!(t, "/home/me/");
        edit_undoable("test", &mut t, &mut c, z);
        assert_eq!((t.as_str(), c), ("/home/", 6));
        edit_undoable("test", &mut t, &mut c, key(KeyCode::Char('z'), ctrl | KeyModifiers::SHIFT));
        assert_eq!(t, "/home/me/");
        // Another field (or this one reopened with other text): no history
        let mut other = "notes.txt".to_string();
        let mut oc = 0;
        edit_undoable("test", &mut other, &mut oc, z);
        assert_eq!(other, "notes.txt");
    }
}
