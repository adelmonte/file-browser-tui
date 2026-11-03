use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::{SortField, ViewMode};

/// Per-directory view settings, persisted across sessions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DirState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_size_idx: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_field: Option<SortField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_ascending: Option<bool>,
}

#[derive(Debug, Default)]
pub struct DirStates {
    map: HashMap<String, DirState>,
    /// Set when the on-disk file failed to parse, so we never overwrite it.
    pub load_error: Option<String>,
}

impl DirStates {
    pub fn load() -> Self {
        let path = state_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default(); };
        match toml::from_str(&text) {
            Ok(map) => Self { map, load_error: None },
            Err(e) => {
                tracing::error!("dirstate {}: {e}", path.display());
                Self { map: HashMap::new(), load_error: Some(format!("{}: {e}", path.display())) }
            }
        }
    }

    pub fn save(&self) {
        if self.load_error.is_some() {
            return;
        }
        let path = state_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string_pretty(&self.map) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn get(&self, dir: &Path) -> Option<&DirState> {
        self.map.get(dir.to_string_lossy().as_ref())
    }

    pub fn set_view(&mut self, dir: &Path, view: ViewMode) {
        self.map.entry(dir.to_string_lossy().to_string()).or_default().view = Some(view);
        self.save();
    }

    pub fn set_icon_size(&mut self, dir: &Path, idx: usize) {
        self.map.entry(dir.to_string_lossy().to_string()).or_default().icon_size_idx = Some(idx);
        self.save();
    }

    pub fn set_sort(&mut self, dir: &Path, field: SortField, ascending: bool) {
        let entry = self.map.entry(dir.to_string_lossy().to_string()).or_default();
        entry.sort_field = Some(field);
        entry.sort_ascending = Some(ascending);
        self.save();
    }
}

fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("file-browser-tui/dirstate.toml")
}
