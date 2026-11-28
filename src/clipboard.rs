//! The system clipboard through wl-copy/wl-paste.

use std::io::Write;
use std::process::{Command, Stdio};

/// Whether the session has a usable clipboard tool.
pub fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() && crate::check::has_command("wl-paste")
}

fn read_cmd(args: &[&str]) -> Option<Vec<u8>> {
    if !available() {
        return None;
    }
    // A stuck clipboard owner mustn't hang the UI
    let out = Command::new("timeout")
        .arg("2").arg("wl-paste").args(args)
        .stdin(Stdio::null()).stderr(Stdio::null())
        .output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The clipboard's MIME types (empty when there's nothing, or no tool).
pub fn types() -> Vec<String> {
    read_cmd(&["--list-types"])
        .map(|out| String::from_utf8_lossy(&out).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The clipboard's content as `mime`, or as plain text with None.
pub fn read(mime: Option<&str>) -> Option<Vec<u8>> {
    match mime {
        Some(m) => read_cmd(&["-t", m]),
        None => read_cmd(&["--no-newline"]),
    }
}

/// Put `data` on the clipboard as `mime` (plain text with None).
pub fn write(mime: Option<&str>, data: &[u8]) {
    if !available() {
        return;
    }
    let mut cmd = Command::new("wl-copy");
    if let Some(m) = mime {
        cmd.args(["-t", m]);
    }
    // wl-copy stays running to serve the clipboard; in a session of its
    // own, the copy survives this window closing
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = crate::launch::spawn_own_session(&mut cmd)
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(data);
        drop(stdin);
        std::thread::spawn(move || { let _ = child.wait(); });
    }
}
