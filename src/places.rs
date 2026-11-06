use crate::config::PlaceConfig;
use crate::fs::mounts::DeviceEntry;

#[derive(Debug, Clone)]
pub struct PlaceEntry {
    pub name: String,
    pub path: String,
    pub icon: Option<String>,
}

impl From<&PlaceConfig> for PlaceEntry {
    fn from(c: &PlaceConfig) -> Self {
        Self {
            name: c.name.clone(),
            path: c.path.clone(),
            icon: c.icon.clone(),
        }
    }
}

pub fn places_from_config(configs: &[PlaceConfig]) -> Vec<PlaceEntry> {
    configs.iter().map(PlaceEntry::from).collect()
}

/// Until the mount watcher's first list (sent as it starts): reading
/// mounts checks each one's free space, which can hang on a dead network
/// mount, so that's never done on the main thread.
pub fn default_devices() -> Vec<DeviceEntry> {
    vec![DeviceEntry::new("Root".into(), "/".into())]
}
