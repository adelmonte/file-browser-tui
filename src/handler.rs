use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::{
    actions,
    app::{App, TOPBAR_H, ViewMode},
    events::{ClickKind, ClipOp, ConfirmDeleteState, DragState, DRAG_THRESHOLD},
    ui::{
        grid::{cell_at_pos, details_row_at_pos},
        menu::{self, MenuKind, MenuOutcome, MenuState, MenuTarget},
        sidebar,
    },
};

/// Close the open menu and force a clean Kitty re-render on the next frame.
fn close_menu(app: &mut App) {
    app.menu = None;
    app.kitty_delete_all = true;
}

const COMPACT_CELL_W: u16 = 20;
const COMPACT_CELL_H: u16 = 2;

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.contains(Position { x: col, y: row })
}

/// Hit-test across all view modes.
fn hit_test(app: &App, col: u16, row: u16) -> Option<usize> {
    let count = app.listing.entries.len();
    match app.view_mode {
        ViewMode::Icons => {
            cell_at_pos((col, row), app.grid_area, app.scroll_offset, app.grid_cols, count, (app.cell_w, app.cell_h))
        }
        ViewMode::Compact => {
            cell_at_pos((col, row), app.grid_area, app.scroll_offset, app.grid_cols, count, (COMPACT_CELL_W, COMPACT_CELL_H))
        }
        ViewMode::Details => {
            details_row_at_pos(row, app.grid_area, app.scroll_offset, count)
        }
    }
}

/// Handle a click inside the path completion dropdown. Returns true if consumed.
fn path_completion_hit(app: &mut App, col: u16, row: u16) -> bool {
    if !app.path_bar_editing || app.path_completions.is_empty() { return false; }
    let area = app.completions_rect;
    if !contains(area, col, row) { return false; }
    if row <= area.y || row >= area.y + area.height.saturating_sub(1) { return true; }
    let item_row = (row - area.y - 1) as usize;
    if item_row < app.path_completions.len() {
        let path = app.path_completions[item_row].clone();
        app.cancel_path_edit();
        if path.is_dir() { app.navigate_to(path); }
    }
    true
}

fn visible_rows(app: &App) -> usize {
    match app.view_mode {
        ViewMode::Details => app.main_area.height.saturating_sub(1) as usize,
        ViewMode::Compact => (app.main_area.height / COMPACT_CELL_H).max(1) as usize,
        ViewMode::Icons   => (app.main_area.height / app.cell_h).max(1) as usize,
    }
}

fn total_scroll_rows(app: &App) -> usize {
    let n = app.listing.entries.len();
    match app.view_mode {
        ViewMode::Details => n,
        ViewMode::Compact => {
            let cols = (app.main_area.width / COMPACT_CELL_W).max(1) as usize;
            n.div_ceil(cols)
        }
        ViewMode::Icons => {
            let cols = app.grid_cols.max(1) as usize;
            n.div_ceil(cols)
        }
    }
}

pub fn handle_key(app: &mut App, ev: KeyEvent) -> bool {
    // Global quit — works regardless of any modal state
    if ev.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(ev.code, KeyCode::Char('c') | KeyCode::Char('q'))
    {
        return true;
    }

    // Help overlay captures all keys
    if app.show_help {
        app.show_help = false;
        return false;
    }

    // Properties popup
    if app.properties.is_some() {
        if matches!(ev.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            app.properties = None;
        }
        return false;
    }

    // Permanent-delete confirmation popup
    if app.confirm_delete.is_some() {
        match ev.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                actions::permanent_delete(app);
            }
            _ => {
                app.confirm_delete = None;
            }
        }
        return false;
    }

    // Menu (context menu / view dropdown / sort dropdown)
    if app.menu.is_some() {
        let mut m = app.menu.take().unwrap();
        match menu::handle_menu_key(&mut m, ev.code) {
            MenuOutcome::Close | MenuOutcome::Outside => { close_menu(app); }
            MenuOutcome::Action(action) => {
                close_menu(app);
                actions::execute(app, action, m.target);
            }
            MenuOutcome::Consumed => {
                app.menu = Some(m);
            }
        }
        return false;
    }

    // Rename mode
    if let Some(ref mut rename) = app.rename_state {
        match ev.code {
            KeyCode::Esc => { app.rename_state = None; }
            KeyCode::Enter => { app.commit_rename(); }
            KeyCode::Backspace => { rename.delete_before_cursor(); }
            KeyCode::Left => { rename.move_left(); }
            KeyCode::Right => { rename.move_right(); }
            KeyCode::Char(c) => { rename.insert_char(c); }
            _ => {}
        }
        return false;
    }

    // Path bar editing
    if app.path_bar_editing {
        match ev.code {
            KeyCode::Esc => app.cancel_path_edit(),
            KeyCode::Enter => {
                let target = if let Some(sel) = app.path_completion_sel {
                    app.path_completions.get(sel).cloned()
                } else {
                    Some(std::path::PathBuf::from(app.path_bar_input.trim()))
                };
                app.cancel_path_edit();
                if let Some(path) = target
                    && path.is_dir() { app.navigate_to(path); }
            }
            KeyCode::Tab => {
                let sel = app.path_completion_sel.unwrap_or(0);
                if let Some(p) = app.path_completions.get(sel).cloned() {
                    app.path_bar_input = p.to_string_lossy().to_string() + "/";
                    app.update_path_completions();
                }
            }
            KeyCode::Down => {
                if !app.path_completions.is_empty() {
                    let next = app.path_completion_sel
                        .map(|s| (s + 1).min(app.path_completions.len() - 1))
                        .unwrap_or(0);
                    app.path_completion_sel = Some(next);
                }
            }
            KeyCode::Up => {
                app.path_completion_sel = match app.path_completion_sel {
                    Some(0) | None => None,
                    Some(s) => Some(s - 1),
                };
            }
            KeyCode::Backspace => {
                app.path_bar_input.pop();
                app.update_path_completions();
            }
            KeyCode::Char(c) => {
                app.path_bar_input.push(c);
                app.update_path_completions();
            }
            _ => {}
        }
        return false;
    }

    // Normal navigation. Modifier combos must precede their plain-key arms.
    match ev.code {
        KeyCode::Char('q') | KeyCode::Char('Q')
            if ev.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
        {
            return true;
        }
        KeyCode::Esc => {
            app.selection = None;
            app.drag_state = DragState::Idle;
        }

        // Path bar focus
        KeyCode::Char('l') if ev.modifiers.contains(KeyModifiers::CONTROL) => app.start_path_edit(),
        KeyCode::Char('d') if ev.modifiers.contains(KeyModifiers::ALT) => app.start_path_edit(),
        KeyCode::Char('/') => app.start_path_edit(),

        // Up a dir
        KeyCode::Up if ev.modifiers.contains(KeyModifiers::ALT) => app.navigate_up(),
        KeyCode::Backspace if ev.modifiers.contains(KeyModifiers::SHIFT) => app.navigate_up(),
        // Previous dir (history)
        KeyCode::Backspace => app.navigate_back(),

        // Shift+Arrow: extend multi-selection (must precede plain arrow arms)
        KeyCode::Left  if ev.modifiers.contains(KeyModifiers::SHIFT) => app.extend_selection(-1),
        KeyCode::Right if ev.modifiers.contains(KeyModifiers::SHIFT) => app.extend_selection(1),
        KeyCode::Up    if ev.modifiers.contains(KeyModifiers::SHIFT) => {
            let cols = app.grid_cols.max(1) as i32;
            app.extend_selection(-cols);
        }
        KeyCode::Down  if ev.modifiers.contains(KeyModifiers::SHIFT) => {
            let cols = app.grid_cols.max(1) as i32;
            app.extend_selection(cols);
        }

        // Navigation (plain, clears multi-selection)
        KeyCode::Left  | KeyCode::Char('h') => {
            app.multi_selection.clear();
            app.selection_anchor = None;
            app.move_selection_cols(-1);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.multi_selection.clear();
            app.selection_anchor = None;
            app.move_selection_cols(1);
        }
        KeyCode::Up    | KeyCode::Char('k') => {
            app.multi_selection.clear();
            app.selection_anchor = None;
            app.move_selection_rows(-1);
        }
        KeyCode::Down  | KeyCode::Char('j') => {
            app.multi_selection.clear();
            app.selection_anchor = None;
            app.move_selection_rows(1);
        }
        KeyCode::Enter => app.enter_selected(),

        // Sidebar resize (Alt+[ and Alt+]) — must be before plain [ / ]
        KeyCode::Char('[') if ev.modifiers.contains(KeyModifiers::ALT) => {
            app.sidebar_w = app.sidebar_w.saturating_sub(2).max(8);
        }
        KeyCode::Char(']') if ev.modifiers.contains(KeyModifiers::ALT) => {
            app.sidebar_w = (app.sidebar_w + 2).min(50);
        }

        // History
        KeyCode::Char('[') => app.navigate_back(),
        KeyCode::Char(']') => app.navigate_forward(),

        // Hidden files toggle
        KeyCode::Char('.') => {
            app.show_hidden = !app.show_hidden;
            app.reload_dir();
        }

        // Clipboard
        KeyCode::Char('c') => actions::clip_selection(app, ClipOp::Copy),
        KeyCode::Char('x') => actions::clip_selection(app, ClipOp::Cut),
        KeyCode::Char('p') => {
            let cwd = app.cwd.clone();
            actions::paste_clipboard(app, &cwd);
        }

        // File ops
        KeyCode::F(2) => actions::rename_selection(app),
        KeyCode::Delete if ev.modifiers.contains(KeyModifiers::SHIFT) => {
            let paths = actions::selected_paths(app);
            if !paths.is_empty() {
                let display_name = if paths.len() == 1 {
                    paths[0].file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("this item")
                        .to_string()
                } else {
                    format!("{} items", paths.len())
                };
                app.confirm_delete = Some(ConfirmDeleteState { paths, display_name });
            }
        }
        KeyCode::Delete => actions::trash_selection(app),

        // Reload
        KeyCode::F(5) | KeyCode::Char('r') => app.reload_dir(),

        // Icon size
        KeyCode::Char('=') | KeyCode::Char('+') => app.resize_icons(1),
        KeyCode::Char('-') => app.resize_icons(-1),

        // Help
        KeyCode::Char('?') => { app.show_help = true; }

        _ => {}
    }
    false
}

pub fn handle_mouse(app: &mut App, ev: MouseEvent) {
    let col = ev.column;
    let row = ev.row;

    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Sidebar border drag: clicking on the right edge of the sidebar
            if col == app.sidebar_w.saturating_sub(1) && row >= TOPBAR_H && app.menu.is_none() {
                app.sidebar_dragging = true;
                return;
            }

            // Open menu consumes all clicks (outside click just closes it)
            if app.menu.is_some() {
                let mut m = app.menu.take().unwrap();
                match menu::handle_menu_click(&mut m, col, row) {
                    MenuOutcome::Close | MenuOutcome::Outside => { close_menu(app); }
                    MenuOutcome::Action(action) => {
                        close_menu(app);
                        actions::execute(app, action, m.target);
                    }
                    MenuOutcome::Consumed => {
                        app.menu = Some(m);
                    }
                }
                return;
            }

            if path_completion_hit(app, col, row) { return; }
            if app.path_bar_editing && !contains(app.topbar_path_rect, col, row) {
                app.cancel_path_edit();
            }

            if app.properties.is_some() {
                app.properties = None;
                return;
            }
            if app.show_help {
                app.show_help = false;
                return;
            }

            // Topbar
            if row < TOPBAR_H {
                handle_topbar_click(app, col, row);
                return;
            }

            // Sidebar
            if let Some(target) = sidebar::target_at(app, col, row) {
                let path = match target {
                    crate::events::SidebarTarget::Place(i) =>
                        app.places.get(i).map(|p| std::path::PathBuf::from(&p.path)),
                    crate::events::SidebarTarget::Device(i) =>
                        app.devices.get(i).map(|d| std::path::PathBuf::from(&d.path)),
                };
                if let Some(path) = path
                    && path.exists() { app.navigate_to(path); }
                return;
            }
            if sidebar::contains(app, col, row) { return; }

            // Grid
            if let Some(idx) = hit_test(app, col, row) {
                // Ctrl+Click toggles individual items in multi-selection
                if ev.modifiers.contains(KeyModifiers::CONTROL) {
                    if app.multi_selection.contains(&idx) {
                        app.multi_selection.remove(&idx);
                    } else {
                        app.multi_selection.insert(idx);
                        app.selection = Some(idx);
                        app.selection_anchor = Some(idx);
                    }
                    return;
                }

                match app.click_tracker.record(idx) {
                    ClickKind::Single => {
                        app.multi_selection.clear();
                        app.selection_anchor = Some(idx);
                        app.selection = Some(idx);
                        app.drag_state = DragState::Armed { item_idx: idx, origin: (col, row) };
                    }
                    ClickKind::Double => {
                        app.multi_selection.clear();
                        app.selection = Some(idx);
                        let entry = &app.listing.entries[idx];
                        if entry.is_dir {
                            let path = entry.path.clone();
                            app.navigate_to(path);
                        } else {
                            app.open_selected();
                        }
                    }
                }
            } else {
                app.selection = None;
                app.multi_selection.clear();
                app.selection_anchor = None;
                app.drag_state = DragState::Idle;
            }
        }

        MouseEventKind::Drag(MouseButton::Left) => {
            // Sidebar resize drag
            if app.sidebar_dragging {
                app.sidebar_w = col.clamp(8, 50) + 1;
                return;
            }

            match app.drag_state.clone() {
                DragState::Armed { item_idx, origin } => {
                    let dc = col.abs_diff(origin.0);
                    let dr = row.abs_diff(origin.1);
                    if dc + dr >= DRAG_THRESHOLD {
                        app.drag_state = DragState::Dragging {
                            item_idx,
                            origin,
                            current_pos: (col, row),
                            hover_target: None,
                        };
                    }
                }
                DragState::Dragging { item_idx, origin, .. } => {
                    let hover = hit_test(app, col, row)
                        .filter(|&i| i != item_idx && app.listing.entries.get(i).map(|e| e.is_dir).unwrap_or(false));
                    app.drag_state = DragState::Dragging {
                        item_idx,
                        origin,
                        current_pos: (col, row),
                        hover_target: hover,
                    };
                }
                _ => {}
            }
        }

        MouseEventKind::Up(MouseButton::Left) => {
            app.sidebar_dragging = false;
            if let DragState::Dragging { item_idx, hover_target, .. } = app.drag_state.clone()
                && let Some(dst_idx) = hover_target {
                    let copy = ev.modifiers.contains(KeyModifiers::CONTROL);
                    app.execute_drop(item_idx, dst_idx, copy);
                }
            // Stale ghost placements are cleaned by the post-draw frame diff.
            app.drag_state = DragState::Idle;
        }

        MouseEventKind::Down(MouseButton::Right) => {
            close_menu(app);
            let target = context_target_at(app, col, row);
            if let Some(target) = target {
                if let MenuTarget::Entry(idx) = target {
                    app.selection = Some(idx);
                }
                let items = menu::build_context_menu(app, target);
                if !items.is_empty() {
                    app.menu = Some(MenuState::new(MenuKind::Context, target, items, (col, row)));
                }
            }
        }

        MouseEventKind::Moved => {
            if let Some(ref mut m) = app.menu {
                menu::handle_menu_hover(m, col, row);
            }
        }

        MouseEventKind::ScrollDown => {
            if app.menu.is_some() { return; }
            let vis = visible_rows(app);
            let total = total_scroll_rows(app);
            if app.scroll_offset + vis < total {
                app.scroll_offset += 1;
            }
        }

        MouseEventKind::ScrollUp => {
            if app.menu.is_some() { return; }
            if app.scroll_offset > 0 {
                app.scroll_offset -= 1;
            }
        }

        _ => {}
    }
}

/// What a right-click at this position should open a menu for.
fn context_target_at(app: &App, col: u16, row: u16) -> Option<MenuTarget> {
    if row < TOPBAR_H {
        return None;
    }
    if let Some(target) = sidebar::target_at(app, col, row) {
        return Some(match target {
            crate::events::SidebarTarget::Place(i) => MenuTarget::Place(i),
            crate::events::SidebarTarget::Device(i) => MenuTarget::Device(i),
        });
    }
    if sidebar::contains(app, col, row) {
        return Some(MenuTarget::SidebarEmpty);
    }
    if contains(app.main_area, col, row) {
        return Some(match hit_test(app, col, row) {
            Some(idx) => MenuTarget::Entry(idx),
            None => MenuTarget::Empty,
        });
    }
    None
}

fn handle_topbar_click(app: &mut App, col: u16, row: u16) {
    if contains(app.topbar_back_rect, col, row) {
        app.navigate_back();
    } else if contains(app.topbar_fwd_rect, col, row) {
        app.navigate_forward();
    } else if contains(app.topbar_up_rect, col, row) {
        app.navigate_up();
    } else if contains(app.topbar_view_rect, col, row) {
        let r = app.topbar_view_rect;
        let items = menu::build_view_menu(app);
        app.menu = Some(MenuState::new(MenuKind::View, MenuTarget::Empty, items, (r.x, TOPBAR_H)));
    } else if contains(app.topbar_sort_rect, col, row) {
        let r = app.topbar_sort_rect;
        let items = menu::build_sort_menu(app);
        app.menu = Some(MenuState::new(MenuKind::Sort, MenuTarget::Empty, items, (r.x, TOPBAR_H)));
    } else if contains(app.topbar_path_rect, col, row) {
        app.start_path_edit();
    }
}
