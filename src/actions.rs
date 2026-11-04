use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{
    app::App,
    config::PlaceConfig,
    events::{ClipOp, Clipboard, RenameState},
    places::PlaceEntry,
    ui::menu::{MenuAction, MenuTarget},
    ui::properties::PropertiesState,
};

/// Execute a menu action against its target.
pub fn execute(app: &mut App, action: MenuAction, target: MenuTarget) {
    match action {
        MenuAction::None => {}
        MenuAction::Drop(op) => crate::dnd::perform(app, op),

        MenuAction::Open => {
            match target {
                MenuTarget::Entry(idx) => {
                    app.selection = Some(idx);
                    app.enter_selected();
                }
                _ => {
                    if let Some(path) = target_path(app, target)
                        && path.is_dir() { app.navigate_to(path); }
                }
            }
        }

        MenuAction::OpenWith(i) => {
            if let Some(desktop_app) = app.open_with.get(i).cloned() {
                crate::apps::launch(&desktop_app, &action_paths(app, target), &terminal_bin(app));
            }
        }

        MenuAction::OpenTerminal => {
            if let Some(dir) = target_dir(app, target) {
                open_terminal(app, &dir);
            }
        }

        MenuAction::Rename => {
            if let MenuTarget::Entry(idx) = target {
                start_rename(app, idx);
            }
        }

        MenuAction::Cut => clip_target(app, target, ClipOp::Cut),
        MenuAction::Copy => clip_target(app, target, ClipOp::Copy),

        MenuAction::CopyLocation => {
            if let Some(path) = target_path(app, target) {
                copy_to_clipboard(&path.to_string_lossy());
            }
        }

        MenuAction::Paste => {
            if let Some(dest) = target_dir(app, target) {
                paste_clipboard(app, &dest);
            }
        }

        MenuAction::Trash => {
            if let Some(path) = target_path(app, target) {
                spawn_detached("gio", &["trash".as_ref(), path.as_os_str()]);
                app.selection = None;
            }
        }

        MenuAction::EmptyTrash => {
            spawn_detached("gio", &["trash".as_ref(), "--empty".as_ref()]);
        }

        MenuAction::NewFolder => create_new(app, true),
        MenuAction::NewFile => create_new(app, false),

        MenuAction::CompressTarGz => compress(app, target, true),
        MenuAction::CompressZip => compress(app, target, false),

        MenuAction::Properties => {
            if let Some(path) = target_path(app, target) {
                app.properties = Some(PropertiesState::new(&path));
            }
        }

        MenuAction::AddToPlaces => add_to_places(app),
        MenuAction::RemovePlace => {
            if let MenuTarget::Place(idx) = target {
                remove_place(app, idx);
            }
        }

        MenuAction::SetView(v) => app.set_view_mode(v),
        MenuAction::SetSortField(f) => app.set_sort_field(f),
        MenuAction::SetAscending(asc) => app.set_sort_ascending(asc),
        MenuAction::ToggleFoldersFirst => app.toggle_folders_first(),
        MenuAction::ToggleShowHidden => app.toggle_show_hidden(),

        MenuAction::Custom(idx) => run_custom(app, idx, target),

        MenuAction::UserAction(gi, ai) => {
            let Some(action) = app.user_actions.get(gi).and_then(|g| g.actions.get(ai)) else { return; };
            crate::user_actions::run(action, &action_paths(app, target), &app.cwd);
        }
        MenuAction::SendToDevice(i) => {
            if let Some(device) = app.kdeconnect_devices.get(i) {
                crate::user_actions::kdeconnect_share(device, &action_paths(app, target));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

fn target_path(app: &App, target: MenuTarget) -> Option<PathBuf> {
    match target {
        MenuTarget::Entry(idx) => app.listing.entries.get(idx).map(|e| e.path.clone()),
        MenuTarget::Empty | MenuTarget::SidebarEmpty => Some(app.cwd.clone()),
        MenuTarget::Place(idx) => app.places.get(idx).map(|p| PathBuf::from(&p.path)),
        MenuTarget::Device(idx) => app.devices.get(idx).map(|d| PathBuf::from(&d.path)),
    }
}

/// Files an action applies to: the whole selection when the right-clicked
/// entry is part of it, else just the target.
fn action_paths(app: &App, target: MenuTarget) -> Vec<PathBuf> {
    if let MenuTarget::Entry(idx) = target
        && app.multi_selection.contains(&idx)
    {
        return selected_paths(app);
    }
    target_path(app, target).into_iter().collect()
}

/// Directory the action operates in (the dir itself for dir entries, cwd otherwise).
fn target_dir(app: &App, target: MenuTarget) -> Option<PathBuf> {
    let path = target_path(app, target)?;
    if path.is_dir() { Some(path) } else { path.parent().map(|p| p.to_path_buf()) }
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

fn clip_target(app: &mut App, target: MenuTarget, op: ClipOp) {
    if let Some(path) = target_path(app, target) {
        app.clipboard = Some(Clipboard { paths: vec![path], op });
    }
}

/// Returns the currently selected paths: multi_selection if non-empty, else single selection.
pub fn selected_paths(app: &App) -> Vec<PathBuf> {
    if !app.multi_selection.is_empty() {
        let mut paths: Vec<PathBuf> = app.multi_selection.iter()
            .filter_map(|&i| app.listing.entries.get(i))
            .map(|e| e.path.clone())
            .collect();
        paths.sort();
        paths
    } else {
        app.selection
            .and_then(|i| app.listing.entries.get(i))
            .map(|e| vec![e.path.clone()])
            .unwrap_or_default()
    }
}

pub fn clip_selection(app: &mut App, op: ClipOp) {
    let paths = selected_paths(app);
    if !paths.is_empty() {
        app.clipboard = Some(Clipboard { paths, op });
    }
}

pub fn paste_clipboard(app: &mut App, dest: &Path) {
    let Some(clip) = app.clipboard.clone() else { return; };
    let op = match clip.op {
        ClipOp::Copy => "copy",
        ClipOp::Cut => "move",
    };
    for src in &clip.paths {
        spawn_detached("gio", &[op.as_ref(), src.as_os_str(), dest.as_os_str()]);
    }
    if clip.op == ClipOp::Cut {
        app.clipboard = None;
    }
}

/// Copy text to the system clipboard: OSC 52 (works in Kitty) + wl-copy fallback.
fn copy_to_clipboard(text: &str) {
    let b64 = base64_encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();

    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()
        && let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------------------
// File operations
// ---------------------------------------------------------------------------

fn start_rename(app: &mut App, idx: usize) {
    if let Some(entry) = app.listing.entries.get(idx) {
        app.rename_state = Some(RenameState::new(idx, &entry.name.clone()));
        app.selection = Some(idx);
    }
}

pub fn rename_selection(app: &mut App) {
    if let Some(idx) = app.selection {
        start_rename(app, idx);
    }
}

pub fn trash_selection(app: &mut App) {
    let paths = selected_paths(app);
    for path in paths {
        spawn_detached("gio", &["trash".as_ref(), path.as_os_str()]);
    }
    app.selection = None;
    app.multi_selection.clear();
}

pub fn permanent_delete(app: &mut App) {
    if let Some(cd) = app.confirm_delete.take() {
        for path in &cd.paths {
            if path.is_dir() { let _ = std::fs::remove_dir_all(path); }
            else { let _ = std::fs::remove_file(path); }
        }
        app.selection = None;
        app.multi_selection.clear();
        app.reload_dir();
    }
}

/// Find an unused "New Folder"/"New File (2)" style name in dir.
fn unique_name(dir: &Path, base: &str) -> PathBuf {
    let candidate = dir.join(base);
    if !candidate.exists() {
        return candidate;
    }
    for n in 2.. {
        let candidate = dir.join(format!("{base} ({n})"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn create_new(app: &mut App, is_dir: bool) {
    let base = if is_dir { "New Folder" } else { "New File" };
    let path = unique_name(&app.cwd, base);
    let result = if is_dir {
        std::fs::create_dir(&path)
    } else {
        std::fs::File::create(&path).map(|_| ())
    };
    if let Err(e) = result {
        tracing::error!("create {base}: {e}");
        return;
    }
    app.reload_dir();
    // Select the new item and jump straight into rename
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if let Some(idx) = app.listing.entries.iter().position(|e| e.name == name) {
        app.selection = Some(idx);
        start_rename(app, idx);
    }
}

fn compress(app: &mut App, target: MenuTarget, targz: bool) {
    let Some(src) = target_path(app, target) else { return; };
    let Some(parent) = src.parent().map(|p| p.to_path_buf()) else { return; };
    let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else { return; };

    if targz {
        let out = unique_name(&parent, &format!("{name}.tar.gz"));
        let _ = Command::new("tar")
            .args(["-czf", out.to_string_lossy().as_ref(), "-C", parent.to_string_lossy().as_ref(), &name])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    } else {
        let out = unique_name(&parent, &format!("{name}.zip"));
        let out_name = out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let _ = Command::new("zip")
            .current_dir(&parent)
            .args(["-r", &out_name, &name])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
}

fn terminal_command(app: &App) -> String {
    if !app.config.terminal_command.is_empty() {
        app.config.terminal_command.clone()
    } else {
        std::env::var("TERMINAL").unwrap_or_else(|_| "kitty".into())
    }
}

/// Terminal program for apps that need one (Terminal=true).
fn terminal_bin(app: &App) -> String {
    terminal_command(app).split_whitespace().next().unwrap_or("kitty").to_string()
}

fn open_terminal(app: &App, dir: &Path) {
    let cmd = terminal_command(app);
    let mut parts = cmd.split_whitespace();
    let Some(bin) = parts.next() else { return; };
    let _ = Command::new(bin)
        .args(parts)
        .current_dir(dir)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn();
}

// ---------------------------------------------------------------------------
// Places
// ---------------------------------------------------------------------------

fn add_to_places(app: &mut App) {
    let path = app.cwd.to_string_lossy().to_string();
    if app.places.iter().any(|p| p.path == path) {
        return;
    }
    let name = app.cwd.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "/".into());
    app.places.push(PlaceEntry { name: name.clone(), path: path.clone(), icon: None });
    app.config.places.push(PlaceConfig { name, path, icon: None });
    let _ = app.config.save();
}

fn remove_place(app: &mut App, idx: usize) {
    if idx < app.places.len() {
        let removed = app.places.remove(idx);
        app.config.places.retain(|p| p.path != removed.path);
        let _ = app.config.save();
    }
}

// ---------------------------------------------------------------------------
// Custom config-defined entries
// ---------------------------------------------------------------------------

fn run_custom(app: &App, idx: usize, target: MenuTarget) {
    let Some(entry) = app.config.context_menu.custom.get(idx) else { return; };
    let Some(path) = target_path(app, target) else { return; };
    let dir = if path.is_dir() { path.clone() } else {
        path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| app.cwd.clone())
    };

    let cmd = entry.command
        .replace("%f", &shell_quote(&path.to_string_lossy()))
        .replace("%d", &shell_quote(&dir.to_string_lossy()));

    let _ = Command::new("sh")
        .args(["-c", &cmd])
        .current_dir(&dir)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn();
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn spawn_detached(bin: &str, args: &[&std::ffi::OsStr]) {
    let _ = Command::new(bin)
        .args(args)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn();
}
