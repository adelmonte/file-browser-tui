use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::mpsc};

use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

use crate::{
    config::Config,
    dirstate::DirStates,
    events::{ClickTracker, Clipboard, ConfirmDeleteState, DragState, RenameState, SidebarTarget},
    fs::{DirEntry, DirListing, mounts::DeviceEntry, mounts::MountEvent, watcher::DirEvent},
    icons::IconCache,
    places::PlaceEntry,
    thumbs::ThumbCache,
    ui::menu::{MenuState, MenuTarget},
    ui::properties::PropertiesState,
    search::{SearchState, SearchUpdate},
    user_actions::{ActionGroup, KdeConnectDevice},
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

/// Icon zoom levels, as icon height in terminal rows: one-row steps at
/// small and medium sizes, wider jumps at the top end.
pub const ICON_LEVELS: [u16; 20] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 22, 24, 27];
pub const DEFAULT_ICON_LEVEL: usize = 3;

/// Grid geometry for a zoom level: (cell_w, cell_h, icon_rows, img_w).
/// The image is square in pixels, so its width in columns follows the font's
/// aspect; the cell adds a small margin for labels so neighbours sit close
/// together, Dolphin-style. Two label rows sit below the icon; the gap
/// above it is a margin inside the image box (see icons::GRID_FIT).
pub fn icon_dims(level: usize, font: ratatui_image::FontSize) -> (u16, u16, u16, u16) {
    let rows = ICON_LEVELS[level.min(ICON_LEVELS.len() - 1)];
    let (fw, fh) = (font.width.max(1) as f32, font.height.max(1) as f32);
    // Only as wide as the image actually drawn (GRID_FIT shrinks it to
    // leave a gap above), or every cell carries invisible width
    let img_w = ((rows as f32 * fh / fw * crate::icons::GRID_FIT.max_h).round() as u16).max(2);
    let cell_w = (img_w + 2).max(8);
    let icon_rows = rows;
    (cell_w, icon_rows + 2, icon_rows, img_w)
}

/// Image identity rendered in a cell this frame: (rect, bg variant, dimmed).
pub type FrameImage = (Rect, Option<(u8, u8, u8)>, bool);

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Rubber band: anchor cell, current cell, and the selection it adds to
/// (Ctrl held).
/// A folder's item to warm up: path, is a folder, guessed mime type.
type PrefetchItem = (PathBuf, bool, Option<String>);

pub type Band = ((u16, u16), (u16, u16), std::collections::HashSet<usize>);

pub struct App {
    // Navigation
    pub cwd: PathBuf,
    pub history_back: Vec<PathBuf>,
    pub history_forward: Vec<PathBuf>,
    /// The item selected in each folder when it was left, re-selected on
    /// coming back.
    pub left_selection: HashMap<PathBuf, PathBuf>,

    // Directory contents
    pub listing: DirListing,
    pub selection: Option<usize>,
    pub scroll_offset: usize,
    /// Icons view smooth scrolling, in terminal rows: `scroll_pos` eases
    /// toward `scroll_target`; `scroll_offset`/`scroll_sub` (grid row and
    /// rows into it) are derived from it each frame.
    pub scroll_pos: f32,
    pub scroll_target: f32,
    pub scroll_sub: u16,
    /// `scroll_offset` as last derived; a different value means it was set
    /// directly (new folder, view change) and the animation jumps there.
    scroll_seen: usize,
    /// Bytes available on the cwd's filesystem, refreshed on reload.
    pub free_space: Option<u64>,

    // View settings
    pub view_mode: ViewMode,
    pub sort: SortState,
    pub show_hidden: bool,
    pub folders_first: bool,
    /// Details view groups (per folder).
    pub show_groups: bool,
    /// Details lines (headers and entries) as last laid out.
    pub details_rows: Vec<crate::ui::groups::DRow>,
    pub scrollbar: Option<crate::ui::scrollbar::Geom>,
    pub scrollbar_dragging: bool,
    /// Where each visible entry was drawn this frame (for rubber-band selection).
    pub item_rects: Vec<(usize, Rect)>,
    /// Rubber-band selection in progress.
    pub band: Option<Band>,

    // Icon zoom (index into ICON_LEVELS) and the grid geometry it implies
    pub icon_size_idx: usize,
    pub img_w: u16,
    pub cell_w: u16,
    /// Rendered column pitch: cell_w plus the leftover width spread evenly
    /// between columns, as Dolphin does.
    pub grid_pitch: u16,
    pub cell_h: u16,
    pub icon_rows: u16,

    // Image rendering
    pub picker: Picker,
    pub icon_cache: IconCache,
    pub thumb_cache: ThumbCache,
    pub previews: crate::thumbs::Previews,

    // Background event channels
    pub dir_event_rx: mpsc::Receiver<DirEvent>,
    pub mount_event_rx: mpsc::Receiver<MountEvent>,
    kdeconnect_rx: mpsc::Receiver<Vec<KdeConnectDevice>>,

    /// Active search (results replace the listing while it has a query).
    pub search: Option<SearchState>,

    // Context menu extensions (reloaded each time a context menu opens)
    pub user_actions: Vec<ActionGroup>,
    /// Apps for the right-clicked item: the default first (Open With).
    pub open_with: Vec<crate::apps::DesktopApp>,
    pub kdeconnect_devices: Vec<KdeConnectDevice>,
    /// Mount points of phones opened from the sidebar, by device id.
    pub kdeconnect_mounts: HashMap<String, PathBuf>,
    kdeconnect_mount_rx: Option<mpsc::Receiver<(String, Result<PathBuf, String>)>>,
    /// A message shown in the status bar until it expires.
    pub notice: Option<(String, std::time::Instant)>,
    /// Theme icons for the sidebar (separate keys from the file grid).
    pub sidebar_icons: IconCache,
    pub place_drag: Option<crate::events::PlaceDrag>,
    /// Details column headers and the sort field each sets.
    pub details_headers: Vec<(Rect, SortField)>,
    /// Last mouse position, for hover highlights.
    pub mouse_pos: Option<(u16, u16)>,

    // Sidebar
    pub places: Vec<PlaceEntry>,
    pub devices: Vec<DeviceEntry>,
    /// Eject buttons drawn this frame: (rect, index into `devices`).
    pub eject_rects: Vec<(Rect, usize)>,
    unmount_tx: mpsc::Sender<(String, Result<(), String>)>,
    unmount_rx: mpsc::Receiver<(String, Result<(), String>)>,
    pub sidebar_w: u16,
    pub sidebar_dragging: bool,
    /// Row → target map built during sidebar render (index = row in inner area).
    pub sidebar_rows: Vec<Option<SidebarTarget>>,
    pub sidebar_area: Rect,

    // Overlay / interaction state
    pub menu: Option<MenuState>,
    pub dnd: crate::dnd::Dnd,
    pub properties: Option<PropertiesState>,
    pub rename_state: Option<RenameState>,
    pub drag_state: DragState,
    pub click_tracker: ClickTracker,
    pub show_help: bool,
    pub clipboard: Option<Clipboard>,
    pub confirm_delete: Option<ConfirmDeleteState>,
    pub name_prompt: Option<crate::events::NamePrompt>,
    /// The open name prompt's Create and Cancel buttons.
    pub prompt_buttons: Option<(Rect, Rect)>,

    // Multi-selection
    pub multi_selection: std::collections::HashSet<usize>,
    /// Folders expanded in place in the Details view.
    pub expanded: std::collections::HashSet<PathBuf>,
    /// Item counts shown as folder sizes in Details (None = unreadable).
    pub dir_counts: HashMap<PathBuf, Option<usize>>,
    pub selection_anchor: Option<usize>,

    // Path bar editing
    pub path_bar_editing: bool,
    pub path_bar_input: String,
    pub path_bar_cursor: usize,
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
    pub topbar_search_rect: Rect,
    /// Breadcrumb hit areas and the folder each opens.
    pub topbar_crumbs: Vec<(Rect, PathBuf)>,
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
    pub keymap: crate::keys::Keymap,
    pub fileops: crate::fileops::FileOps,
    /// Quit was pressed while a file operation ran; a second press quits.
    pub quit_armed: Option<std::time::Instant>,
    /// The status bar's cancel button while a file operation runs.
    pub cancel_rect: Option<Rect>,
    /// Folders on screen whose contents' icons are being warmed up (so
    /// opening them shows icons at once), and at what icon size.
    prefetched: std::collections::HashSet<PathBuf>,
    prefetched_px: u32,
    prefetch_tx: mpsc::Sender<(PathBuf, bool)>,
    prefetch_rx: mpsc::Receiver<Vec<PrefetchItem>>,
    pub dirstates: DirStates,

    // Kitty graphics bookkeeping
    /// Image uploads lifted out of placeholder cells, written before the frame.
    pub kitty_transmits: Vec<String>,
    /// Raw output written after ratatui flushes a frame (Kitty multicell
    /// text, which ratatui can't express).
    pub post_draw: Vec<String>,
    /// Field backgrounds drawn under the text (see ui::underlay), by slot.
    pub underlays: HashMap<u32, crate::ui::underlay::Placed>,
    pub underlays_seen: std::collections::HashSet<u32>,
    /// Underlay slots currently shown under their alternate image id.
    pub underlays_alt: std::collections::HashSet<u32>,
    /// Rubber band on Kitty: pixel size its pieces were uploaded for, and
    /// where it's shown.
    pub band_uploaded: Option<(u32, u32)>,
    pub band_shown: Option<Rect>,
    /// The encoded band upload for a size, reused by later drags.
    pub band_upload: Option<((u32, u32), String)>,
    /// Images rendered this frame, keyed by path.
    pub frame_images: HashMap<PathBuf, FrameImage>,
    prev_frame_images: HashMap<PathBuf, FrameImage>,
    /// Request a bulk placement wipe after the next draw.
    pub kitty_delete_all: bool,
    /// Until then, icons still loading show nothing rather than a glyph
    /// that would flash for a few frames.
    pub icons_grace_until: std::time::Instant,
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
        // Filled by apply_dir_state + reload_dir below
        let listing = DirListing::default();

        let places = crate::places::places_from_config(&config.places);
        let devices = crate::places::default_devices();
        let (unmount_tx, unmount_rx) = mpsc::channel();
        let dirstates = DirStates::load();

        let (kdeconnect_tx, kdeconnect_rx) = mpsc::channel();
        crate::user_actions::spawn_kdeconnect_watcher(kdeconnect_tx);

        let terminal_size = Size { width: 220, height: 50 };
        let icon_size_idx = config.icon_size_idx.min(ICON_LEVELS.len() - 1);
        let view_mode = config.default_view;
        let (cell_w, cell_h, icon_rows, img_w) = icon_dims(icon_size_idx, picker.font_size());

        let (keymap, mut key_problems) = crate::keys::Keymap::new(&config.keys);
        let (prefetch_tx, prefetch_rx) = spawn_prefetch_lister();
        if let Some(t) = &config.theme {
            match t.clone().try_into::<crate::ui::theme::Theme>() {
                Ok(t) => crate::ui::theme::set(t),
                Err(e) => key_problems.push(format!("[theme]: {}", e.message())),
            }
        }
        let mut app = Self {
            keymap,
            fileops: crate::fileops::FileOps::new(),
            quit_armed: None,
            cancel_rect: None,
            prefetched: std::collections::HashSet::new(),
            prefetched_px: 0,
            prefetch_tx,
            prefetch_rx,
            watched_path: cwd.clone(),
            sidebar_w: config.sidebar_w.clamp(10, 50),
            sidebar_dragging: false,
            sidebar_rows: Vec::new(),
            sidebar_area: Rect::default(),
            show_help: false,
            cwd,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            left_selection: HashMap::new(),
            listing,
            selection: None,
            scroll_offset: 0,
            scroll_pos: 0.0,
            scroll_target: 0.0,
            scroll_sub: 0,
            scroll_seen: 0,
            free_space: free_space(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
            view_mode,
            sort,
            show_hidden: config.show_hidden,
            folders_first: config.folders_first,
            show_groups: config.show_in_groups,
            details_rows: Vec::new(),
            scrollbar: None,
            scrollbar_dragging: false,
            item_rects: Vec::new(),
            band: None,
            icon_size_idx,
            img_w,
            cell_w,
            grid_pitch: cell_w,
            cell_h,
            icon_rows,
            picker,
            icon_cache: IconCache::new(config.folder_previews),
            thumb_cache: ThumbCache::new(),
            previews: crate::thumbs::Previews::new(&config),
            dir_event_rx,
            mount_event_rx,
            kdeconnect_rx,
            user_actions: Vec::new(),
            open_with: Vec::new(),
            search: None,
            kdeconnect_devices: Vec::new(),
            kdeconnect_mounts: HashMap::new(),
            kdeconnect_mount_rx: None,
            notice: None,
            sidebar_icons: IconCache::new(false)
                .with_fit(crate::icons::Fit {
                    max_h: 1.0,
                    max_rows: crate::ui::sidebar::ICON_ROWS,
                    valign: crate::icons::VAlign::Center,
                }),
            mouse_pos: None,
            place_drag: None,
            details_headers: Vec::new(),
            places,
            devices,
            eject_rects: Vec::new(),
            unmount_tx,
            unmount_rx,
            menu: None,
            dnd: Default::default(),
            properties: None,
            rename_state: None,
            drag_state: DragState::Idle,
            click_tracker: ClickTracker::default(),
            clipboard: None,
            confirm_delete: None,
            name_prompt: None,
            prompt_buttons: None,
            multi_selection: std::collections::HashSet::new(),
            expanded: std::collections::HashSet::new(),
            dir_counts: HashMap::new(),
            selection_anchor: None,
            path_bar_editing: false,
            path_bar_input: String::new(),
            path_bar_cursor: 0,
            path_completions: Vec::new(),
            path_completion_sel: None,
            completions_rect: Rect::default(),
            topbar_back_rect: Rect::default(),
            topbar_fwd_rect: Rect::default(),
            topbar_up_rect: Rect::default(),
            topbar_view_rect: Rect::default(),
            topbar_sort_rect: Rect::default(),
            topbar_search_rect: Rect::default(),
            topbar_crumbs: Vec::new(),
            topbar_path_rect: Rect::default(),
            grid_cols: 8,
            grid_area: Rect::default(),
            main_area: Rect::default(),
            last_terminal_size: terminal_size,
            config,
            dirstates,
            kitty_transmits: Vec::new(),
            post_draw: Vec::new(),
            underlays: HashMap::new(),
            underlays_seen: std::collections::HashSet::new(),
            underlays_alt: std::collections::HashSet::new(),
            band_uploaded: None,
            band_shown: None,
            band_upload: None,
            frame_images: HashMap::new(),
            prev_frame_images: HashMap::new(),
            kitty_delete_all: false,
            icons_grace_until: std::time::Instant::now(),
        };
        app.apply_dir_state();
        app.reload_dir();
        if !key_problems.is_empty() {
            app.notify(format!("Config: {}", key_problems.join("; ")));
        }
        Ok(app)
    }

    pub fn reload_dir(&mut self) {
        // While showing results, "reload" means re-running the search
        // (sort/hidden toggles, renames, ...), never listing the folder.
        if let Some(search) = &mut self.search
            && search.has_query()
        {
            search.mark_dirty();
            return;
        }
        let mut listing = match crate::fs::read_dir(&self.cwd, self.show_hidden, self.folders_first, &self.sort) {
            Ok(listing) => listing,
            Err(e) => { tracing::error!("reload_dir: {e}"); return; }
        };

        if self.view_mode == ViewMode::Details && !self.expanded.is_empty() {
            listing.entries = self.with_expanded(listing.entries, 0);
        }
        if self.grouped() {
            listing.entries = crate::ui::groups::arrange(listing.entries, self.sort.field, self.sort.ascending);
        }
        self.dir_counts.clear();
        self.icons_grace_until = std::time::Instant::now() + std::time::Duration::from_millis(300);

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

    /// Insert the contents of expanded folders after each one (Details tree).
    fn with_expanded(&self, entries: Vec<DirEntry>, depth: u16) -> Vec<DirEntry> {
        let mut out = Vec::with_capacity(entries.len());
        for mut entry in entries {
            entry.depth = depth;
            let open = entry.is_dir && depth < 32 && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            out.push(entry);
            if open
                && let Ok(children) = crate::fs::read_dir(&path, self.show_hidden, self.folders_first, &self.sort)
            {
                out.extend(self.with_expanded(children.entries, depth + 1));
            }
        }
        out
    }

    /// Expand or collapse a folder in the Details tree.
    pub fn toggle_expand(&mut self, idx: usize) {
        let Some(entry) = self.listing.entries.get(idx) else { return; };
        if !entry.is_dir {
            return;
        }
        let path = entry.path.clone();
        if self.expanded.contains(&path) {
            self.expanded.retain(|p| !p.starts_with(&path));
        } else {
            self.expanded.insert(path);
        }
        self.reload_dir();
    }

    /// Details tree: Right expands a folder (or steps into it), Left
    /// collapses it (or steps out to its parent). Returns false when the
    /// key doesn't apply, so it can move the selection as usual.
    pub fn tree_key(&mut self, expand: bool) -> bool {
        let Some(idx) = self.selection else { return false; };
        let Some(entry) = self.listing.entries.get(idx) else { return false; };
        let open = self.expanded.contains(&entry.path);
        match (expand, entry.is_dir, open) {
            (true, true, false) | (false, true, true) => self.toggle_expand(idx),
            (true, true, true) => {
                if self.listing.entries.get(idx + 1).is_some_and(|e| e.depth > entry.depth) {
                    self.selection = Some(idx + 1);
                }
            }
            (false, _, _) if entry.depth > 0 => {
                let depth = entry.depth;
                self.selection = self.listing.entries[..idx].iter().rposition(|e| e.depth < depth);
            }
            _ => return false,
        }
        self.multi_selection.clear();
        self.ensure_selection_visible();
        true
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
        self.remember_selection();
        let old = std::mem::replace(&mut self.cwd, path);
        self.history_back.push(old);
        self.history_forward.clear();
        self.after_navigate();
    }

    /// Cut and waiting to be pasted: drawn faded, like Dolphin.
    pub fn is_cut(&self, path: &std::path::Path) -> bool {
        self.clipboard.as_ref().is_some_and(|c| c.op == crate::events::ClipOp::Cut && c.paths.iter().any(|p| p == path))
    }

    /// Go to `path`'s folder with it selected (search results' Open Path).
    pub fn reveal(&mut self, path: &std::path::Path) {
        let Some(parent) = path.parent() else { return; };
        if parent == self.cwd {
            self.close_search();
        } else {
            self.navigate_to(parent.to_path_buf());
        }
        if let Some(idx) = self.listing.entries.iter().position(|e| e.path == path) {
            self.multi_selection.clear();
            self.selection = Some(idx);
            self.selection_anchor = Some(idx);
            self.ensure_selection_visible();
        }
    }

    pub fn navigate_back(&mut self) {
        if let Some(prev) = self.history_back.pop() {
            self.remember_selection();
            let current = std::mem::replace(&mut self.cwd, prev);
            self.history_forward.push(current.clone());
            self.after_navigate();
            self.restore_selection(&current, false);
        }
    }

    pub fn navigate_forward(&mut self) {
        if let Some(next) = self.history_forward.pop() {
            self.remember_selection();
            let current = std::mem::replace(&mut self.cwd, next);
            self.history_back.push(current.clone());
            self.after_navigate();
            self.restore_selection(&current, false);
        }
    }

    pub fn navigate_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let child = self.cwd.clone();
            self.navigate_to(parent);
            self.restore_selection(&child, true);
        }
    }

    fn remember_selection(&mut self) {
        if self.search.as_ref().is_some_and(|s| s.has_query()) {
            return;
        }
        match self.selection.and_then(|i| self.listing.entries.get(i)) {
            Some(e) => { self.left_selection.insert(self.cwd.clone(), e.path.clone()); }
            None => { self.left_selection.remove(&self.cwd); }
        }
    }

    /// Like Dolphin, coming back to a folder selects what was selected
    /// there, or else the folder just left (that first when going up).
    fn restore_selection(&mut self, came_from: &std::path::Path, prefer_from: bool) {
        let find = |p: &std::path::Path| self.listing.entries.iter().position(|e| e.path == p);
        let left = self.left_selection.get(&self.cwd).and_then(|w| find(w));
        let from = find(came_from);
        let idx = if prefer_from { from.or(left) } else { left.or(from) };
        if let Some(idx) = idx {
            self.selection = Some(idx);
            self.selection_anchor = Some(idx);
            self.ensure_selection_visible();
        }
    }

    fn after_navigate(&mut self) {
        self.search = None;
        self.expanded.clear();
        self.reset_scroll();
        self.selection = None;
        self.multi_selection.clear();
        self.selection_anchor = None;
        self.click_tracker.reset();
        self.apply_dir_state();
        self.reload_dir();
    }

    /// View settings for the current folder: the global defaults from the
    /// config, overridden by whatever was saved for this folder (Dolphin's
    /// per-folder view properties).
    fn apply_dir_state(&mut self) {
        let state = self.dirstates.get(&self.cwd).cloned().unwrap_or_default();
        let c = &self.config;
        self.view_mode = state.view.unwrap_or(c.default_view);
        self.sort = SortState {
            field: state.sort_field.unwrap_or(c.sort.field),
            ascending: state.sort_ascending.unwrap_or(c.sort.ascending),
        };
        self.folders_first = state.folders_first.unwrap_or(c.folders_first);
        self.show_hidden = state.show_hidden.unwrap_or(c.show_hidden);
        self.show_groups = state.groups.unwrap_or(c.show_in_groups);
        // Folders without their own zoom follow the home folder's
        let home_size = dirs::home_dir().and_then(|h| self.dirstates.get(&h)).and_then(|s| s.icon_size_idx);
        let idx = state.icon_size_idx.or(home_size).unwrap_or(c.icon_size_idx).min(ICON_LEVELS.len() - 1);
        self.set_icon_size(idx);
    }

    /// Move a place to another position in the list and save it.
    pub fn move_place(&mut self, from: usize, to: usize) {
        if from == to || from >= self.places.len() || to >= self.places.len() {
            return;
        }
        let place = self.places.remove(from);
        self.places.insert(to, place);
        if from < self.config.places.len() && to < self.config.places.len() {
            let c = self.config.places.remove(from);
            self.config.places.insert(to, c);
            let _ = self.config.save();
        }
    }

    /// Open a KDE Connect device's storage, mounting it in the background
    /// first (takes a few seconds).
    /// Quit, unless a file operation is running: then the first press warns
    /// and a second within a few seconds cancels it and quits.
    pub fn confirm_quit(&mut self) -> bool {
        if !self.fileops.busy() || self.quit_armed.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(4)) {
            self.fileops.cancel();
            return true;
        }
        self.quit_armed = Some(std::time::Instant::now());
        self.notify("A file operation is still running: quit again to cancel it and quit".into());
        false
    }

    /// Unmount a device in the background (and power the drive off with
    /// `power_off`), leaving it first if we're inside.
    pub fn unmount_device(&mut self, i: usize, power_off: bool) {
        let Some(dev) = self.devices.get(i).filter(|d| d.can_unmount()).cloned() else { return; };
        if self.cwd.starts_with(&dev.path) {
            self.navigate_to(dirs::home_dir().unwrap_or_else(|| "/".into()));
        }
        self.notify(format!("Unmounting {}\u{2026}", dev.name));
        let tx = self.unmount_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((dev.name.clone(), crate::fs::mounts::unmount(&dev, power_off)));
        });
    }

    pub fn notify(&mut self, text: String) {
        self.notice = Some((text, std::time::Instant::now() + std::time::Duration::from_secs(8)));
    }

    pub fn open_phone(&mut self, i: usize) {
        let Some(device) = self.kdeconnect_devices.get(i).cloned() else { return; };
        if !device.reachable {
            return;
        }
        if let Some(path) = self.kdeconnect_mounts.get(&device.id).filter(|p| p.is_dir()).cloned() {
            self.navigate_to(path);
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.kdeconnect_mount_rx = Some(rx);
        std::thread::spawn(move || {
            let mount = crate::user_actions::kdeconnect_mount(&device);
            let _ = tx.send((device.id, mount));
        });
    }

    /// Save the current sort/filter settings for this folder and re-list.
    fn save_sort_state(&mut self) {
        let (sort, folders_first, show_hidden) = (self.sort.clone(), self.folders_first, self.show_hidden);
        self.dirstates.update(&self.cwd, |s| {
            s.sort_field = Some(sort.field);
            s.sort_ascending = Some(sort.ascending);
            s.folders_first = Some(folders_first);
            s.show_hidden = Some(show_hidden);
        });
        self.reload_dir();
    }

    pub fn set_sort_field(&mut self, field: SortField) {
        self.sort.field = field;
        self.save_sort_state();
    }

    pub fn set_sort_ascending(&mut self, ascending: bool) {
        self.sort.ascending = ascending;
        self.save_sort_state();
    }

    pub fn toggle_folders_first(&mut self) {
        self.folders_first = !self.folders_first;
        self.save_sort_state();
    }

    /// Whether the listing is laid out in groups (Details, not searching).
    pub fn grouped(&self) -> bool {
        self.show_groups && self.view_mode == ViewMode::Details
            && !self.search.as_ref().is_some_and(|s| s.has_query())
    }

    pub fn toggle_groups(&mut self) {
        self.show_groups = !self.show_groups;
        let on = self.show_groups;
        self.dirstates.update(&self.cwd, |s| s.groups = Some(on));
        self.reload_dir();
    }

    /// The Details line showing entry `idx`.
    pub fn details_line(&self, idx: usize) -> usize {
        use crate::ui::groups::DRow;
        self.details_rows.iter().position(|r| *r == DRow::Entry(idx)).unwrap_or(idx)
    }

    pub fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.save_sort_state();
    }

    /// Set view mode and persist it for the current directory.
    pub fn set_view_mode(&mut self, view: ViewMode) {
        if view != self.view_mode {
            self.view_mode = view;
            // The tree and groups only exist in Details
            if !self.expanded.is_empty() || self.show_groups {
                self.reload_dir();
            }
        }
        self.dirstates.update(&self.cwd, |s| s.view = Some(view));
    }

    fn set_icon_size(&mut self, idx: usize) {
        if idx == self.icon_size_idx { return; }
        self.icon_size_idx = idx;
        let (w, h, r, iw) = icon_dims(idx, self.picker.font_size());
        self.cell_w = w;
        self.cell_h = h;
        self.icon_rows = r;
        self.img_w = iw;
        // Icon raster size changed: re-rasterize icons; thumbnails only need
        // re-encoding. Wipe placements after the next draw.
        self.icon_cache.clear();
        self.thumb_cache.clear_protocols();
        self.kitty_delete_all = true;
        // Rows changed height: re-derive the position from the grid row
        self.scroll_pos = (self.scroll_offset * h as usize) as f32;
        self.scroll_target = self.scroll_pos;
        self.scroll_sub = 0;
    }

    /// Step icon size up (+1) or down (-1) and persist per-dir.
    pub fn resize_icons(&mut self, delta: i8) {
        let new_idx = (self.icon_size_idx as i8 + delta)
            .clamp(0, ICON_LEVELS.len() as i8 - 1) as usize;
        if new_idx != self.icon_size_idx {
            self.set_icon_size(new_idx);
            self.dirstates.update(&self.cwd, |s| s.icon_size_idx = Some(new_idx));
        }
    }

    /// Pixel size to render icons at.
    pub fn icon_target_px(&self) -> u32 {
        // The size the grid draws them (icons::fit_to_cells), so a
        // frame full of new icons doesn't resize each one
        let fs = self.picker.font_size();
        let img_w = icon_dims(self.icon_size_idx, fs).3 as u32;
        let max_h = ((fs.height as u32 * self.icon_rows as u32) as f32 * crate::icons::GRID_FIT.max_h) as u32;
        max_h.min(img_w * fs.width as u32).clamp(32, 512)
    }

    /// Return the freedesktop icon name for XDG user directories (Music, Videos, etc.).
    pub fn xdg_folder_icon(path: &std::path::Path) -> Option<String> {
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
        if self.view_mode == ViewMode::Compact {
            return;
        }
        let target_px = self.icon_target_px();
        let (cols, rows_visible) = match self.view_mode {
            ViewMode::Details => (1, self.grid_area.height as usize),
            _ => (self.grid_cols.max(1) as usize, (self.grid_area.height / self.cell_h + 2) as usize),
        };
        let (mut start, mut end) = (self.scroll_offset * cols, ((self.scroll_offset + rows_visible) * cols).min(self.listing.entries.len()));
        // Details lines include group headers: take the entries they show
        if self.view_mode == ViewMode::Details && !self.details_rows.is_empty() {
            use crate::ui::groups::DRow;
            let shown = || self.details_rows.iter().skip(self.scroll_offset).take(rows_visible)
                .filter_map(|r| if let DRow::Entry(i) = r { Some(*i) } else { None });
            if let (Some(a), Some(b)) = (shown().min(), shown().max()) {
                (start, end) = (a, (b + 1).min(self.listing.entries.len()));
            }
        }
        if start >= end { return; }

        for i in start..end {
            let entry = &self.listing.entries[i];
            let mime = entry.mime_type.clone();
            let is_media = mime.as_deref().is_some_and(|m| self.previews.covers(m))
                && !self.thumb_cache.failed(&entry.path);

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

        // Then warm up the visible folders' contents
        if self.prefetched_px != target_px {
            self.prefetched.clear();
            self.prefetched_px = target_px;
        }
        for i in start..end {
            let entry = &self.listing.entries[i];
            if entry.is_dir && self.prefetched.insert(entry.path.clone()) {
                let _ = self.prefetch_tx.send((entry.path.clone(), self.show_hidden));
            }
        }
    }

    /// Queue a listed folder's first items as low-priority loads.
    fn prefetch_children(&mut self, children: Vec<PrefetchItem>) {
        let target_px = self.icon_target_px();
        let mut thumbs = 0;
        for (path, is_dir, mime) in children {
            match mime {
                // Thumbnails have no low-priority lane: only a few per folder
                Some(m) if self.previews.covers(&m) => {
                    if thumbs < 8 && !self.thumb_cache.failed(&path) {
                        self.thumb_cache.request(&path, &m);
                        thumbs += 1;
                    }
                }
                _ => {
                    let icon_override = if is_dir {
                        self.config.folder_icons.get(path.to_string_lossy().as_ref()).cloned()
                            .or_else(|| Self::xdg_folder_icon(&path))
                    } else {
                        None
                    };
                    self.icon_cache.prefetch(&path, is_dir, target_px, icon_override);
                }
            }
        }
    }

    /// Drain all background channels.
    pub fn drain_channels(&mut self) {
        while let Ok(children) = self.prefetch_rx.try_recv() {
            self.prefetch_children(children);
        }
        for report in self.fileops.poll() {
            self.notify(report.message());
            self.reload_dir();
        }
        self.sidebar_icons.drain_ready();
        let mime_updates = self.icon_cache.drain_ready();
        for (path, mime) in mime_updates {
            if let Some(entry) = self.listing.entries.iter_mut().find(|e| e.path == path)
                && entry.mime_type.is_none() {
                    entry.mime_type = mime;
                }
        }

        self.thumb_cache.drain_ready();

        // Coalesce bursts of events into a single reload
        if self.dir_event_rx.try_iter().count() > 0
            && !self.search.as_ref().is_some_and(|s| s.has_query())
        {
            self.reload_dir();
        }

        self.poll_search();

        if let Some(devices) = self.kdeconnect_rx.try_iter().last() {
            self.kdeconnect_devices = devices;
        }

        if let Ok(ev) = self.mount_event_rx.try_recv() {
            let MountEvent::Updated(devs) = ev;
            self.devices = devs;
        }
        while let Ok((name, result)) = self.unmount_rx.try_recv() {
            match result {
                Ok(()) => self.notify(format!("{name} can now be safely removed")),
                Err(e) => self.notify(format!("Couldn't unmount {name}: {e}")),
            }
            if let Ok(devs) = crate::fs::mounts::parse_mountinfo() {
                self.devices = devs;
            }
        }

        if let Some(rx) = &self.kdeconnect_mount_rx
            && let Ok((id, mount)) = rx.try_recv()
        {
            self.kdeconnect_mount_rx = None;
            match mount {
                Ok(path) => {
                    self.kdeconnect_mounts.insert(id, path.clone());
                    self.navigate_to(path);
                }
                Err(why) => self.notify(format!("Can't browse the phone: {why}")),
            }
        }

        if let Some(ref mut props) = self.properties {
            props.poll();
        }
    }

    /// Open the search bar, or focus it if already open.
    pub fn open_search(&mut self) {
        match &mut self.search {
            Some(s) => s.focused = true,
            None => self.search = Some(SearchState::new(self.cwd.clone())),
        }
    }

    pub fn close_search(&mut self) {
        if self.search.take().is_some_and(|s| s.has_query()) {
            self.reset_scroll();
            self.reload_dir();
        }
    }

    /// Search query/options changed: re-run, or show the folder again if
    /// the query was cleared.
    pub fn search_changed(&mut self) {
        let Some(search) = &mut self.search else { return; };
        search.mark_dirty();
        if !search.has_query() {
            search.cancel();
            // reload_dir lists the folder only while no query is active
            let s = self.search.take();
            self.reload_dir();
            self.search = s;
        }
    }

    fn poll_search(&mut self) {
        let show_hidden = self.show_hidden;
        let Some(search) = &mut self.search else { return; };
        let update = search.poll(show_hidden);
        // Cleared query: the folder listing is back; drop late results
        if !search.has_query() {
            return;
        }
        let entries = match update {
            SearchUpdate::None => return,
            SearchUpdate::Replace(entries) => {
                self.reset_scroll();
                self.selection = None;
                self.multi_selection.clear();
                self.selection_anchor = None;
                entries
            }
            SearchUpdate::Append(entries) => {
                let mut all = self.listing.entries.clone();
                all.extend(entries);
                all
            }
        };
        let mut listing = DirListing { entries };
        for entry in &mut listing.entries {
            if let Some(mime) = self.icon_cache.cached_mime(&entry.path) {
                entry.mime_type = mime.clone();
            }
        }
        crate::fs::sort_entries(&mut listing.entries, self.folders_first, &self.sort);
        let old = std::mem::replace(&mut self.listing, listing);
        self.remap_indices(&old);
    }

    /// Diff the images rendered this frame against the previous frame.
    /// Returns rects whose Kitty placements must be deleted (or a delete-all
    /// flag for bulk changes), and invalidates the affected protocols so they
    /// re-transmit on their next render.
    pub fn kitty_frame_diff(&mut self) -> (Vec<Rect>, bool) {
        let mut deletions: Vec<(PathBuf, Rect)> = Vec::new();

        for (path, (rect, bg, dim)) in &self.prev_frame_images {
            match self.frame_images.get(path) {
                // Unchanged, or moved with the same variant (ratatui-image
                // re-places moved images itself).
                Some((nr, nbg, ndim)) if nbg == bg && ndim == dim && nr == rect => {}
                Some((nr, _, _)) if *nr != *rect => {}
                // Variant changed (selection toggled) or image vanished
                // (scrolled off, dir changed, occluded by an overlay).
                _ => deletions.push((path.clone(), *rect)),
            }
        }

        let delete_all = self.kitty_delete_all;
        self.kitty_delete_all = false;
        self.prev_frame_images = std::mem::take(&mut self.frame_images);

        if delete_all {
            // Deleted along with everything else; re-placed next frame
            self.underlays.clear();
            self.band_uploaded = None;
            self.band_shown = None;
            self.sidebar_icons.clear_protocols();
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

    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
        self.scroll_pos = 0.0;
        self.scroll_target = 0.0;
        self.scroll_sub = 0;
    }

    fn max_scroll(&self) -> f32 {
        let rows = self.listing.entries.len().div_ceil(self.grid_cols.max(1) as usize);
        (rows as f32 * self.cell_h as f32 - self.grid_area.height as f32).max(0.0)
    }

    /// Scroll the Icons view by `rows` terminal rows (animated).
    pub fn scroll_by(&mut self, rows: f32) {
        self.scroll_target = (self.scroll_target + rows).clamp(0.0, self.max_scroll());
    }

    /// Advance the scroll animation one frame. Returns whether it's moving.
    pub fn tick_scroll(&mut self) -> bool {
        let ch = self.cell_h.max(1) as usize;
        if self.view_mode != ViewMode::Icons {
            self.scroll_sub = 0;
            self.scroll_seen = self.scroll_offset;
            self.scroll_pos = (self.scroll_offset * ch) as f32;
            self.scroll_target = self.scroll_pos;
            return false;
        }
        if self.scroll_offset != self.scroll_seen {
            self.scroll_pos = (self.scroll_offset * ch) as f32;
            self.scroll_target = self.scroll_pos;
        }
        self.scroll_target = self.scroll_target.clamp(0.0, self.max_scroll());
        let d = self.scroll_target - self.scroll_pos;
        let moving = d.abs() >= 0.5;
        self.scroll_pos = if moving { self.scroll_pos + d * 0.35 } else { self.scroll_target };
        let rows = self.scroll_pos.round().max(0.0) as usize;
        self.scroll_offset = rows / ch;
        self.scroll_sub = (rows % ch) as u16;
        self.scroll_seen = self.scroll_offset;
        moving
    }

    pub fn ensure_selection_visible(&mut self) {
        if self.view_mode == ViewMode::Icons {
            // Bring the whole item into view, animated
            let Some(idx) = self.selection else { return; };
            let ch = self.cell_h as f32;
            let top = (idx / self.grid_cols.max(1) as usize) as f32 * ch;
            let h = self.grid_area.height as f32;
            if top < self.scroll_target {
                self.scroll_target = top;
            } else if top + ch > self.scroll_target + h {
                self.scroll_target = (top + ch - h).max(0.0);
            }
            return;
        }
        let rows_visible = match self.view_mode {
            ViewMode::Details => self.grid_area.height.saturating_sub(1) as usize,
            ViewMode::Compact => (self.grid_area.height / crate::ui::compact::COMPACT_CELL_H).max(1) as usize,
            ViewMode::Icons => (self.grid_area.height / self.cell_h).max(1) as usize,
        };
        let cols = self.grid_cols.max(1) as usize;
        if let Some(idx) = self.selection {
            let row = match self.view_mode {
                ViewMode::Details => self.details_line(idx),
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
        // With the trailing slash the breadcrumbs show, ready for a child name
        let mut input = self.cwd.to_string_lossy().to_string();
        if !input.ends_with('/') {
            input.push('/');
        }
        self.path_bar_input = input;
        self.path_bar_cursor = self.path_bar_input.len();
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
                if copy {
                    self.fileops.copy(vec![src], &dst);
                } else {
                    self.fileops.move_to(vec![src], &dst);
                }
            }
    }
}

/// Background lister for prefetching: a folder's first items roughly as
/// they'll show (folders first, by name), with their guessed mime types.
fn spawn_prefetch_lister() -> (mpsc::Sender<(PathBuf, bool)>, mpsc::Receiver<Vec<PrefetchItem>>) {
    let (tx, rx) = mpsc::channel::<(PathBuf, bool)>();
    let (out_tx, out_rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((dir, show_hidden)) = rx.recv() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut items: Vec<(PathBuf, bool)> = rd.take(2000).flatten()
                .filter(|e| show_hidden || !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| { let p = e.path(); let d = p.is_dir(); (p, d) })
                .collect();
            items.sort_by_cached_key(|(p, d)| (!*d, p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
            let children = items.into_iter().take(48)
                .map(|(p, d)| { let m = if d { None } else { crate::mime::guess_by_name(&p) }; (p, d, m) })
                .collect();
            if out_tx.send(children).is_err() {
                break;
            }
        }
    });
    (tx, out_rx)
}

fn free_space(path: &std::path::Path) -> Option<u64> {
    let st = nix::sys::statvfs::statvfs(path).ok()?;
    Some(st.blocks_available() as u64 * st.fragment_size() as u64)
}



