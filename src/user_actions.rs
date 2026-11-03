//! User-defined context menu actions, one TOML file per group in
//! ~/.config/file-browser-tui/actions/. On first run, KDE service menus
//! (~/.local/share/kio/servicemenus/*.desktop) are imported into that format.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionGroup {
    /// Group the actions under this submenu instead of the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submenu: Option<String>,
    /// Mime patterns the actions apply to ("image/*", "inode/directory",
    /// "all/allfiles", "all/all"). Empty = everything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mime: Vec<String>,
    #[serde(rename = "action", default)]
    pub actions: Vec<UserAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAction {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Shell command. %f = each file (runs once per file), %F = all files
    /// (runs once), %d = containing directory. %u/%U work like %f/%F.
    pub command: String,
}

const README: &str = "\
# Context menu actions

Each *.toml file here adds actions to the right-click menu. Changes apply
the next time a menu opens.

    submenu = \"Convert\"        # optional: group under a submenu
    mime = [\"video/*\"]         # optional: image/*, inode/directory,
                               # all/allfiles (files only), all/all

    [[action]]
    label = \"Convert to webm\"
    icon = \"video\"             # optional freedesktop icon name
    command = \"ffmpeg -i %f -y %f.webm\"

Placeholders (paths are shell-quoted for you):
    %f  one file; the command runs once per selected file
    %F  all selected files; the command runs once
    %d  the directory containing the file
    %u / %U  same as %f / %F (KDE service menu compatibility)
    %%  a literal %; any other %x is left as is

Right-clicking the folder background targets the current folder
(mime inode/directory). Files imported from KDE service menus note their
source on the first line.
";

pub fn actions_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("file-browser-tui/actions")
}

/// Load all action groups, importing KDE service menus the first time.
pub fn load() -> Vec<ActionGroup> {
    let dir = actions_dir();
    if !dir.exists()
        && let Err(e) = migrate_kio(&dir)
    {
        tracing::warn!("service menu import: {e}");
    }

    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new(); };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();

    files.iter().filter_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        toml::from_str(&text)
            .map_err(|e| tracing::warn!("{}: {e}", p.display()))
            .ok()
    }).collect()
}

impl ActionGroup {
    pub fn applies_to(&self, mime: &str) -> bool {
        let is_dir = mime == "inode/directory";
        self.mime.is_empty() || self.mime.iter().any(|pat| match pat.as_str() {
            "all/all" | "*" | "*/*" => true,
            "all/allfiles" => !is_dir,
            p => match p.strip_suffix("/*") {
                Some(major) => mime.split('/').next() == Some(major),
                None => p == mime || (is_dir && p.starts_with("inode/directory")),
            },
        })
    }
}

/// Run `action` against `paths`, from `cwd`.
pub fn run(action: &UserAction, paths: &[PathBuf], cwd: &Path) {
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let all = paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
    let cmd = &action.command;
    let per_file = cmd.contains("%f") || cmd.contains("%u");

    let expand = |file: &Path| {
        let dir = if file.is_dir() { file } else { file.parent().unwrap_or(cwd) };
        let mut out = String::with_capacity(cmd.len());
        let mut chars = cmd.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('f' | 'u') => out.push_str(&quote(file)),
                Some('F' | 'U') => out.push_str(&all),
                Some('d') => out.push_str(&quote(dir)),
                Some('%') => out.push('%'),
                // Anything else stays literal (e.g. printf's %s)
                Some(other) => { out.push('%'); out.push(other); }
                None => out.push('%'),
            }
        }
        out
    };

    let targets: Vec<&PathBuf> = if per_file { paths.iter().collect() } else { paths.iter().take(1).collect() };
    for file in targets {
        let line = expand(file);
        tracing::info!("user action {:?}: {line}", action.label);
        let _ = Command::new("sh")
            .args(["-c", &line])
            .current_dir(cwd)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
}

// ---------------------------------------------------------------------------
// KDE service menu import
// ---------------------------------------------------------------------------

fn kio_dirs() -> Vec<PathBuf> {
    let Some(data) = dirs::data_dir() else { return Vec::new(); };
    vec![data.join("kio/servicemenus"), data.join("kservices5/ServiceMenus")]
}

fn migrate_kio(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("README.md"), README)?;

    for src_dir in kio_dirs() {
        let Ok(rd) = std::fs::read_dir(&src_dir) else { continue; };
        for path in rd.filter_map(|e| e.ok().map(|e| e.path())) {
            if path.extension().is_none_or(|x| x != "desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue; };
            let Some(group) = parse_service_menu(&text) else {
                tracing::info!("skipping service menu {}", path.display());
                continue;
            };
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let out = dir.join(format!("{stem}.toml"));
            if out.exists() {
                continue;
            }
            let body = format!("# Imported from {}\n\n{}", path.display(), toml::to_string_pretty(&group)?);
            std::fs::write(&out, body)?;
            tracing::info!("imported service menu {} -> {}", path.display(), out.display());
        }
    }
    Ok(())
}

/// Convert a KDE service menu .desktop file into an action group.
fn parse_service_menu(text: &str) -> Option<ActionGroup> {
    // section name -> key -> value (unlocalized keys only)
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.contains('[') {
                sections.entry(current.clone()).or_default()
                    .entry(k.to_string()).or_insert_with(|| unescape(v.trim()));
            }
        }
    }

    let entry = sections.get("Desktop Entry")?;
    let list = |v: Option<&String>| -> Vec<String> {
        v.map(|s| s.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default()
    };

    let actions: Vec<UserAction> = list(entry.get("Actions")).iter().filter_map(|id| {
        let a = sections.get(&format!("Desktop Action {id}"))?;
        Some(UserAction {
            label: a.get("Name")?.clone(),
            icon: a.get("Icon").cloned(),
            command: a.get("Exec")?.clone(),
        })
    }).collect();
    if actions.is_empty() {
        return None;
    }

    // Dolphin puts non-TopLevel service menus under an "Actions" submenu
    let top_level = entry.get("X-KDE-Priority").is_some_and(|p| p == "TopLevel");
    let submenu = entry.get("X-KDE-Submenu").cloned()
        .or_else(|| (!top_level).then(|| "Actions".to_string()));

    Some(ActionGroup { submenu, mime: list(entry.get("MimeType")), actions })
}

/// Desktop-entry value escapes: \s \n \t \\.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => { out.push('\\'); out.push(other); }
            None => out.push('\\'),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// KDE Connect
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct KdeConnectDevice {
    pub id: String,
    pub name: String,
}

/// Poll reachable KDE Connect devices in the background (no-op if
/// kdeconnect-cli isn't installed), so menus never block on D-Bus.
pub fn spawn_kdeconnect_watcher(tx: std::sync::mpsc::Sender<Vec<KdeConnectDevice>>) {
    std::thread::spawn(move || loop {
        let out = match Command::new("kdeconnect-cli")
            .args(["-a", "--id-name-only"])
            .stdin(Stdio::null()).stderr(Stdio::null())
            .output()
        {
            Ok(o) => o,
            Err(_) => return, // not installed
        };
        let devices = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
            let (id, name) = l.trim().split_once(' ')?;
            Some(KdeConnectDevice { id: id.to_string(), name: name.trim().to_string() })
        }).collect();
        if tx.send(devices).is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}

pub fn kdeconnect_share(device: &KdeConnectDevice, paths: &[PathBuf]) {
    for p in paths {
        let _ = Command::new("kdeconnect-cli")
            .args(["-d", &device.id, "--share"])
            .arg(p)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_menu() {
        let g = parse_service_menu(
            "[Desktop Entry]\nType=Service\nMimeType=video/*\nActions=a;b\nX-KDE-Submenu=Convert\n\n\
             [Desktop Action a]\nName=To webm\nName[de]=Nach webm\nExec=ffmpeg -i %u -y %u.webm\n\n\
             [Desktop Action b]\nName=To mp4\nExec=ffmpeg -i %u -y %u.mp4\n",
        ).unwrap();
        assert_eq!(g.submenu.as_deref(), Some("Convert"));
        assert_eq!(g.actions.len(), 2);
        assert_eq!(g.actions[0].label, "To webm");
        assert!(g.applies_to("video/mp4"));
        assert!(!g.applies_to("image/png"));
    }

    #[test]
    fn mime_patterns() {
        let g = |m: &[&str]| ActionGroup { submenu: None, mime: m.iter().map(|s| s.to_string()).collect(), actions: vec![] };
        assert!(g(&["all/allfiles"]).applies_to("text/plain"));
        assert!(!g(&["all/allfiles"]).applies_to("inode/directory"));
        assert!(g(&["inode/directory", "inode/directory-locked"]).applies_to("inode/directory"));
        assert!(g(&[]).applies_to("inode/directory"));
    }
}
