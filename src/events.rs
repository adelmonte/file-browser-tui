use std::path::PathBuf;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Permanent-delete confirmation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConfirmDeleteState {
    pub paths: Vec<PathBuf>,
    pub display_name: String,
}

// ---------------------------------------------------------------------------
// Clipboard (cut / copy / paste)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOp {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct Clipboard {
    pub paths: Vec<PathBuf>,
    pub op: ClipOp,
}

// ---------------------------------------------------------------------------
// Sidebar hit-testing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTarget {
    Place(usize),
    Device(usize),
    /// A paired KDE Connect device (index into `kdeconnect_devices`).
    Phone(usize),
}

// ---------------------------------------------------------------------------
// Drag state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub enum DragState {
    #[default]
    Idle,
    Armed {
        item_idx: usize,
        origin: (u16, u16),
    },
    Dragging {
        item_idx: usize,
        origin: (u16, u16),
        current_pos: (u16, u16),
        hover_target: Option<usize>,
    },
}

pub const DRAG_THRESHOLD: u16 = 2;

// ---------------------------------------------------------------------------
// Double-click tracker
// ---------------------------------------------------------------------------

const DOUBLE_CLICK_MS: u128 = 400;

#[derive(Debug, Default)]
pub struct ClickTracker {
    last_item: Option<usize>,
    last_time: Option<Instant>,
}

#[derive(Debug, PartialEq)]
pub enum ClickKind {
    Single,
    Double,
}

impl ClickTracker {
    pub fn record(&mut self, item: usize) -> ClickKind {
        let now = Instant::now();
        let is_double = self.last_item == Some(item)
            && self.last_time
                .map(|t| now.duration_since(t).as_millis() < DOUBLE_CLICK_MS)
                .unwrap_or(false);

        self.last_item = Some(item);
        self.last_time = Some(now);

        if is_double {
            // Reset so triple-click is two singles
            self.last_item = None;
            ClickKind::Double
        } else {
            ClickKind::Single
        }
    }

    /// Follow the last-clicked item to its new index after a listing reload.
    pub fn remap(&mut self, f: impl Fn(usize) -> Option<usize>) {
        self.last_item = self.last_item.and_then(f);
    }

    pub fn reset(&mut self) {
        self.last_item = None;
        self.last_time = None;
    }
}

// ---------------------------------------------------------------------------
// Inline rename state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RenameState {
    pub item_idx: usize,
    pub input: String,
    pub cursor_pos: usize,
    /// Last commit was rejected (bad name or target exists); cleared on edit.
    pub invalid: bool,
}

impl RenameState {
    pub fn new(item_idx: usize, current_name: &str) -> Self {
        let len = current_name.len();
        Self {
            item_idx,
            input: current_name.to_string(),
            cursor_pos: len,
            invalid: false,
        }
    }
}
