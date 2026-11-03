use std::{
    os::unix::io::AsRawFd,
    sync::mpsc,
    thread,
    fs::File,
    io::{BufRead, BufReader},
};

use nix::poll::{PollFd, PollFlags, poll};

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug)]
pub enum MountEvent {
    Updated(Vec<DeviceEntry>),
}

pub fn spawn_mount_watcher(tx: mpsc::Sender<MountEvent>) {
    thread::spawn(move || {
        let file = match File::open("/proc/self/mountinfo") {
            Ok(f) => f,
            Err(e) => { tracing::error!("open mountinfo: {e}"); return; }
        };

        loop {
            let mut pfd = PollFd::new(
                unsafe { std::os::unix::io::BorrowedFd::borrow_raw(file.as_raw_fd()) },
                PollFlags::POLLPRI | PollFlags::POLLERR,
            );
            // 5 second fallback timeout
            let _ = poll(std::slice::from_mut(&mut pfd), 5000u16);

            if let Ok(devices) = parse_mountinfo() {
                let _ = tx.send(MountEvent::Updated(devices));
            }
        }
    });
}

/// FUSE pseudo-filesystems that are infrastructure, not user mounts.
const FUSE_BLOCKLIST: &[&str] = &["fuse.gvfsd-fuse", "fuse.portal"];

/// Filesystems we consider "real" for removable media under /run/media.
const MEDIA_FSTYPES: &[&str] = &[
    "ext4", "ext3", "ext2", "btrfs", "xfs", "f2fs",
    "vfat", "exfat", "ntfs", "ntfs3", "udf", "iso9660",
];

pub fn parse_mountinfo() -> anyhow::Result<Vec<DeviceEntry>> {
    let file = File::open("/proc/self/mountinfo")?;
    let reader = BufReader::new(file);
    let mut devices = Vec::new();

    // Always include root
    devices.push(DeviceEntry { name: "Root (/)".into(), path: "/".into() });

    for line in reader.lines() {
        let line = line?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        // mountinfo format: id parent_id major:minor root mountpoint mountopts [optfields] - fstype source superopts
        if fields.len() < 5 {
            continue;
        }
        let mountpoint = unescape_mountpoint(fields[4]);

        // Find separator "-" to get fstype
        let dash_pos = fields.iter().position(|&f| f == "-");
        let fstype = dash_pos.and_then(|p| fields.get(p + 1)).copied().unwrap_or("");

        if mountpoint == "/" {
            continue;
        }

        // Real user FUSE mounts (sshfs, rclone, ...) — but not gvfs/portal
        // infrastructure (nothing under /run/user), and not fusectl.
        let is_fuse = (fstype == "fuse" || fstype == "fuseblk" || fstype.starts_with("fuse."))
            && !FUSE_BLOCKLIST.contains(&fstype)
            && !mountpoint.starts_with("/run/user/");

        // Removable media mounted by udisks
        let is_media = mountpoint.starts_with("/run/media/")
            && MEDIA_FSTYPES.contains(&fstype);

        if !is_fuse && !is_media {
            continue;
        }

        let name = mountpoint
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&mountpoint)
            .to_string();

        devices.push(DeviceEntry { name, path: mountpoint });
    }

    Ok(devices)
}

/// mountinfo escapes space, tab, newline and backslash as octal (\040 etc).
fn unescape_mountpoint(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let (Some(&a), Some(&b), Some(&c)) = (bytes.get(i + 1), bytes.get(i + 2), bytes.get(i + 3))
                && a.is_ascii_digit() && b.is_ascii_digit() && c.is_ascii_digit() {
                    let val = (a - b'0') * 64 + (b - b'0') * 8 + (c - b'0');
                    out.push(val as char);
                    i += 4;
                    continue;
                }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
