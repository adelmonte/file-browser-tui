use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::lineedit::{self, Edit};
use crate::{
    search::{SearchMode, SearchScope},
    actions,
    app::{App, TOPBAR_H, ViewMode},
    events::{ClickKind, ClipOp, ConfirmDeleteState, DragState, DRAG_THRESHOLD},
    ui::{
        compact::{COMPACT_CELL_H, COMPACT_CELL_W},
        grid::{cell_at_pos, details_row_at_pos},
        menu::{self, MenuKind, MenuOutcome, MenuState, MenuTarget},
        sidebar,
    },
};

fn close_menu(app: &mut App) {
    app.menu = None;
}


/// Clicks on the search bar: focus the field or toggle a chip. Clicking
/// anywhere else moves focus to the results.
fn search_bar_click(app: &mut App, col: u16, row: u16) -> bool {
    let Some(search) = &mut app.search else { return false; };
    let hit = |r: Rect| contains(r, col, row);
    if hit(search.field_rect) {
        search.focused = true;
        return true;
    }
    if let Some(i) = search.mode_rects.iter().position(|r| hit(*r)) {
        let want = if i == 0 { SearchMode::Name } else { SearchMode::Content };
        if search.mode != want { search.toggle_mode(); app.search_changed(); }
        return true;
    }
    if let Some(i) = search.scope_rects.iter().position(|r| hit(*r)) {
        let want = if i == 0 { SearchScope::Here } else { SearchScope::Home };
        if search.scope != want { search.toggle_scope(); app.search_changed(); }
        return true;
    }
    search.focused = false;
    false
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.contains(Position { x: col, y: row })
}

/// Hit-test across all view modes.
pub(crate) fn hit_test(app: &App, col: u16, row: u16) -> Option<usize> {
    let count = app.listing.entries.len();
    match app.view_mode {
        ViewMode::Icons => {
            cell_at_pos((col, row), app.grid_area, (app.scroll_offset, app.scroll_sub), app.grid_cols, count, (app.grid_pitch, app.cell_h))
        }
        ViewMode::Compact => {
            cell_at_pos((col, row), app.grid_area, (app.scroll_offset, 0), app.grid_cols, count, (COMPACT_CELL_W, COMPACT_CELL_H))
        }
        ViewMode::Details => {
            details_row_at_pos(row, app.grid_area, app.scroll_offset, &app.details_rows)
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
        ViewMode::Details => app.grid_area.height.saturating_sub(1) as usize,
        ViewMode::Compact => (app.grid_area.height / COMPACT_CELL_H).max(1) as usize,
        ViewMode::Icons   => (app.grid_area.height / app.cell_h).max(1) as usize,
    }
}

fn total_scroll_rows(app: &App) -> usize {
    let n = app.listing.entries.len();
    match app.view_mode {
        ViewMode::Details => app.details_rows.len().max(n),
        ViewMode::Compact | ViewMode::Icons => {
            let cols = app.grid_cols.max(1) as usize;
            n.div_ceil(cols)
        }
    }
}

pub fn handle_key(app: &mut App, ev: KeyEvent) -> bool {
    // Global quit — works regardless of any modal state
    if ev.modifiers.contains(KeyModifiers::CONTROL) && ev.code == KeyCode::Char('q') {
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

    // Clipboard content naming prompt
    if let Some(ref mut p) = app.paste_name {
        match ev.code {
            KeyCode::Esc => { app.paste_name = None; }
            KeyCode::Enter => { actions::commit_paste_name(app); }
            _ => {
                if lineedit::edit(&mut p.input, &mut p.cursor_pos, ev) == Edit::Changed {
                    p.invalid = false;
                }
            }
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
            _ => {
                if lineedit::edit(&mut rename.input, &mut rename.cursor_pos, ev) == Edit::Changed {
                    rename.invalid = false;
                }
            }
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
                    app.path_bar_cursor = app.path_bar_input.len();
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
            _ => {
                if lineedit::edit(&mut app.path_bar_input, &mut app.path_bar_cursor, ev) == Edit::Changed {
                    app.update_path_completions();
                }
            }
        }
        return false;
    }

    // Search field focused: typing edits the query
    if let Some(search) = &mut app.search
        && search.focused
    {
        match ev.code {
            KeyCode::Esc => app.close_search(),
            KeyCode::Enter | KeyCode::Down => {
                search.focused = false;
                if app.selection.is_none() && !app.listing.entries.is_empty() {
                    app.selection = Some(0);
                }
            }
            KeyCode::Tab => { search.toggle_mode(); app.search_changed(); }
            KeyCode::BackTab => { search.toggle_scope(); app.search_changed(); }
            _ => {
                if lineedit::edit(&mut search.query, &mut search.cursor, ev) == Edit::Changed {
                    app.search_changed();
                }
            }
        }
        return false;
    }

    // Normal navigation. Modifier combos must precede their plain-key arms.
    match ev.code {
        KeyCode::Char('f') if ev.modifiers.contains(KeyModifiers::CONTROL) => app.open_search(),
        KeyCode::Char('f') if ev.modifiers.contains(KeyModifiers::ALT) =>
            actions::execute(app, crate::ui::menu::MenuAction::NewFolder, crate::ui::menu::MenuTarget::Empty),
        KeyCode::Char('a') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = app.listing.entries.len();
            if n > 0 {
                app.multi_selection = (0..n).collect();
                app.selection = Some(app.selection.unwrap_or(0).min(n - 1));
            }
        }
        KeyCode::Esc if app.search.is_some() => app.close_search(),
        KeyCode::Char('q') | KeyCode::Char('Q')
            if ev.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
        {
            return true;
        }
        KeyCode::Esc => {
            app.selection = None;
            app.multi_selection.clear();
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
        KeyCode::Left | KeyCode::Right if app.view_mode == ViewMode::Details
            && app.tree_key(ev.code == KeyCode::Right) => {}
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
        KeyCode::Char('.') => app.toggle_show_hidden(),

        // Clipboard
        KeyCode::Char('c') => actions::clip_selection(app, ClipOp::Copy),
        KeyCode::Char('x') => actions::clip_selection(app, ClipOp::Cut),
        // Dolphin's Create Folder (F10, and Alt+F in this user's dolphinui.rc)
        KeyCode::F(10) => actions::execute(app, crate::ui::menu::MenuAction::NewFolder, crate::ui::menu::MenuTarget::Empty),
        KeyCode::Char('v' | 'p') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
            let cwd = app.cwd.clone();
            actions::paste_clipboard(app, &cwd);
        }
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

            if app.menu.is_none() && crate::ui::scrollbar::hit(app, col, row) {
                app.scrollbar_dragging = true;
                crate::ui::scrollbar::scroll_to(app, row);
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
            if search_bar_click(app, col, row) { return; }
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
                    // Opens on release, unless it turns into a reorder drag
                    crate::events::SidebarTarget::Place(i) => {
                        app.place_drag = Some(crate::events::PlaceDrag { from: i, origin_row: row, active: false, target: None });
                        None
                    }
                    crate::events::SidebarTarget::Device(i) =>
                        app.devices.get(i).map(|d| std::path::PathBuf::from(&d.path)),
                    crate::events::SidebarTarget::Phone(i) => {
                        app.open_phone(i);
                        None
                    }
                };
                if let Some(path) = path
                    && path.exists() { app.navigate_to(path); }
                return;
            }
            if sidebar::contains(app, col, row) { return; }

            // Details headers sort; clicking the active one flips direction
            if app.view_mode == ViewMode::Details
                && let Some(&(_, field)) = app.details_headers.iter().find(|(r, _)| contains(*r, col, row))
            {
                if app.sort.field == field {
                    app.set_sort_ascending(!app.sort.ascending);
                } else {
                    app.set_sort_field(field);
                }
                return;
            }

            // Details tree: the arrow left of a folder's icon expands it
            if app.view_mode == ViewMode::Details
                && let Some(idx) = hit_test(app, col, row)
                && let Some(e) = app.listing.entries.get(idx)
                && e.is_dir
            {
                let x = crate::ui::details::expander_x(app.grid_area, e.depth);
                if col >= x && col < x + 2 {
                    app.toggle_expand(idx);
                    return;
                }
            }

            // Grid
            if let Some(idx) = hit_test(app, col, row) {
                let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
                // Shift+Click selects the range from the anchor; with Ctrl
                // it's added to the selection
                if ev.modifiers.contains(KeyModifiers::SHIFT) {
                    let anchor = app.selection_anchor.or(app.selection).unwrap_or(idx);
                    if !ctrl {
                        app.multi_selection.clear();
                    }
                    app.multi_selection.extend(anchor.min(idx)..=anchor.max(idx));
                    app.selection = Some(idx);
                    app.selection_anchor = Some(anchor);
                    return;
                }
                // Ctrl+Click toggles individual items in multi-selection
                if ctrl {
                    // The single selection so far becomes part of the set
                    if app.multi_selection.is_empty()
                        && let Some(s) = app.selection.filter(|&s| s != idx)
                    {
                        app.multi_selection.insert(s);
                    }
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
                // Empty space: start a rubber band (Ctrl adds to the selection)
                let keep = ev.modifiers.contains(KeyModifiers::CONTROL);
                let base = if keep {
                    let mut b = app.multi_selection.clone();
                    b.extend(app.selection);
                    b
                } else {
                    app.selection = None;
                    app.multi_selection.clear();
                    app.selection_anchor = None;
                    Default::default()
                };
                app.drag_state = DragState::Idle;
                if app.main_area.contains(ratatui::layout::Position { x: col, y: row }) {
                    app.band = Some(((col, row), (col, row), base));
                }
            }
        }

        MouseEventKind::Drag(MouseButton::Left) => {
            if app.scrollbar_dragging {
                crate::ui::scrollbar::scroll_to(app, row);
                return;
            }
            if app.band.is_some() {
                update_band(app, col, row);
                return;
            }

            // Sidebar resize drag
            if app.sidebar_dragging {
                app.sidebar_w = col.clamp(8, 50) + 1;
                return;
            }

            // Reordering places
            if let Some(mut drag) = app.place_drag {
                if row != drag.origin_row {
                    drag.active = true;
                }
                if drag.active {
                    drag.target = match sidebar::target_at(app, col, row) {
                        Some(crate::events::SidebarTarget::Place(j)) => Some(j),
                        _ => drag.target,
                    };
                }
                app.place_drag = Some(drag);
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
            if std::mem::take(&mut app.scrollbar_dragging) || app.band.take().is_some() {
                return;
            }
            if let Some(drag) = app.place_drag.take() {
                if !drag.active {
                    if let Some(p) = app.places.get(drag.from).map(|p| std::path::PathBuf::from(&p.path))
                        && p.exists()
                    {
                        app.navigate_to(p);
                    }
                } else if let Some(to) = drag.target {
                    app.move_place(drag.from, to);
                }
                return;
            }
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
                app.user_actions = crate::user_actions::load();
                app.open_with = match target {
                    MenuTarget::Entry(idx) => app.listing.entries.get(idx).map(|e| {
                        let mime = if e.is_dir {
                            "inode/directory".to_string()
                        } else {
                            e.mime_type.clone().or_else(|| crate::mime::detect(&e.path))
                                .unwrap_or_else(|| "application/octet-stream".into())
                        };
                        let (default, others) = crate::apps::apps_for(&mime);
                        default.into_iter().chain(others).collect()
                    }).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let items = menu::build_context_menu(app, target);
                if !items.is_empty() {
                    app.menu = Some(MenuState::new(MenuKind::Context, target, items, (col, row)));
                }
            }
        }

        MouseEventKind::Moved => {
            app.mouse_pos = Some((col, row));
            if let Some(ref mut m) = app.menu {
                menu::handle_menu_hover(m, col, row);
            }
        }

        MouseEventKind::ScrollDown => {
            if app.menu.is_some() { return; }
            if app.view_mode == ViewMode::Icons {
                app.scroll_by(app.cell_h as f32 * 0.5);
                return;
            }
            let vis = visible_rows(app);
            let total = total_scroll_rows(app);
            if app.scroll_offset + vis < total {
                app.scroll_offset += 1;
            }
        }

        MouseEventKind::ScrollUp => {
            if app.menu.is_some() { return; }
            if app.view_mode == ViewMode::Icons {
                app.scroll_by(-(app.cell_h as f32 * 0.5));
                return;
            }
            if app.scroll_offset > 0 {
                app.scroll_offset -= 1;
            }
        }

        _ => {}
    }
}

/// Move the rubber band's corner and select what it touches.
fn update_band(app: &mut App, col: u16, row: u16) {
    let Some((anchor, _, base)) = app.band.take() else { return; };
    let area = app.grid_area;
    let cur = (col.clamp(area.x, area.right().saturating_sub(1)), row.clamp(area.y, area.bottom().saturating_sub(1)));
    let r = crate::ui::band_rect(anchor, cur);
    let hits: Vec<usize> = app.item_rects.iter()
        .filter(|(_, ir)| ir.intersects(r))
        .map(|(i, _)| *i)
        .collect();
    app.multi_selection = base.iter().copied().chain(hits.iter().copied()).collect();
    app.selection = hits.last().copied().or(app.selection.filter(|s| base.contains(s)));
    if app.multi_selection.len() == 1 {
        // A single item is a plain selection
        app.multi_selection.clear();
    }
    app.band = Some((anchor, cur, base));
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
            crate::events::SidebarTarget::Phone(_) => MenuTarget::SidebarEmpty,
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
    } else if contains(app.topbar_search_rect, col, row) {
        if app.search.is_some() { app.close_search(); } else { app.open_search(); }
    } else if let Some(path) = app.topbar_crumbs.iter()
        .find(|(r, _)| contains(*r, col, row))
        .map(|(_, p)| p.clone())
    {
        if path != app.cwd { app.navigate_to(path); }
    } else if contains(app.topbar_path_rect, col, row) {
        app.start_path_edit();
    }
}

