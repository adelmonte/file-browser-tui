use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Resolve icon names + mime type for a path without spawning a process.
/// Falls back to `gio info` only for extensionless regular files.
pub fn resolve_names_and_mime(path: &Path, is_dir: bool) -> (Vec<String>, Option<String>) {
    if is_dir {
        return (vec!["folder".into()], Some("inode/directory".into()));
    }

    match crate::mime::detect(path) {
        Some(mime) => (icon_names_for_mime(&mime), Some(mime)),
        // No name match (extensionless etc.): ask gio, which sniffs content
        None => query_gio_info(path),
    }
}

/// Map a mime type to freedesktop icon name candidates, most specific first.
fn icon_names_for_mime(mime: &str) -> Vec<String> {
    let major = mime.split('/').next().unwrap_or("");

    let mut names = crate::mime::icon_names(mime);

    // Common aliases that themes actually ship
    match mime {
        "application/pdf" => names.push("application-pdf".into()),
        "application/zip" | "application/x-tar" | "application/gzip"
        | "application/x-xz" | "application/x-bzip2" | "application/zstd"
        | "application/x-7z-compressed" | "application/x-rar-compressed" =>
            names.push("package-x-generic".into()),
        "text/markdown" => names.push("text-x-markdown".into()),
        _ => {}
    }

    match major {
        "image" => names.push("image-x-generic".into()),
        "video" => names.push("video-x-generic".into()),
        "audio" => names.push("audio-x-generic".into()),
        "text"  => names.push("text-x-generic".into()),
        "font"  => names.push("font-x-generic".into()),
        _       => names.push("application-x-generic".into()),
    }

    names
}

/// Returns (icon_names, mime_type) from `gio info` for the given path.
pub fn query_gio_info(path: &Path) -> (Vec<String>, Option<String>) {
    let output = Command::new("gio")
        .args(["info", "-a", "standard::icon,standard::content-type",
               path.to_string_lossy().as_ref()])
        .stderr(Stdio::null())
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("gio info failed: {e}");
            return (Vec::new(), None);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut icon_names = Vec::new();
    let mut mime_type = None;

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("standard::icon:") {
            // val like " folder-documents, folder, folder-symbolic"
            icon_names = val
                .split([',', ' '])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !s.ends_with("-symbolic"))
                .collect();
        } else if let Some(val) = line.strip_prefix("standard::content-type:") {
            mime_type = Some(val.trim().to_string());
        }
    }

    (icon_names, mime_type)
}

/// Resolve icon names to a file through the user's icon theme and the
/// themes it inherits from (see `theme`). Returns (path, is_svg).
pub fn resolve_icon_path(icon_names: &[String], is_dir: bool) -> Option<(PathBuf, bool)> {
    icon_names.iter()
        .find_map(|n| super::theme::lookup(n))
        .or_else(|| super::theme::lookup(if is_dir { "folder" } else { "text-x-generic" }))
}

/// A custom folder icon set in Dolphin (Icon= in the folder's .directory).
pub fn directory_icon(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".directory")).ok()?;
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some(v) = line.strip_prefix("Icon=") {
            return Some(v.trim().to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

