//! The system clipboard through the desktop's command-line tools:
//! wl-copy/wl-paste on Wayland, xclip on X11, and the arboard crate
//! (the real pasteboard: text, pictures and Finder-compatible files) on macOS.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Wayland,
    X11,
    /// The macOS pasteboard, through arboard.
    Mac,
}

pub fn backend() -> Option<Backend> {
    if cfg!(target_os = "macos") {
        return arboard::Clipboard::new().is_ok().then_some(Backend::Mac);
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && crate::check::has_command("wl-paste") {
        Some(Backend::Wayland)
    } else if std::env::var_os("DISPLAY").is_some() && crate::check::has_command("xclip") {
        Some(Backend::X11)
    } else {
        None
    }
}

fn read_cmd(args: &[&str]) -> Option<Vec<u8>> {
    let (bin, base): (&str, &[&str]) = match backend()? {
        Backend::Wayland => ("wl-paste", &[]),
        Backend::X11 => ("xclip", &["-selection", "clipboard", "-o"]),
        Backend::Mac => return None, // handled by read_mac
    };
    // A stuck clipboard owner mustn't hang the UI
    let out = Command::new("timeout")
        .arg("2").arg(bin).args(base).args(args)
        .stdin(Stdio::null()).stderr(Stdio::null())
        .output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// macOS: what's on the pasteboard, as the MIME types the rest of the app asks for.
fn types_mac() -> Vec<String> {
    let Ok(mut cb) = arboard::Clipboard::new() else { return Vec::new(); };
    let mut types = Vec::new();
    if cb.get().file_list().is_ok_and(|l| !l.is_empty()) {
        types.push("text/uri-list".to_string());
    }
    if cb.get().text().is_ok_and(|t| !t.is_empty()) {
        types.push("text/plain".to_string());
    }
    // Decoding a picture is the dear check, so only when nothing else is there
    if types.is_empty() && cb.get().image().is_ok() {
        types.push("image/png".to_string());
    }
    types
}

/// macOS: the pasteboard's content as `mime` (None: plain text).
fn read_mac(mime: Option<&str>) -> Option<Vec<u8>> {
    let mut cb = arboard::Clipboard::new().ok()?;
    match mime {
        None | Some("text/plain") => cb.get().text().ok().map(String::into_bytes),
        Some("text/uri-list") => {
            let files = cb.get().file_list().ok().filter(|l| !l.is_empty())?;
            let list: String = files.iter().map(|p| crate::thumbs::file_uri(p) + "\r\n").collect();
            Some(list.into_bytes())
        }
        Some("image/png") => {
            let img = cb.get().image().ok()?;
            let rgba = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())?;
            let mut png = std::io::Cursor::new(Vec::new());
            rgba.write_to(&mut png, image::ImageFormat::Png).ok()?;
            Some(png.into_inner())
        }
        Some(_) => None,
    }
}

/// macOS: put `data` on the pasteboard as `mime` (None: plain text).
fn write_mac(mime: Option<&str>, data: &[u8]) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return; };
    let result = match mime {
        None => cb.set().text(String::from_utf8_lossy(data).into_owned()),
        Some("text/uri-list") => {
            let files = crate::dnd::parse_uri_list(&String::from_utf8_lossy(data));
            cb.set().file_list(&files)
        }
        Some(_) => return,
    };
    if let Err(e) = result {
        tracing::warn!("clipboard write: {e}");
    }
}

/// The clipboard's MIME types (empty when there's nothing, or no tool).
pub fn types() -> Vec<String> {
    let args: &[&str] = match backend() {
        Some(Backend::Wayland) => &["--list-types"],
        Some(Backend::X11) => &["-t", "TARGETS"],
        Some(Backend::Mac) => return types_mac(),
        None => return Vec::new(),
    };
    read_cmd(args)
        .map(|out| String::from_utf8_lossy(&out).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The clipboard's content as `mime`, or as plain text with None.
pub fn read(mime: Option<&str>) -> Option<Vec<u8>> {
    match (backend()?, mime) {
        (Backend::Wayland, Some(m)) => read_cmd(&["-t", m]),
        (Backend::Wayland, None) => read_cmd(&["--no-newline"]),
        (Backend::X11, Some(m)) => read_cmd(&["-t", m]),
        (Backend::X11, None) => read_cmd(&["-t", "UTF8_STRING"]),
        (Backend::Mac, m) => read_mac(m),
    }
}

/// Put `data` on the clipboard as `mime` (plain text with None).
pub fn write(mime: Option<&str>, data: &[u8]) {
    let mut cmd = match backend() {
        Some(Backend::Wayland) => {
            let mut c = Command::new("wl-copy");
            if let Some(m) = mime {
                c.args(["-t", m]);
            }
            c
        }
        Some(Backend::X11) => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-i"]);
            c.args(["-t", mime.unwrap_or("UTF8_STRING")]);
            c
        }
        Some(Backend::Mac) => return write_mac(mime, data),
        None => return,
    };
    // wl-copy/xclip stay running to serve the clipboard; in a session of
    // their own, the copy survives this window closing
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = crate::launch::spawn_own_session(&mut cmd)
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(data);
        drop(stdin);
        std::thread::spawn(move || { let _ = child.wait(); });
    }
}

