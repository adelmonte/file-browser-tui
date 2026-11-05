use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::{SortField, ViewMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_places")]
    pub places: Vec<PlaceConfig>,
    #[serde(default)]
    pub default_view: ViewMode,
    #[serde(default)]
    pub sort: SortConfig,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_true")]
    pub folders_first: bool,
    /// Details view: group rows under headers, like Dolphin's Show in Groups.
    #[serde(default = "default_true")]
    pub show_in_groups: bool,
    #[serde(default = "default_icon_size")]
    pub icon_size_idx: usize,
    #[serde(default = "default_sidebar_w")]
    pub sidebar_w: u16,
    /// Terminal emulator for "Open in terminal". Empty = $TERMINAL, fallback kitty.
    #[serde(default)]
    pub terminal_command: String,
    /// Custom folder icon names keyed by absolute directory path.
    #[serde(default)]
    pub folder_icons: HashMap<String, String>,
    #[serde(default)]
    pub context_menu: ContextMenuConfig,
    /// Show previews of a folder's images/videos on its icon, Dolphin-style.
    #[serde(default = "default_true")]
    pub folder_previews: bool,
    /// Which file types show a preview of their content, by Dolphin's
    /// thumbnailer plugin names: imagethumbnail, jpegthumbnail,
    /// svgthumbnail, ffmpegthumbs (video), audiothumbnail (album art),
    /// ebookthumbnail (EPUB covers), opendocumentthumbnail,
    /// comicbookthumbnail (CBZ), gsthumbnail (PDF first page, needs
    /// pdftoppm). Unset: follow Dolphin's own setting, or its defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_plugins: Option<Vec<String>>,
    /// Interface colors by role as "#rrggbb" (see ui/theme.rs); unset
    /// roles keep the Nord defaults. Kept as written, so it round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<toml::Value>,
    /// Keyboard shortcuts by action name, e.g. `rename = ["F2", "Alt+R"]`;
    /// replaces that action's defaults, `[]` unbinds it (see keys.rs).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub keys: HashMap<String, Vec<String>>,
    /// Set when the on-disk config failed to parse, so we never overwrite it.
    #[serde(skip)]
    read_only: bool,
}

fn default_icon_size() -> usize { crate::app::DEFAULT_ICON_LEVEL }
fn default_sidebar_w() -> u16 { 18 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextMenuConfig {
    #[serde(default)]
    pub custom: Vec<CustomMenuEntry>,
}

/// User-defined context menu entry.
/// command supports %f (quoted target path) and %d (quoted containing dir).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMenuEntry {
    pub label: String,
    pub command: String,
    /// Any of "file", "dir", "empty". Empty list = all.
    #[serde(default)]
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceConfig {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortConfig {
    #[serde(default)]
    pub field: SortField,
    #[serde(default = "default_true")]
    pub ascending: bool,
}

impl Default for SortConfig {
    fn default() -> Self {
        Self { field: SortField::default(), ascending: true }
    }
}

fn default_true() -> bool {
    true
}

fn default_places() -> Vec<PlaceConfig> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let home_str = home.to_string_lossy().to_string();

    let mut places = vec![
        PlaceConfig { name: "Home".into(),      path: home_str.clone(),                         icon: Some("user-home".into()) },
        PlaceConfig { name: "Desktop".into(),   path: format!("{}/Desktop", home_str),          icon: Some("user-desktop".into()) },
        PlaceConfig { name: "Documents".into(), path: format!("{}/Documents", home_str),        icon: Some("folder-documents".into()) },
        PlaceConfig { name: "Downloads".into(), path: format!("{}/Downloads", home_str),        icon: Some("folder-download".into()) },
        PlaceConfig { name: "Music".into(),     path: format!("{}/Music", home_str),            icon: Some("folder-music".into()) },
        PlaceConfig { name: "Pictures".into(),  path: format!("{}/Pictures", home_str),         icon: Some("folder-pictures".into()) },
        PlaceConfig { name: "Videos".into(),    path: format!("{}/Videos", home_str),           icon: Some("folder-videos".into()) },
    ];

    // Trash: freedesktop location
    if let Some(data) = dirs::data_dir() {
        places.push(PlaceConfig {
            name: "Trash".into(),
            path: data.join("Trash/files").to_string_lossy().to_string(),
            icon: Some("user-trash".into()),
        });
    }

    places
}

impl Default for Config {
    fn default() -> Self {
        Self {
            places: default_places(),
            default_view: ViewMode::Icons,
            sort: SortConfig::default(),
            show_hidden: false,
            folders_first: true,
            show_in_groups: true,
            icon_size_idx: default_icon_size(),
            sidebar_w: 18,
            terminal_command: String::new(),
            folder_icons: HashMap::new(),
            context_menu: ContextMenuConfig::default(),
            folder_previews: true,
            preview_plugins: None,
            keys: HashMap::new(),
            theme: None,
            read_only: false,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        } else {
            Ok(Config::default())
        }
    }

    /// Defaults that will not be written back (used when loading failed).
    pub fn unwritable() -> Self {
        Self { read_only: true, ..Self::default() }
    }

    pub fn save(&self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("file-browser-tui/config.toml")
}

