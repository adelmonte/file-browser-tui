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

/// Resolve an icon name to a filesystem path by searching icon themes.
/// Returns (path, is_svg).
pub fn resolve_icon_path(icon_names: &[String], is_dir: bool) -> Option<(PathBuf, bool)> {
    let theme_dirs: &[(&str, &[&str])] = &[
        ("/usr/share/icons/Tela-nord-dark", &["256x256", "scalable"]),
        ("/home/user/.local/share/icons/Tela-nord-dark", &["256x256", "scalable"]),
        ("/usr/share/icons/Papirus-Dark", &["64x64", "48x48"]),
        ("/usr/share/icons/hicolor", &["scalable", "48x48"]),
    ];

    let contexts = &["places", "apps", "mimetypes", "devices", "filesystems"];

    for name in icon_names {
        for (theme_root, size_dirs) in theme_dirs {
            for size_dir in *size_dirs {
                let is_scalable = *size_dir == "scalable";
                for ctx in contexts {
                    let base = PathBuf::from(theme_root).join(size_dir).join(ctx);

                    // Try PNG first (Tela-nord-dark 256x256 has PNGs)
                    let png = base.join(format!("{name}.png"));
                    if png.exists() {
                        return Some((png, false));
                    }

                    // Try SVG
                    if is_scalable || *size_dir != "256x256" {
                        let svg = base.join(format!("{name}.svg"));
                        if svg.exists() {
                            return Some((svg, true));
                        }
                    }
                    // For non-scalable dirs that might have SVGs (Papirus)
                    let svg = base.join(format!("{name}.svg"));
                    if svg.exists() {
                        return Some((svg, true));
                    }
                }
            }
        }
    }

    // Fallback
    fallback_icon(is_dir)
}

fn fallback_icon(is_dir: bool) -> Option<(PathBuf, bool)> {
    let fallback_name = if is_dir { "folder" } else { "text-x-generic" };
    let theme_dirs = [
        "/usr/share/icons/Tela-nord-dark",
        "/usr/share/icons/Papirus-Dark",
        "/usr/share/icons/hicolor",
    ];
    let size_dirs = ["256x256", "scalable", "64x64", "48x48"];
    let contexts = ["places", "apps", "mimetypes"];

    for theme in &theme_dirs {
        for size in &size_dirs {
            for ctx in &contexts {
                let base = PathBuf::from(theme).join(size).join(ctx);
                let png = base.join(format!("{fallback_name}.png"));
                if png.exists() { return Some((png, false)); }
                let svg = base.join(format!("{fallback_name}.svg"));
                if svg.exists() { return Some((svg, true)); }
            }
        }
    }
    None
}


