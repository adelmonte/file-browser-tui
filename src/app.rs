use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::mpsc};

use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

use crate::{
    config::Config,
    dirstate::DirStates,
    events::{ClickTracker, Clipboard, ConfirmDeleteState, DragState, RenameState, SidebarTarget},
    fs::{DirListing, mounts::DeviceEntry, mounts::MountEvent, watcher::DirEvent},
    icons::IconCache,
    places::PlaceEntry,
    thumbs::ThumbCache,
    ui::menu::{MenuState, MenuTarget},
    ui::properties::PropertiesState,
};

// ---------------------------------------------------------------------------
// Enums shared between app, config, and fs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, EnumIter, Display)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    Icons,
    Compact,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, EnumIter, Display)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    #[default]
    Name,
    Size,
    Modified,
    Created,
    Accessed,
    Type,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortState {
    pub field: SortField,
    pub ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self { field: SortField::Name, ascending: true }
    }
}

// ---------------------------------------------------------------------------
// Grid layout constants
// ---------------------------------------------------------------------------

/// Topbar height in rows (1 padding + 1 content + 1 padding).
pub const TOPBAR_H: u16 = 3;

/// Available icon grid cell sizes: (cell_w, cell_h, icon_rows)
pub const ICON_SIZES: [(u16, u16, u16); 14] = [
    (4,  2, 1),   // 0: tiny
    (6,  3, 2),   // 1: xs
    (8,  4, 3),   // 2: sm
    (10, 5, 3),   // 3: sm+
    (12, 6, 4),   // 4: md  (default)
    (14, 7, 5),   // 5: md+
    (16, 8, 6),   // 6: lg
    (18, 9, 7),   // 7: lg+
    (20, 10, 8),  // 8: xl
    (24, 12, 10), // 9: xl+
    (28, 14, 12), // 10: 2xl
    (32, 17, 15), // 11: 3xl
    (38, 20, 18), // 12: 4xl
    (48, 25, 23), // 13: 5xl
];

/// Image identity rendered in a cell this frame: (rect, bg variant).
pub type FrameImage = (Rect, Option<(u8, u8, u8)>);

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    // Navigation
    pub cwd: PathBuf,
    pub history_back: Vec<PathBuf>,
    pub history_forward: Vec<PathBuf>,

    // Directory contents
    pub listing: DirListing,
    pub selection: Option<usize>,
    pub scroll_offset: usize,
    /// Bytes available on the cwd's filesystem, refreshed on reload.
    pub free_space: Option<u64>,

    // View settings
    pub view_mode: ViewMode,
    pub sort: SortState,
    pub show_hidden: bool,
    pub folders_first: bool,

    // Dynamic icon cell size (index into ICON_SIZES)
    pub icon_size_idx: usize,
    pub cell_w: u16,
    pub cell_h: u16,
    pub icon_rows: u16,

    // Image rendering
    pub picker: Picker,
    pub icon_cache: IconCache,
    pub thumb_cache: ThumbCache,

    // Background event channels
    pub dir_event_rx: mpsc::Receiver<DirEvent>,
    pub mount_event_rx: mpsc::Receiver<MountEvent>,

    // Sidebar
    pub places: Vec<PlaceEntry>,
    pub devices: Vec<DeviceEntry>,
    pub sidebar_w: u16,
    pub sidebar_dragging: bool,
    /// Row → target map built during sidebar render (index = row in inner area).
    pub sidebar_rows: Vec<Option<SidebarTarget>>,
    pub sidebar_area: Rect,

    // Overlay / interaction state
    pub menu: Option<MenuState>,
    pub properties: Option<PropertiesState>,
    pub rename_state: Option<RenameState>,
    pub drag_state: DragState,
    pub click_tracker: ClickTracker,
    pub show_help: bool,
    pub clipboard: Option<Clipboard>,
    pub confirm_delete: Option<ConfirmDeleteState>,

    // Multi-selection
    pub multi_selection: std::collections::HashSet<usize>,
    pub selection_anchor: Option<usize>,

    // Path bar editing
    pub path_bar_editing: bool,
    pub path_bar_input: String,
    pub path_completions: Vec<PathBuf>,
    pub path_completion_sel: Option<usize>,
    /// Actual rendered completion dropdown rect (for hit-testing).
    pub completions_rect: Rect,

    // Topbar button rects (set during render, used for hit-testing)
    pub topbar_back_rect: Rect,
    pub topbar_fwd_rect: Rect,
    pub topbar_up_rect: Rect,
    pub topbar_view_rect: Rect,
    pub topbar_sort_rect: Rect,
    pub topbar_path_rect: Rect,

    // Cached layout (updated on each render)
    pub grid_cols: u16,
    pub grid_area: Rect,
    pub main_area: Rect,

    // Track last terminal size for resize detection
    pub last_terminal_size: Size,

    // Track which path the dir watcher is currently watching
    pub watched_path: PathBuf,

    // Config & per-dir persistence
    pub config: Config,
    pub dirstates: DirStates,

    // Kitty graphics bookkeeping
    /// Image uploads lifted out of placeholder cells, written before the frame.
    pub kitty_transmits: Vec<String>,
    /// Images rendered this frame, keyed by path.
    pub frame_images: HashMap<PathBuf, FrameImage>,
    prev_frame_images: HashMap<PathBuf, FrameImage>,
    /// Request a bulk placement wipe after the next draw.
    pub kitty_delete_all: bool,
}

impl App {
    pub fn new(
        picker: Picker,
        config: Config,
        dir_event_rx: mpsc::Receiver<DirEvent>,
        mount_event_rx: mpsc::Receiver<MountEvent>,
    ) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let sort = SortState {
            field: config.sort.field,
            ascending: config.sort.ascending,
        };
        let listing = crate::fs::read_dir(&cwd, config.show_hidden, config.folders_first, &sort)
            .unwrap_or_default();

        let places = crate::places::places_from_config(&config.places);
        let devices = crate::places::default_devices();
        let dirstates = DirStates::load();

        let terminal_size = Size { width: 220, height: 50 };
        let mut icon_size_idx = config.icon_size_idx.min(ICON_SIZES.len() - 1);
        let mut view_mode = config.default_view;
        if let Some(state) = dirstates.get(&cwd) {
            if let Some(v) = state.view { view_mode = v; }
            if let Some(idx) = state.icon_size_idx { icon_size_idx = idx.min(ICON_SIZES.len() - 1); }
        }
        let (cell_w, cell_h, icon_rows) = ICON_SIZES[icon_size_idx];

        Ok(Self {
            watched_path: cwd.clone(),
            sidebar_w: config.sidebar_w.clamp(10, 50),
            sidebar_dragging: false,
            sidebar_rows: Vec::new(),
            sidebar_area: Rect::default(),
            show_help: false,
            cwd,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            listing,
            selection: None,
            scroll_offset: 0,
            free_space: free_space(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
            view_mode,
            sort,
            show_hidden: config.show_hidden,
            folders_first: config.folders_first,
            icon_size_idx,
            cell_w,
            cell_h,
            icon_rows,
            picker,
            icon_cache: IconCache::new(),
            thumb_cache: ThumbCache::new(),
            dir_event_rx,
            mount_event_rx,
            places,
            devices,
            menu: None,
            properties: None,
            rename_state: None,
            drag_state: DragState::Idle,
            click_tracker: ClickTracker::default(),
            clipboard: None,
            confirm_delete: None,
            multi_selection: std::collections::HashSet::new(),
            selection_anchor: None,
            path_bar_editing: false,
            path_bar_input: String::new(),
            path_completions: Vec::new(),
            path_completion_sel: None,
            completions_rect: Rect::default(),
            topbar_back_rect: Rect::default(),
            topbar_fwd_rect: Rect::default(),
            topbar_up_rect: Rect::default(),
            topbar_view_rect: Rect::default(),
            topbar_sort_rect: Rect::default(),
            topbar_path_rect: Rect::default(),
            grid_cols: 8,
            grid_area: Rect::default(),
            main_area: Rect::default(),
            last_terminal_size: terminal_size,
            config,
            dirstates,
            kitty_transmits: Vec::new(),
            frame_images: HashMap::new(),
            prev_frame_images: HashMap::new(),
            kitty_delete_all: false,
        })
    }

    pub fn reload_dir(&mut self) {
        let mut listing = match crate::fs::read_dir(&self.cwd, self.show_hidden, self.folders_first, &self.sort) {
            Ok(listing) => listing,
            Err(e) => { tracing::error!("reload_dir: {e}"); return; }
        };

        // Mime types are only resolved once per path; restore them so media
        // entries keep showing thumbnails instead of generic icons.
        for entry in &mut listing.entries {
            if let Some(mime) = self.icon_cache.cached_mime(&entry.path) {
                entry.mime_type = mime.clone();
            }
        }

        let old = std::mem::replace(&mut self.listing, listing);
        self.remap_indices(&old);
        self.free_space = free_space(&self.cwd);
    }

    /// Re-point every index into the listing at the same path in the new
    /// listing, so a reload never shifts state onto a different file.
    fn remap_indices(&mut self, old: &DirListing) {
        let new_idx: HashMap<&std::path::Path, usize> = self.listing.entries.iter()
            .enumerate()
            .map(|(i, e)| (e.path.as_path(), i))
            .collect();
        let remap = |i: usize| old.entries.get(i).and_then(|e| new_idx.get(e.path.as_path()).copied());

        self.selection = self.selection.and_then(remap);
        self.selection_anchor = self.selection_anchor.and_then(remap);
        self.multi_selection = self.multi_selection.iter().filter_map(|&i| remap(i)).collect();
        self.click_tracker.remap(remap);

        if let Some(rename) = &mut self.rename_state {
            match remap(rename.item_idx) {
                Some(i) => rename.item_idx = i,
                None => self.rename_state = None,
            }
        }

        match &mut self.drag_state {
            DragState::Idle => {}
            DragState::Armed { item_idx, .. } => match remap(*item_idx) {
                Some(i) => *item_idx = i,
                None => self.drag_state = DragState::Idle,
            },
            DragState::Dragging { item_idx, hover_target, .. } => match remap(*item_idx) {
                Some(i) => {
                    *item_idx = i;
                    *hover_target = hover_target.and_then(remap);
                }
                None => self.drag_state = DragState::Idle,
            },
        }

        if let Some(MenuState { target: MenuTarget::Entry(idx), .. }) = &mut self.menu {
            match remap(*idx) {
                Some(i) => *idx = i,
                None => self.menu = None,
            }
        }
    }

    pub fn navigate_to(&mut self, path: PathBuf) {
        if path == self.cwd { return; }
        let old = std::mem::replace(&mut self.cwd, path);
        self.history_back.push(old);
        self.history_forward.clear();
        self.after_navigate();
    }

    pub fn navigate_back(&mut self) {
        if let Some(prev) = self.history_back.pop() {
            let current = std::mem::replace(&mut self.cwd, prev);
            self.history_forward.push(current);
            self.after_navigate();
        }
    }

    pub fn navigate_forward(&mut self) {
        if let Some(next) = self.history_forward.pop() {
            let current = std::mem::replace(&mut self.cwd, next);
            self.history_back.push(current);
            self.after_navigate();
        }
    }

    pub fn navigate_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            self.navigate_to(parent);
        }
    }

    fn after_navigate(&mut self) {
        self.scroll_offset = 0;
        self.selection = None;
        self.multi_selection.clear();
        self.selection_anchor = None;
        self.click_tracker.reset();
        self.apply_dir_state();
        self.reload_dir();
    }

    /// Apply the saved per-directory view settings, if any.
    fn apply_dir_state(&mut self) {
        let Some(state) = self.dirstates.get(&self.cwd) else { return; };
        let (view, size, field, asc) = (
            state.view,
            state.icon_size_idx,
            state.sort_field,
            state.sort_ascending,
        );
        if let Some(v) = view
            && v != self.view_mode { self.view_mode = v; }
        if let Some(idx) = size {
            self.set_icon_size(idx.min(ICON_SIZES.len() - 1));
        }
        if let Some(f) = field { self.sort.field = f; }
        if let Some(a) = asc   { self.sort.ascending = a; }
    }

    /// Set view mode and persist it for the current directory.
    pub fn set_view_mode(&mut self, view: ViewMode) {
        if view != self.view_mode {
            self.view_mode = view;
        }
        self.dirstates.set_view(&self.cwd, view);
    }

    fn set_icon_size(&mut self, idx: usize) {
        if idx == self.icon_size_idx { return; }
        self.icon_size_idx = idx;
        let (w, h, r) = ICON_SIZES[idx];
        self.cell_w = w;
        self.cell_h = h;
        self.icon_rows = r;
        // Raster size changed: drop everything and wipe placements after next draw
        self.icon_cache.clear();
        self.thumb_cache.clear();
        self.kitty_delete_all = true;
    }

    /// Step icon size up (+1) or down (-1) and persist per-dir.
    pub fn resize_icons(&mut self, delta: i8) {
        let new_idx = (self.icon_size_idx as i8 + delta)
            .clamp(0, ICON_SIZES.len() as i8 - 1) as usize;
        if new_idx != self.icon_size_idx {
            self.set_icon_size(new_idx);
            self.dirstates.set_icon_size(&self.cwd, new_idx);
        }
    }

    /// Pixel size to render icons at.
    pub fn icon_target_px(&self) -> u32 {
        let fs = self.picker.font_size();
        let px = fs.width as u32 * self.cell_w as u32;
        px.clamp(32, 256)
    }

    /// Return the freedesktop icon name for XDG user directories (Music, Videos, etc.).
    fn xdg_folder_icon(path: &std::path::Path) -> Option<String> {
        macro_rules! check {
            ($expr:expr, $name:literal) => {
                if $expr.as_deref() == Some(path) { return Some($name.into()); }
            };
        }
        check!(dirs::home_dir(),     "user-home");
        check!(dirs::download_dir(), "folder-download");
        check!(dirs::audio_dir(),    "folder-music");
        check!(dirs::picture_dir(),  "folder-pictures");
        check!(dirs::video_dir(),    "folder-videos");
        check!(dirs::document_dir(), "folder-documents");
        check!(dirs::desktop_dir(),  "user-desktop");
        check!(dirs::template_dir(), "folder-templates");
        check!(dirs::public_dir(),   "folder-publicshare");
        None
    }

    /// Queue icon loads for all visible entries.
    pub fn queue_visible_icons(&mut self) {
        if self.view_mode != ViewMode::Icons {
            return;
        }
        let target_px = self.icon_target_px();
        let cols = self.grid_cols.max(1) as usize;
        let rows_visible = (self.grid_area.height / self.cell_h + 2) as usize;
        let start = self.scroll_offset * cols;
        let end = ((self.scroll_offset + rows_visible) * cols).min(self.listing.entries.len());
        if start >= end { return; }

        for i in start..end {
            let entry = &self.listing.entries[i];
            let mime = entry.mime_type.clone();
            let is_media = mime.as_deref()
                .map(|m| m.starts_with("image/") || m.starts_with("video/"))
                .unwrap_or(false);

            if is_media {
                if let Some(ref mime_type) = mime {
                    self.thumb_cache.request(&entry.path, mime_type);
                }
            } else {
                let icon_override = if entry.is_dir {
                    self.config.folder_icons
                        .get(entry.path.to_string_lossy().as_ref())
                        .cloned()
                        .or_else(|| Self::xdg_folder_icon(&entry.path))
                } else {
                    None
                };
                self.icon_cache.request(&entry.path, entry.is_dir, target_px, icon_override);
            }
        }
    }

    /// Drain all background channels.
    pub fn drain_channels(&mut self) {
        let mime_updates = self.icon_cache.drain_ready();
        for (path, mime) in mime_updates {
            if let Some(entry) = self.listing.entries.iter_mut().find(|e| e.path == path)
                && entry.mime_type.is_none() {
                    entry.mime_type = mime;
                }
        }

        self.thumb_cache.drain_ready();

        // Coalesce bursts of events into a single reload
        if self.dir_event_rx.try_iter().count() > 0 {
            self.reload_dir();
        }

        if let Ok(ev) = self.mount_event_rx.try_recv() {
            let MountEvent::Updated(devs) = ev;
            self.devices = devs;
        }

        if let Some(ref mut props) = self.properties {
            props.poll();
        }
    }

    /// Diff the images rendered this frame against the previous frame.
    /// Returns rects whose Kitty placements must be deleted (or a delete-all
    /// flag for bulk changes), and invalidates the affected protocols so they
    /// re-transmit on their next render.
    pub fn kitty_frame_diff(&mut self) -> (Vec<Rect>, bool) {
        let mut deletions: Vec<(PathBuf, Rect)> = Vec::new();

        for (path, (rect, bg)) in &self.prev_frame_images {
            match self.frame_images.get(path) {
                // Unchanged, or moved with the same variant (ratatui-image
                // re-places moved images itself).
                Some((nr, nbg)) if nbg == bg && nr == rect => {}
                Some((nr, _)) if *nr != *rect => {}
                // Variant changed (selection toggled) or image vanished
                // (scrolled off, dir changed, occluded by an overlay).
                _ => deletions.push((path.clone(), *rect)),
            }
        }

        let delete_all = self.kitty_delete_all;
        self.kitty_delete_all = false;
        self.prev_frame_images = std::mem::take(&mut self.frame_images);

        if delete_all {
            self.icon_cache.clear_protocols();
            self.thumb_cache.clear_protocols();
            return (Vec::new(), true);
        }

        for (path, _) in &deletions {
            self.icon_cache.invalidate(path);
            self.thumb_cache.invalidate(path);
        }
        (deletions.into_iter().map(|(_, r)| r).collect(), false)
    }

    pub fn open_selected(&self) {
        if let Some(idx) = self.selection
            && let Some(entry) = self.listing.entries.get(idx) {
                if entry.is_dir { return; }
                let _ = std::process::Command::new("xdg-open").arg(&entry.path)
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            }
    }

    pub fn enter_selected(&mut self) {
        if let Some(idx) = self.selection
            && let Some(entry) = self.listing.entries.get(idx).cloned() {
                if entry.is_dir {
                    self.navigate_to(entry.path);
                } else {
                    let _ = std::process::Command::new("xdg-open").arg(&entry.path)
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
                }
            }
    }

    pub fn move_selection(&mut self, delta: i32) {
        let len = self.listing.entries.len();
        if len == 0 { return; }
        let new_idx = match self.selection {
            None => 0,
            Some(i) => (i as i32 + delta).clamp(0, len as i32 - 1) as usize,
        };
        self.selection = Some(new_idx);
        self.ensure_selection_visible();
    }

    pub fn move_selection_cols(&mut self, delta_col: i32) {
        self.move_selection(delta_col);
    }

    pub fn move_selection_rows(&mut self, delta_row: i32) {
        let cols = match self.view_mode {
            ViewMode::Details => 1,
            _ => self.grid_cols.max(1) as i32,
        };
        self.move_selection(delta_row * cols);
    }

    /// Extend the multi-selection by delta cells from the current anchor.
    pub fn extend_selection(&mut self, delta: i32) {
        let len = self.listing.entries.len();
        if len == 0 { return; }
        let anchor = self.selection_anchor.unwrap_or(self.selection.unwrap_or(0));
        self.selection_anchor = Some(anchor);
        let new_sel = (self.selection.unwrap_or(anchor) as i32 + delta)
            .clamp(0, len as i32 - 1) as usize;
        self.selection = Some(new_sel);
        let lo = anchor.min(new_sel);
        let hi = anchor.max(new_sel);
        self.multi_selection = (lo..=hi).collect();
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        let rows_visible = match self.view_mode {
            ViewMode::Details => self.grid_area.height.saturating_sub(1) as usize,
            ViewMode::Compact => (self.grid_area.height / crate::ui::compact::COMPACT_CELL_H).max(1) as usize,
            ViewMode::Icons => (self.grid_area.height / self.cell_h).max(1) as usize,
        };
        let cols = self.grid_cols.max(1) as usize;
        if let Some(idx) = self.selection {
            let row = match self.view_mode {
                ViewMode::Details => idx,
                _ => idx / cols,
            };
            if row < self.scroll_offset {
                self.scroll_offset = row;
            } else if rows_visible > 0 && row >= self.scroll_offset + rows_visible {
                self.scroll_offset = row + 1 - rows_visible;
            }
        }
    }

    /// Apply the inline rename. Invalid names, existing targets and failed
    /// renames keep the editor open (flagged invalid) instead of committing.
    pub fn commit_rename(&mut self) {
        let Some(state) = &mut self.rename_state else { return; };
        let Some(entry) = self.listing.entries.get(state.item_idx) else {
            self.rename_state = None;
            return;
        };

        let name = state.input.as_str();
        if name == entry.name {
            self.rename_state = None;
            return;
        }
        if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
            state.invalid = true;
            return;
        }

        let new_path = entry.path.with_file_name(name);
        if let Ok(target) = std::fs::symlink_metadata(&new_path) {
            // Allow case-only renames on case-insensitive filesystems, where
            // the "existing" target is the entry itself.
            use std::os::unix::fs::MetadataExt;
            let same_file = std::fs::symlink_metadata(&entry.path)
                .is_ok_and(|m| m.dev() == target.dev() && m.ino() == target.ino());
            if !same_file {
                tracing::warn!("rename: {:?} already exists", new_path);
                state.invalid = true;
                return;
            }
        }

        if let Err(e) = std::fs::rename(&entry.path, &new_path) {
            tracing::error!("rename: {e}");
            state.invalid = true;
            return;
        }
        self.rename_state = None;
        self.reload_dir();
        self.selection = self.listing.entries.iter().position(|e| e.path == new_path);
    }

    /// Recompute path completions from the current `path_bar_input`.
    pub fn update_path_completions(&mut self) {
        let input = self.path_bar_input.as_str();
        let path = std::path::Path::new(input);

        let (dir, prefix): (&std::path::Path, &str) = if input.ends_with('/') {
            (path, "")
        } else {
            let parent = path.parent().unwrap_or(std::path::Path::new("/"));
            let pfx = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            (parent, pfx)
        };

        self.path_completions = if dir.exists() {
            let prefix_lc = prefix.to_lowercase();
            let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| n.to_lowercase().starts_with(&prefix_lc))
                                .unwrap_or(false)
                        })
                        .collect()
                })
                .unwrap_or_default();
            entries.sort();
            entries.truncate(10);
            entries
        } else {
            vec![]
        };
        self.path_completion_sel = None;
    }

    /// Open the path bar for editing (Ctrl+L / Alt+D / click).
    pub fn start_path_edit(&mut self) {
        self.path_bar_editing = true;
        self.path_bar_input = self.cwd.to_string_lossy().to_string();
        self.path_completions.clear();
        self.path_completion_sel = None;
    }

    pub fn cancel_path_edit(&mut self) {
        self.path_bar_editing = false;
        self.path_bar_input.clear();
        self.path_completions.clear();
        self.path_completion_sel = None;
    }

    pub fn execute_drop(&mut self, src_idx: usize, dst_idx: usize, copy: bool) {
        let src = self.listing.entries.get(src_idx).map(|e| e.path.clone());
        let dst = self.listing.entries.get(dst_idx).map(|e| e.path.clone());
        if let (Some(src), Some(dst)) = (src, dst)
            && dst.is_dir() {
                let op = if copy { "copy" } else { "move" };
                let _ = std::process::Command::new("gio")
                    .args([op, src.to_str().unwrap_or(""), dst.to_str().unwrap_or("")])
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                    .spawn();
            }
    }
}

fn free_space(path: &std::path::Path) -> Option<u64> {
    let st = nix::sys::statvfs::statvfs(path).ok()?;
    Some(st.blocks_available() as u64 * st.fragment_size() as u64)
}
