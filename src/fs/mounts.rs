use std::{
    collections::HashMap,
    os::unix::io::AsRawFd,
    sync::mpsc,
    thread,
    fs::File,
    io::{BufRead, BufReader},
};

use nix::poll::{PollFd, PollFlags, poll};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// The root filesystem: always there, never unmounted from here.
    Root,
    /// Removable media mounted by udisks under /run/media.
    Media,
    /// A user FUSE mount (sshfs, rclone, gocryptfs, ...).
    Fuse,
    /// Plugged in but not mounted: mounted on click (`path` is empty).
    Unmounted,
    /// An encrypted (LUKS) drive, still locked: asks for its passphrase.
    Locked,
    /// A phone on a USB cable, spoken to over MTP (`crate::phone`).
    Phone,
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub path: String,
    /// Fraction of the filesystem in use (0..=1).
    pub usage: Option<f32>,
    pub kind: MountKind,
    /// What's mounted (e.g. /dev/sdb1), from mountinfo.
    pub source: String,
}

impl DeviceEntry {
    pub fn new(name: String, path: String) -> Self {
        Self::with_kind(name, path, MountKind::Root, String::new())
    }

    /// A phone plugged in by USB, at its folder under /mtp.
    pub fn phone(p: &crate::phone::PhoneInfo) -> Self {
        Self { name: p.name.clone(), path: p.path().to_string_lossy().into_owned(), usage: p.usage, kind: MountKind::Phone, source: String::new() }
    }

    fn with_kind(name: String, path: String, kind: MountKind, source: String) -> Self {
        let usage = nix::sys::statvfs::statvfs(path.as_str()).ok().and_then(|st| {
            let total = st.blocks() as f64;
            (total > 0.0).then(|| (1.0 - st.blocks_available() as f64 / total) as f32)
        });
        Self { name, path, usage, kind, source }
    }

    pub fn can_unmount(&self) -> bool {
        matches!(self.kind, MountKind::Media | MountKind::Fuse)
    }

    pub fn is_mounted(&self) -> bool {
        !matches!(self.kind, MountKind::Unmounted | MountKind::Locked)
    }

    /// Theme icon for the sidebar.
    pub fn icon(&self) -> &'static str {
        match self.kind {
            MountKind::Root => "drive-harddisk-root",
            MountKind::Media => "drive-removable-media",
            MountKind::Fuse => "folder-remote",
            MountKind::Unmounted | MountKind::Locked => "drive-removable-media",
            MountKind::Phone => "smartphone",
        }
    }
}

/// Unmount `dev` (powering the drive off too with `power_off`, Dolphin's
/// Safely Remove). Blocks; returns the tool's complaint on failure.
pub fn unmount(dev: &DeviceEntry, power_off: bool) -> Result<(), String> {
    use std::process::{Command, Stdio};
    // Our own working directory inside it would keep it busy
    if std::env::current_dir().is_ok_and(|d| d.starts_with(&dev.path)) {
        let _ = std::env::set_current_dir(dirs::home_dir().unwrap_or_else(|| "/".into()));
    }
    let run = |cmd: &str, args: &[&str]| -> Result<(), String> {
        let out = Command::new(cmd).args(args).stdin(Stdio::null()).output()
            .map_err(|e| format!("{cmd}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{cmd} failed") } else { err })
    };
    match dev.kind {
        MountKind::Root | MountKind::Unmounted | MountKind::Locked | MountKind::Phone => Err("not mounted from here".into()),
        MountKind::Fuse => run("fusermount3", &["-u", &dev.path])
            .or_else(|_| run("fusermount", &["-u", &dev.path]))
            .or_else(|e| run("umount", &[&dev.path]).map_err(|_| e)),
        MountKind::Media if dev.source.starts_with("/dev/") => {
            run("udisksctl", &["unmount", "--no-user-interaction", "-b", &dev.source])
                .or_else(|e| run("umount", &[&dev.path]).map_err(|_| e))?;
            // An unlocked encrypted drive is locked again, as Dolphin does
            let backing = encrypted_backing(&dev.source);
            if let Some(b) = &backing {
                run("udisksctl", &["lock", "--no-user-interaction", "-b", b])?;
            }
            if power_off {
                let drive = backing.as_deref().unwrap_or(&dev.source);
                run("udisksctl", &["power-off", "--no-user-interaction", "-b", drive])?;
            }
            Ok(())
        }
        MountKind::Media => run("umount", &[&dev.path]),
    }
}

/// The device under a dm-crypt mapping (`/dev/mapper/luks-...` or
/// `/dev/dm-N`), e.g. `/dev/sdb1`; None if `dev` isn't one.
fn encrypted_backing(dev: &str) -> Option<String> {
    let real = std::fs::canonicalize(dev).ok()?;
    let name = real.file_name()?.to_string_lossy().to_string();
    if !name.starts_with("dm-") {
        return None;
    }
    let uuid = std::fs::read_to_string(format!("/sys/class/block/{name}/dm/uuid")).unwrap_or_default();
    if !uuid.starts_with("CRYPT-") {
        return None;
    }
    let slave = std::fs::read_dir(format!("/sys/class/block/{name}/slaves")).ok()?.flatten().next()?;
    Some(format!("/dev/{}", slave.file_name().to_string_lossy()))
}

/// Run a udisksctl command quietly; its first line of output on success,
/// its complaint on failure.
fn udisksctl(args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    use std::{io::Write, process::{Command, Stdio}};
    let mut child = Command::new("udisksctl").args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("udisksctl: {e} (install udisks2)"))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        let err = text(&out.stderr);
        // "Error unlocking /dev/sdb1: GDBus.Error:...: Failed to activate device: Operation not permitted"
        let short = err.rsplit(": ").next().unwrap_or(&err).to_string();
        Err(if short.is_empty() { "udisksctl failed".into() } else { short })
    }
}

/// Mount a plugged-in drive (`dev.source`); where it was mounted.
pub fn mount(dev: &str) -> Result<std::path::PathBuf, String> {
    // "Mounted /dev/sdb1 at /run/media/me/STICK" (older versions end with ".")
    let out = udisksctl(&["mount", "--no-user-interaction", "-b", dev], None)?;
    out.split_once(" at ").map(|(_, p)| std::path::PathBuf::from(p.trim_end_matches('.')))
        .ok_or_else(|| format!("mounted, but udisksctl said: {out}"))
}

/// Unlock an encrypted drive with `passphrase`, then mount what's inside.
pub fn unlock_and_mount(dev: &str, passphrase: &[u8]) -> Result<std::path::PathBuf, String> {
    // The passphrase goes through a pipe, never the command line
    let out = udisksctl(&["unlock", "--no-user-interaction", "-b", dev, "--key-file", "/dev/stdin"], Some(passphrase))?;
    // "Unlocked /dev/sdb1 as /dev/dm-3."
    let clear = out.split_once(" as ").map(|(_, d)| d.trim_end_matches('.').to_string())
        .ok_or_else(|| format!("unlocked, but udisksctl said: {out}"))?;
    // Nothing usable inside: lock it again rather than leave it open
    mount(&clear).inspect_err(|_| {
        let _ = udisksctl(&["lock", "--no-user-interaction", "-b", dev], None);
    })
}

/// Drives that are plugged in (removable or hot-pluggable, or attached disk
/// images) with a filesystem nobody has mounted yet, and locked encrypted
/// ones, from lsblk.
fn unmounted_drives() -> Vec<DeviceEntry> {
    let Ok(out) = std::process::Command::new("lsblk")
        .args(["-P", "-o", "NAME,PATH,LABEL,FSTYPE,MOUNTPOINT,RM,HOTPLUG,SIZE,TYPE,PKNAME"])
        .stderr(std::process::Stdio::null())
        .output() else { return Vec::new(); };
    let rows: Vec<HashMap<String, String>> = String::from_utf8_lossy(&out.stdout).lines().map(parse_pairs).collect();
    let by_name: HashMap<&str, &HashMap<String, String>> = rows.iter().map(|r| (r["NAME"].as_str(), r)).collect();
    let get = |r: &HashMap<String, String>, k: &str| r.get(k).cloned().unwrap_or_default();
    // Plugged in: the device or anything it sits on is removable/hotplug, or a loop device
    let pluggable = |r: &HashMap<String, String>| {
        let mut cur = Some(r);
        while let Some(c) = cur {
            if get(c, "RM") == "1" || get(c, "HOTPLUG") == "1" || get(c, "TYPE") == "loop" {
                return true;
            }
            cur = by_name.get(get(c, "PKNAME").as_str()).copied();
        }
        false
    };
    let mut drives = Vec::new();
    for r in &rows {
        let (fstype, kind) = (get(r, "FSTYPE"), get(r, "TYPE"));
        if fstype.is_empty() || matches!(fstype.as_str(), "swap" | "squashfs" | "LVM2_member")
            || !get(r, "MOUNTPOINT").is_empty()
            || !matches!(kind.as_str(), "part" | "disk" | "crypt" | "loop")
            || !pluggable(r)
        {
            continue;
        }
        let locked = fstype == "crypto_LUKS";
        // Unlocked already: its mapping stands in for it
        if locked && rows.iter().any(|c| get(c, "PKNAME") == get(r, "NAME") && get(c, "TYPE") == "crypt") {
            continue;
        }
        let label = get(r, "LABEL");
        let name = if !label.is_empty() {
            label
        } else if locked {
            format!("{} Encrypted Drive", get(r, "SIZE"))
        } else {
            format!("{} Drive", get(r, "SIZE"))
        };
        drives.push(DeviceEntry {
            name,
            path: String::new(),
            usage: None,
            kind: if locked { MountKind::Locked } else { MountKind::Unmounted },
            source: get(r, "PATH"),
        });
    }
    drives
}

/// One `lsblk -P` line: KEY="value" pairs (values escape `"` as \x22).
fn parse_pairs(line: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut rest = line.trim();
    while let Some((key, after)) = rest.split_once("=\"") {
        let Some(end) = after.find('"') else { break; };
        map.insert(key.trim().to_string(), after[..end].replace("\\x22", "\"").replace("\\x20", " "));
        rest = &after[end + 1..];
    }
    map
}

#[derive(Debug)]
pub enum MountEvent {
    Updated(Vec<DeviceEntry>),
}

/// Watch for mounts coming and going (mountinfo), and drives being plugged
/// in or pulled out (/dev), sending the device list after each change.
pub fn spawn_mount_watcher(tx: mpsc::Sender<MountEvent>) {
    use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
    use std::os::fd::AsFd;
    thread::spawn(move || {
        let file = match File::open("/proc/self/mountinfo") {
            Ok(f) => f,
            Err(e) => { tracing::error!("open mountinfo: {e}"); return; }
        };
        let dev_watch = Inotify::init(InitFlags::IN_NONBLOCK | InitFlags::IN_CLOEXEC).ok()
            .filter(|i| i.add_watch("/dev", AddWatchFlags::IN_CREATE | AddWatchFlags::IN_DELETE).is_ok());
        let mut last: Option<Vec<(String, MountKind, String)>> = None;
        if let Ok(devices) = parse_mountinfo() {
            let _ = tx.send(MountEvent::Updated(devices));
        }

        loop {
            let mut fds = vec![PollFd::new(
                unsafe { std::os::unix::io::BorrowedFd::borrow_raw(file.as_raw_fd()) },
                PollFlags::POLLPRI | PollFlags::POLLERR,
            )];
            if let Some(i) = &dev_watch {
                fds.push(PollFd::new(i.as_fd(), PollFlags::POLLIN));
            }
            // 5 second fallback timeout
            let _ = poll(&mut fds, 5000u16);
            let plugged = fds.get(1).and_then(|f| f.revents()).is_some_and(|r| r.contains(PollFlags::POLLIN));
            drop(fds);
            if plugged && let Some(i) = &dev_watch {
                while i.read_events().is_ok_and(|e| !e.is_empty()) {}
                // udev needs a moment to read a new drive's label and type
                thread::sleep(std::time::Duration::from_millis(600));
                while i.read_events().is_ok_and(|e| !e.is_empty()) {}
            }

            if let Ok(devices) = parse_mountinfo() {
                let key: Vec<_> = devices.iter().map(|d| (d.path.clone(), d.kind, d.source.clone())).collect();
                if last.as_ref() != Some(&key) || !plugged {
                    last = Some(key);
                    let _ = tx.send(MountEvent::Updated(devices));
                }
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
    devices.push(DeviceEntry::new("Root".into(), "/".into()));

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
        let source = dash_pos.and_then(|p| fields.get(p + 2)).map(|s| unescape_mountpoint(s)).unwrap_or_default();

        if mountpoint == "/" {
            continue;
        }

        // Real user FUSE mounts (sshfs, rclone, ...) — but not gvfs/portal
        // infrastructure (nothing under /run/user but sshfs, where KDE
        // Connect and lanlink mount phones), and not fusectl.
        let is_fuse = (fstype == "fuse" || fstype == "fuseblk" || fstype.starts_with("fuse."))
            && !FUSE_BLOCKLIST.contains(&fstype)
            && (fstype == "fuse.sshfs" || !mountpoint.starts_with("/run/user/"));

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

        let kind = if is_media { MountKind::Media } else { MountKind::Fuse };
        devices.push(DeviceEntry::with_kind(name, mountpoint, kind, source));
    }
    devices.extend(unmounted_drives());

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
