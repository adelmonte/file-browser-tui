mod actions;
mod app;
mod config;
mod dirstate;
mod dnd;
mod events;
mod fileops;
mod fs;
mod handler;
mod icons;
mod keys;
mod launch;
mod apps;
mod archive;
mod check;
mod clipboard;
mod lineedit;
mod mime;
mod pick;
mod places;
mod search;
mod thumbs;
mod ui;
mod user_actions;
mod phone;
mod vfs;

use std::{io::Write, sync::mpsc, time::Duration};

use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal::{SetTitle, supports_keyboard_enhancement}};
use ratatui_image::picker::{Picker, ProtocolType};

use app::App;
use config::Config;

fn main() -> anyhow::Result<()> {
    let launched = std::time::Instant::now();
    match std::env::args().nth(1).as_deref() {
        Some("--check") => std::process::exit(check::run()),
        Some("--kitty") => {
            // Become a kitty window running the app with its options
            use std::os::unix::process::CommandExt;
            let cmd = if std::env::args().nth(2).as_deref() == Some("--pick") {
                let Some(req) = std::env::args_os().nth(3).map(std::path::PathBuf::from) else {
                    eprintln!("file-browser-tui: --pick needs a request file");
                    std::process::exit(2);
                };
                let title = pick::Picking::load(&req).map(|p| p.req.title).unwrap_or_default();
                actions::kitty_pick_command(&req, &title)
            } else {
                let path = std::env::args_os().nth(2).map(std::path::PathBuf::from)
                    .and_then(|p| std::fs::canonicalize(p).ok());
                actions::kitty_command(path.as_deref())
            };
            let Some(mut cmd) = cmd else { std::process::exit(1) };
            let err = cmd.exec();
            eprintln!("file-browser-tui: couldn't start kitty: {err}");
            std::process::exit(1);
        }
        Some("--version" | "-V") => {
            println!("file-browser-tui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("{}", HELP);
            return Ok(());
        }
        Some("--pick") => {}
        Some(a) if a.starts_with('-') => {
            eprintln!("file-browser-tui: unknown option {a} (see --help)");
            std::process::exit(2);
        }
        _ => {}
    }
    // Log to file (stdout is occupied by TUI)
    let log_file = std::fs::File::create("/tmp/file-browser-tui.log")?;
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("file_browser_tui=debug")),
        )
        .init();

    // A crash puts the terminal back and says where, in the log too
    std::panic::set_hook(Box::new(|info| {
        ratatui::restore();
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        let trace = std::backtrace::Backtrace::force_capture();
        tracing::error!("crashed: {info}\n{trace}");
        eprintln!("file-browser-tui crashed: {info}\n(details in /tmp/file-browser-tui.log)");
    }));

    let first_run = !Config::path().exists();
    let (config, config_err) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => {
            tracing::error!("config: {e:#}");
            (Config::unwritable(), Some(e))
        }
    };

    // Enter alternate screen FIRST (required before Picker::from_query_stdio)
    let mut terminal = ratatui::init();

    execute!(std::io::stdout(), EnableMouseCapture, EnableFocusChange)?;

    // Kitty keyboard protocol: distinguishes Shift+Backspace etc. and reports
    // event kinds (we filter to Press/Repeat so keys don't double-fire).
    let kbd_enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    if kbd_enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
    }

    // Query terminal capabilities AFTER alternate screen, BEFORE any event::read()
    let picker = Picker::from_query_stdio().unwrap_or_else(|e| {
        tracing::warn!("Picker::from_query_stdio failed: {e}");
        Picker::halfblocks()
    });

    tracing::info!(
        "Protocol: {:?}, font_size: {:?} (terminal answered after {:?})",
        picker.protocol_type(),
        picker.font_size(),
        launched.elapsed()
    );

    // Background event channels
    let (dir_tx, dir_rx) = mpsc::channel();
    let (mount_tx, mount_rx) = mpsc::channel();

    // Picking for another program: what to pick, from the request file
    let pick = if std::env::args().nth(1).as_deref() == Some("--pick") {
        let req = std::env::args_os().nth(2).map(std::path::PathBuf::from).unwrap_or_default();
        match pick::Picking::load(&req) {
            Ok(p) => Some(p),
            Err(e) => {
                ratatui::restore();
                eprintln!("file-browser-tui: pick request {}: {e:#}", req.display());
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    // Optional start path: a folder opens there, a file opens its folder
    // with it selected
    let start = if pick.is_some() {
        None
    } else {
        std::env::args_os().nth(1).map(std::path::PathBuf::from)
            .and_then(|p| std::fs::canonicalize(p).ok())
    };
    match &start {
        Some(p) => {
            let dir = if p.is_dir() { Some(p.as_path()) } else { p.parent() };
            if let Some(dir) = dir {
                let _ = std::env::set_current_dir(dir);
            }
        }
        None => {
            let dir = pick.as_ref().map_or_else(|| config.start_folder(), pick::start_folder);
            let _ = std::env::set_current_dir(dir);
        }
    }

    std::thread::spawn(vfs::clean_up);
    let mut app = App::new(picker, config, dir_rx, mount_rx)?;
    if let Some(p) = pick {
        let file = p.req.current_file.clone();
        app.pick = Some(p);
        app.reload_dir();
        if let Some(f) = file.filter(|f| f.exists()) {
            app.reveal(&f);
        }
    }
    // Started in kitty some other way: say once how to get its options
    if first_run
        && app.picker.protocol_type() == ProtocolType::Kitty
        && std::env::var_os(actions::KITTY_ENV).is_none()
    {
        app.notify("Tip: launch with `file-browser-tui --kitty` for Shift-click selection and no window padding".into());
    }
    if let Some(p) = start.filter(|p| !p.is_dir()) {
        app.reveal(&p);
    }

    // Start background watchers
    let dir_watcher = fs::watcher::DirWatcher::spawn(dir_tx);
    if let Some(w) = &dir_watcher {
        w.watch(&app.cwd);
    }
    fs::mounts::spawn_mount_watcher(mount_tx);

    let is_kitty = app.picker.protocol_type() == ProtocolType::Kitty;
    if is_kitty {
        dnd::enable();
    }
    tracing::info!("app ready after {:?}", launched.elapsed());
    let result = run(&mut app, &mut terminal, dir_watcher.as_ref(), launched);
    if is_kitty {
        dnd::disable();
    }

    if kbd_enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    execute!(std::io::stdout(), DisableMouseCapture, DisableFocusChange)?;
    ratatui::restore();

    // Persist config and per-dir view state
    if app.pick.is_none() {
        config::save_last_folder(&app.cwd);
    }
    let _ = build_config(&app).save();
    app.dirstates.save();
    vfs::clean_up();

    if let Some(e) = config_err {
        eprintln!("file-browser-tui: config not loaded or saved: {e:#}");
    }
    if let Some(e) = &app.dirstates.load_error {
        eprintln!("file-browser-tui: dirstate not loaded or saved: {e}");
    }

    result
}

const HELP: &str = "\
file-browser-tui: a Dolphin-like file manager for kitty

Usage: file-browser-tui [--kitty] [PATH]
  PATH        folder to open, or a file to open its folder with it selected
              (without one: home_page, or the last folder with resume = true)

Options:
  --kitty     open in a new kitty window set up for the app (no padding,
              Shift/Ctrl+Shift clicks reach it); use this in launchers
  --pick REQUEST
              pick files for another program, as xdg-desktop-portal-tui asks
              (a TOML request; with --kitty first, in a window of its own)
  --check     list the optional tools and features found on this system
  --version   print the version
  --help      print this help

Config: ~/.config/file-browser-tui/config.toml (keys, theme, places, ...)
In the app, F1 lists the keyboard shortcuts.";

fn run(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
    dir_watcher: Option<&fs::watcher::DirWatcher>,
    launched: std::time::Instant,
) -> anyhow::Result<()> {
    // A window we just opened is usually resized by the window manager
    // (tiled) a moment after it appears: draw once it has its size, not
    // once at kitty's default size and again laid out anew
    if std::env::var_os(actions::KITTY_ENV).is_some() && launched.elapsed() < Duration::from_millis(500) {
        let first = terminal.size()?;
        let mut last = first;
        let mut changed_at = None;
        let waiting = std::time::Instant::now();
        while waiting.elapsed() < Duration::from_millis(250) {
            std::thread::sleep(Duration::from_millis(4));
            let now = terminal.size()?;
            if now != last {
                last = now;
                changed_at = Some(std::time::Instant::now());
            }
            // Settled: no further change for a moment after the first
            if changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(30)) {
                break;
            }
        }
        tracing::info!("window size {}x{} -> {}x{} after {:?}", first.width, first.height, last.width, last.height, waiting.elapsed());
    }
    // Bootstrap layout before first render so queue_visible_icons has correct areas
    let sz = terminal.size()?;
    app.main_area = ratatui::layout::Rect {
        x: app.sidebar_w,
        y: app::TOPBAR_H,
        width: sz.width.saturating_sub(app.sidebar_w),
        height: sz.height.saturating_sub(app::TOPBAR_H),
    };
    app.grid_cols = (app.main_area.width / app.cell_w).max(1);
    app.last_terminal_size = ratatui::layout::Size { width: sz.width, height: sz.height };
    app.queue_visible_icons();
    // A new window shows up complete: give the first frame's icons a
    // moment to load instead of drawing without them
    ui::sidebar::request_icons(app);
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(150)
        && (app.icon_cache.loading() || app.sidebar_icons.loading() || app.thumb_cache.loading() || !app.devices_ready)
    {
        std::thread::sleep(Duration::from_millis(2));
        app.drain_channels();
    }

    tracing::info!(
        "first frame after {:?} (still loading: icons {}, sidebar {}, thumbnails {}, devices {})",
        launched.elapsed(), app.icon_cache.loading(), app.sidebar_icons.loading(), app.thumb_cache.loading(), !app.devices_ready
    );
    let is_kitty = app.picker.protocol_type() == ProtocolType::Kitty;
    let mut title_path = std::path::PathBuf::new();
    let mut last_active = std::time::Instant::now();
    let mut last_frame_hash = 0u64;
    let mut overlay_was_open = false;
    let mut drawn_cwd = app.cwd.clone();

    'frames: loop {
        if app.pick.as_ref().is_some_and(|p| p.done) {
            break;
        }
        // Drain background results (icons, thumbs, dir events, mount events)
        app.drain_channels();

        // A folder just opened: give its images on screen a moment to load
        // (from the thumbnail cache, a few ms each) and show it complete,
        // like a new window, instead of filling in over a few frames. The
        // last folder stays up meanwhile; slow ones still fill in after
        if app.cwd != drawn_cwd {
            drawn_cwd = app.cwd.clone();
            if app.view_mode == app::ViewMode::Icons {
                app.layout_icons(app.grid_cols);
            }
            app.queue_all_visible_icons();
            let opened = std::time::Instant::now();
            while opened.elapsed() < Duration::from_millis(150)
                && (app.icon_cache.loading() || app.thumb_cache.loading())
            {
                std::thread::sleep(Duration::from_millis(2));
                app.drain_channels();
            }
        }

        // Render frame. Everything it writes (cells, centered labels, image
        // cleanup) goes in one synchronized update, so the terminal shows
        // whole frames only, never one half-drawn mid-scroll
        app.tick_scroll();
        {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"\x1b[?2026h");
        }
        // A menu or dialog just closed: redraw the whole screen. Kitty text
        // blocks under it can end up where ratatui doesn't know (kitty
        // moves text written onto another block), and only a full redraw
        // is sure to put every cell back
        let overlay_open = app.overlay_open();
        if overlay_was_open && !overlay_open && is_kitty {
            terminal.clear()?;
            app.forget_screen();
        }
        overlay_was_open = overlay_open;
        // Every image is to go (the icon size changed): delete them before
        // drawing, so this frame sends the new ones in the same update.
        // Deleted after it, the images just sent went too, and every icon
        // blinked out until the next frame
        if is_kitty && app.kitty_delete_all {
            tracing::debug!("frame: deleting all images before drawing");
            app.forget_images();
            let mut out = std::io::stdout().lock();
            let _ = ui::kitty::free_images(&mut out, &std::mem::take(&mut app.kitty_free));
            let _ = ui::kitty::delete_all(&mut out);
        }
        let frame_hash = {
            use std::hash::{Hash, Hasher};
            let done = terminal.draw(|f| ui::render(f, app))?;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            done.buffer.hash(&mut h);
            h.finish()
        };
        // Anything that changed the picture (an icon or preview arriving,
        // a list filling in) keeps frames at full rate for a while, so work
        // still coming in shows at once rather than at the idle pace
        if frame_hash != last_frame_hash || !app.post_draw.is_empty() {
            last_active = std::time::Instant::now();
        }
        last_frame_hash = frame_hash;
        // Window title follows the current folder, like Dolphin's
        if app.cwd != title_path {
            title_path = app.cwd.clone();
            let title = match app.pick.as_ref().map(|p| p.req.title.as_str()).filter(|t| !t.is_empty()) {
                Some(t) => format!("{t} \u{2014} {}", title_path.display()),
                None => title_path.to_string_lossy().to_string(),
            };
            let _ = execute!(std::io::stdout(), SetTitle(title));
        }
        if !app.post_draw.is_empty() {
            let mut out = std::io::stdout().lock();
            for s in app.post_draw.drain(..) {
                let _ = out.write_all(s.as_bytes());
            }
            let _ = out.flush();
        }

        // Clean up Kitty placements for images that vanished or changed
        // this frame (occluded by overlays, scrolled away, dir changed, ...)
        let (deletions, delete_all) = app.kitty_frame_diff();
        if is_kitty && (delete_all || !deletions.is_empty() || !app.kitty_free.is_empty() || app.frame_sent.0 > 0) {
            tracing::debug!(
                "frame: sent {} images ({} KiB), freed {}, cleared {} places{}",
                app.frame_sent.0, app.frame_sent.1 / 1024, app.kitty_free.len(), deletions.len(),
                if delete_all { ", deleted all images" } else { "" }
            );
        }
        app.frame_sent = (0, 0);
        if is_kitty && !app.kitty_free.is_empty() {
            let mut out = std::io::stdout().lock();
            let _ = ui::kitty::free_images(&mut out, &std::mem::take(&mut app.kitty_free));
        }
        if is_kitty && (delete_all || !deletions.is_empty()) {
            let mut out = std::io::stdout().lock();
            if delete_all {
                let _ = ui::kitty::delete_all(&mut out);
            } else {
                let _ = ui::kitty::delete_at(&mut out, &deletions);
            }
            let _ = out.flush();
        }
        {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"\x1b[?2026l");
            let _ = out.flush();
        }

        // Queue icons for newly visible cells after render updated grid_cols/grid_area
        app.queue_all_visible_icons();

        // Wait up to a frame for input, then take everything already
        // queued before drawing again: a burst of wheel events is one frame.
        // Idle a while (no input, nothing loading or moving), frames slow to
        // 10 a second, enough for cursor blinks and notices; input still
        // ends the wait at once
        if app.animating() {
            last_active = std::time::Instant::now();
        }
        let frame = if last_active.elapsed() < Duration::from_millis(300) {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        let mut timeout = frame;
        let mut first_event: Option<std::time::Instant> = None;
        while event::poll(timeout)? {
            timeout = Duration::ZERO;
            last_active = std::time::Instant::now();
            // A steady stream of events still gets a frame every 16 ms
            if first_event.get_or_insert_with(std::time::Instant::now).elapsed() > Duration::from_millis(16) {
                break;
            }
            let ev = event::read()?;
            if let Event::Key(key) = &ev
                && dnd::swallow(app, key)
            {
                // The rest of the message is already buffered: take it
                // whole instead of redrawing per character
                while app.dnd.collecting() && event::poll(Duration::from_millis(50))? {
                    if let Event::Key(key) = event::read()? {
                        dnd::swallow(app, &key);
                    }
                }
                continue;
            }
            match ev {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if handler::handle_key(app, key) {
                        break 'frames;
                    }
                    maybe_restart_watcher(app, dir_watcher);
                }
                Event::Mouse(mouse) => {
                    handler::handle_mouse(app, mouse);
                    maybe_restart_watcher(app, dir_watcher);  // restart watcher if navigated
                }
                Event::FocusGained | Event::FocusLost => {
                    tracing::debug!("{}", if matches!(ev, Event::FocusGained) { "focus gained" } else { "focus lost" });
                }
                Event::Resize(w, h) => {
                    tracing::debug!("resize to {w}x{h}");
                    let new_size = ratatui::layout::Size { width: w, height: h };
                    if new_size != app.last_terminal_size {
                        app.last_terminal_size = new_size;
                        if is_kitty {
                            // Placeholder images follow their cells through the
                            // reflow; only direct placements (underlays) and
                            // sized text are wiped by the screen clear
                            app.forget_screen();
                        } else {
                            app.post_text_shown.clear();
                            app.icon_cache.clear_protocols();
                            app.sidebar_icons.clear_protocols();
                            app.thumb_cache.clear_protocols();
                            app.kitty_delete_all = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Re-point the dir watcher when cwd has changed from what we're currently watching.
fn maybe_restart_watcher(app: &mut App, dir_watcher: Option<&fs::watcher::DirWatcher>) {
    if app.watched_path != app.cwd {
        app.watched_path = app.cwd.clone();
        tracing::debug!("folder: {}", app.cwd.display());
        if let Some(w) = dir_watcher {
            w.watch(&app.cwd);
        }
    }
}

fn build_config(app: &App) -> Config {
    let mut config = app.config.clone();
    config.places = app.places.iter().map(|p| config::PlaceConfig {
        name: p.name.clone(),
        path: p.path.clone(),
        icon: p.icon.clone(),
    }).collect();
    // View/sort settings are per folder (dirstate.toml); the config's
    // values are the defaults for folders without their own
    config.sidebar_w = app.sidebar_w;
    config
}
