//! file-browser-tui-mount: mounts, unlocks and safely removes plugged-in
//! drives for file-browser-tui, so udisks2 isn't needed. Installed setuid
//! root; it only ever touches removable or hot-plugged drives, and mounts
//! them under /run/media/<user> with nosuid,nodev.
//!
//!   file-browser-tui-mount mount <device>        prints where it went
//!   file-browser-tui-mount unmount <mount point>
//!   file-browser-tui-mount unlock <device>       passphrase on stdin; prints the unlocked device
//!   file-browser-tui-mount lock <device>
//!   file-browser-tui-mount power-off <device>

use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use nix::{
    mount::{MntFlags, MsFlags},
    unistd::{Gid, Uid, User},
};

type Res<T> = Result<T, String>;

const USAGE: &str = "usage: file-browser-tui-mount mount|unmount|unlock|lock|power-off <device or mount point>";
const MEDIA: &str = "/run/media";

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Res<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [cmd, target] = args.as_slice() else { return Err(USAGE.into()); };
    if !nix::unistd::geteuid().is_root() {
        return Err("file-browser-tui-mount isn't installed setuid root".into());
    }
    let user = User::from_uid(nix::unistd::getuid()).ok().flatten().ok_or("unknown user")?;
    // Wholly root from here, so the tools it runs aren't half-privileged
    let root = (Uid::from_raw(0), Gid::from_raw(0));
    nix::unistd::setgroups(&[]).and_then(|_| nix::unistd::setresgid(root.1, root.1, root.1))
        .and_then(|_| nix::unistd::setresuid(root.0, root.0, root.0))
        .map_err(|e| format!("can't become root: {e}"))?;
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o022));
    match cmd.as_str() {
        "mount" => mount(&user, target),
        "unmount" => unmount(&user, target).map(|_| String::new()),
        "unlock" => unlock(target),
        "lock" => lock(target).map(|_| String::new()),
        "power-off" => power_off(target).map(|_| String::new()),
        _ => Err(USAGE.into()),
    }
}

/// A block device, found by its device number so no path tricks lead
/// elsewhere.
struct Dev {
    /// Its kernel name, e.g. sdb1 or dm-3.
    name: String,
    /// Its sysfs folder, symlinks resolved.
    sys: PathBuf,
}

impl Dev {
    fn open(arg: &str) -> Res<Dev> {
        let meta = fs::metadata(arg).map_err(|e| format!("{arg}: {e}"))?;
        if !meta.file_type().is_block_device() {
            return Err(format!("{arg} isn't a drive"));
        }
        let (major, minor) = (nix::sys::stat::major(meta.rdev()), nix::sys::stat::minor(meta.rdev()));
        Dev::from_number(&format!("{major}:{minor}")).ok_or_else(|| format!("{arg}: not in sysfs"))
    }

    fn from_number(number: &str) -> Option<Dev> {
        let sys = fs::canonicalize(format!("/sys/dev/block/{number}")).ok()?;
        let name = sys.file_name()?.to_str()?.to_string();
        Some(Dev { name, sys })
    }

    fn from_name(name: &str) -> Option<Dev> {
        let number = attr(&Path::new("/sys/class/block").join(name).join("dev"))?;
        Dev::from_number(&number)
    }

    fn path(&self) -> String {
        format!("/dev/{}", self.name)
    }

    fn number(&self) -> String {
        attr(&self.sys.join("dev")).unwrap_or_default()
    }

    /// The whole drive a partition is on (itself if it isn't one).
    fn disk(&self) -> Dev {
        match self.sys.join("partition").exists().then(|| self.sys.parent()).flatten() {
            Some(parent) => Dev {
                name: parent.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                sys: parent.to_path_buf(),
            },
            None => Dev { name: self.name.clone(), sys: self.sys.clone() },
        }
    }

    /// An unlocked encrypted drive's mapping (dm-crypt).
    fn is_crypt(&self) -> bool {
        attr(&self.sys.join("dm/uuid")).is_some_and(|u| u.starts_with("CRYPT-"))
    }

    fn holders(&self) -> Vec<Dev> {
        list(&self.sys.join("holders")).iter().filter_map(|n| Dev::from_name(n)).collect()
    }

    fn slaves(&self) -> Vec<Dev> {
        list(&self.sys.join("slaves")).iter().filter_map(|n| Dev::from_name(n)).collect()
    }

    /// Plugged in: on a removable drive (USB, FireWire, an SD card,
    /// removable media), or an unlocked mapping of one.
    fn pluggable(&self) -> bool {
        if self.name.starts_with("dm-") {
            let slaves = self.slaves();
            return self.is_crypt() && slaves.len() == 1 && slaves[0].pluggable();
        }
        if self.name.starts_with("loop") || self.sys.starts_with("/sys/devices/virtual") {
            return false;
        }
        let disk = self.disk();
        attr(&disk.sys.join("removable")).as_deref() == Some("1")
            || disk.sys.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with("usb") || s.starts_with("fw")))
            || (disk.name.starts_with("mmcblk") && attr(&disk.sys.join("device/type")).as_deref() == Some("SD"))
    }

    fn is_mounted(&self) -> bool {
        let number = self.number();
        mountinfo().iter().any(|(n, _)| *n == number)
    }

    /// Fit for us to touch: plugged in, not mounted, not in use by a mapping.
    fn usable(&self) -> Res<()> {
        if !self.pluggable() {
            return Err(format!("{} isn't a removable drive", self.path()));
        }
        if self.is_mounted() {
            return Err(format!("{} is already mounted", self.path()));
        }
        if !self.holders().is_empty() {
            return Err(format!("{} is in use", self.path()));
        }
        Ok(())
    }
}

fn attr(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn list(dir: &Path) -> Vec<String> {
    fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// Mounts: device number and mount point.
fn mountinfo() -> Vec<(String, String)> {
    fs::read_to_string("/proc/self/mountinfo").unwrap_or_default().lines().filter_map(|l| {
        let f: Vec<&str> = l.split(' ').collect();
        Some((f.get(2)?.to_string(), unescape(f.get(4)?)))
    }).collect()
}

/// mountinfo writes space, tab, newline and backslash as octal (\040).
fn unescape(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run a system tool with a clean environment; its output, or its
/// complaint.
fn tool(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Res<String> {
    let mut child = Command::new(program).args(args)
        .env_clear().env("PATH", "/usr/bin:/usr/sbin:/bin:/sbin").env("LC_ALL", "C")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("{program}: {e}"))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("").to_string();
    Err(match out.status.code() {
        // cryptsetup: no key slot opened with it
        Some(2) if program.ends_with("cryptsetup") => "wrong passphrase".into(),
        _ if err.is_empty() => format!("{program} failed"),
        _ => err,
    })
}

/// The first of `names` installed in the system folders.
fn system_tool(names: &[&str]) -> Option<String> {
    names.iter().flat_map(|n| ["/usr/bin", "/usr/sbin", "/bin", "/sbin"].map(|d| format!("{d}/{n}")))
        .find(|p| Path::new(p).is_file())
}

/// What's on a drive (TYPE, LABEL, UUID, ...), from blkid.
fn probe(dev: &Dev) -> Res<HashMap<String, String>> {
    let blkid = system_tool(&["blkid"]).ok_or("blkid (util-linux) isn't installed")?;
    let out = tool(&blkid, &["-p", "-o", "export", &dev.path()], None).map_err(|_| format!("nothing on {} to mount", dev.path()))?;
    Ok(parse_export(&out))
}

/// `blkid -o export` lines: KEY=value, with special characters
/// backslash-escaped.
fn parse_export(out: &str) -> HashMap<String, String> {
    out.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| {
        let mut value = String::new();
        let mut chars = v.chars();
        while let Some(c) = chars.next() {
            value.push(if c == '\\' { chars.next().unwrap_or('\\') } else { c });
        }
        (k.to_string(), value)
    }).collect()
}

/// The kernel filesystem to mount a blkid TYPE as, its options for the
/// user, and whether it's read-only by nature.
fn filesystem(kind: &str, user: &User) -> Option<(&'static str, String, bool)> {
    let ids = format!("uid={},gid={}", user.uid, user.gid);
    Some(match kind {
        "vfat" => ("vfat", format!("{ids},shortname=mixed,utf8=1,flush,errors=remount-ro"), false),
        "exfat" => ("exfat", format!("{ids},errors=remount-ro"), false),
        "ntfs" => ("ntfs3", format!("{ids},windows_names"), false),
        "udf" => ("udf", ids, false),
        "iso9660" => ("iso9660", ids, true),
        "hfsplus" => ("hfsplus", ids, false),
        "ext2" => ("ext2", String::new(), false),
        "ext3" => ("ext3", String::new(), false),
        "ext4" => ("ext4", String::new(), false),
        "btrfs" => ("btrfs", String::new(), false),
        "xfs" => ("xfs", String::new(), false),
        "f2fs" => ("f2fs", String::new(), false),
        _ => return None,
    })
}

/// The user's folder for mounts, /run/media/<user>: root's, so nothing in
/// it can be swapped for a symlink, but readable by the user only.
fn media_dir(user: &User, create: bool) -> Res<PathBuf> {
    if user.name.is_empty() || user.name.contains('/') || user.name.starts_with('.') {
        return Err("unusable user name".into());
    }
    let dir = Path::new(MEDIA).join(&user.name);
    for (path, mode, gid) in [(Path::new(MEDIA), 0o755, 0), (dir.as_path(), 0o750, user.gid.as_raw())] {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() && m.uid() == 0 => {}
            Ok(_) => return Err(format!("{} isn't a folder of root's", path.display())),
            Err(_) if create => {
                fs::DirBuilder::new().mode(mode).create(path).map_err(|e| format!("{}: {e}", path.display()))?;
                nix::unistd::chown(path, Some(Uid::from_raw(0)), Some(Gid::from_raw(gid))).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(dir)
}

/// A folder name from a drive's label: no slashes, not . or .., not empty.
fn folder_name(label: &str) -> String {
    let name: String = label.chars().map(|c| if c == '/' || c == '\0' || c.is_control() { '_' } else { c }).collect();
    let name = name.trim();
    let mut name = if name.is_empty() || name == "." || name == ".." { "disk".to_string() } else { name.to_string() };
    while name.len() > 200 {
        name.pop();
    }
    name
}

fn mount(user: &User, target: &str) -> Res<String> {
    let dev = Dev::open(target)?;
    dev.usable()?;
    let info = probe(&dev)?;
    let kind = info.get("TYPE").map(String::as_str).unwrap_or("");
    if kind == "crypto_LUKS" {
        return Err("encrypted: unlock it first".into());
    }
    let (fstype, options, read_only) = filesystem(kind, user)
        .ok_or_else(|| if kind.is_empty() { "no filesystem on it".to_string() } else { format!("can't mount {kind} drives") })?;
    let dir = media_dir(user, true)?;
    let label = info.get("LABEL").or(info.get("UUID")).map(String::as_str).unwrap_or("disk");
    let base = folder_name(label);
    let mounted: Vec<String> = mountinfo().into_iter().map(|(_, p)| p).collect();
    // A free folder: new, or an empty one left behind
    let point = (0..100).map(|i| dir.join(if i == 0 { base.clone() } else { format!("{base}{i}") })).find(|p| {
        if mounted.iter().any(|m| Path::new(m) == p) {
            return false;
        }
        match fs::symlink_metadata(p) {
            Err(_) => fs::DirBuilder::new().mode(0o700).create(p).is_ok(),
            Ok(m) => m.is_dir() && fs::read_dir(p).is_ok_and(|mut d| d.next().is_none()),
        }
    }).ok_or("no free folder to mount it in")?;
    let mut flags = MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_RELATIME;
    if read_only || attr(&dev.sys.join("ro")).as_deref() == Some("1") {
        flags |= MsFlags::MS_RDONLY;
    }
    let attempt = |flags: MsFlags| nix::mount::mount(Some(dev.path().as_str()), &point, Some(fstype), flags, Some(options.as_str()));
    let result = attempt(flags).or_else(|e| match e {
        // A write-protected drive
        nix::errno::Errno::EROFS | nix::errno::Errno::EACCES if !flags.contains(MsFlags::MS_RDONLY) => attempt(flags | MsFlags::MS_RDONLY),
        e => Err(e),
    });
    if let Err(e) = result {
        let _ = fs::remove_dir(&point);
        return Err(match e {
            nix::errno::Errno::ENODEV => format!("this system can't read {kind} drives"),
            nix::errno::Errno::EINVAL => format!("{kind} filesystem is damaged or unreadable"),
            e => format!("mount: {}", e.desc()),
        });
    }
    Ok(point.to_string_lossy().into_owned())
}

fn unmount(user: &User, target: &str) -> Res<()> {
    let dir = media_dir(user, false)?;
    let target = Path::new(target);
    let name = target.file_name().filter(|_| target.parent() == Some(dir.as_path()))
        .ok_or("only drives mounted in /run/media can be unmounted")?;
    let point = dir.join(name);
    if !mountinfo().iter().any(|(_, p)| Path::new(p) == point) {
        return Err(format!("{} isn't mounted", point.display()));
    }
    nix::mount::umount2(&point, MntFlags::UMOUNT_NOFOLLOW).map_err(|e| match e {
        nix::errno::Errno::EBUSY => "it's busy: a program still has files open on it".to_string(),
        e => format!("unmount: {}", e.desc()),
    })?;
    let _ = fs::remove_dir(&point);
    Ok(())
}

fn cryptsetup() -> Res<String> {
    system_tool(&["cryptsetup"]).ok_or_else(|| "cryptsetup isn't installed".into())
}

fn unlock(target: &str) -> Res<String> {
    let dev = Dev::open(target)?;
    dev.usable()?;
    let info = probe(&dev)?;
    if info.get("TYPE").map(String::as_str) != Some("crypto_LUKS") {
        return Err(format!("{} isn't encrypted", dev.path()));
    }
    let uuid = info.get("UUID").filter(|u| !u.is_empty() && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .ok_or("the drive has no usable UUID")?;
    let cryptsetup = cryptsetup()?;
    let mut passphrase = Vec::new();
    std::io::stdin().take(8192).read_to_end(&mut passphrase).map_err(|e| e.to_string())?;
    let name = format!("luks-{uuid}");
    let result = tool(&cryptsetup, &["open", "--type", "luks", "--key-file", "-", &dev.path(), &name], Some(&passphrase));
    passphrase.fill(0);
    result?;
    Ok(format!("/dev/mapper/{name}"))
}

/// The unlocked mapping for `target`: the mapping itself, or the one over
/// an encrypted drive.
fn mapping(target: &str) -> Res<Dev> {
    let dev = Dev::open(target)?;
    if dev.is_crypt() {
        return Ok(dev);
    }
    dev.holders().into_iter().find(Dev::is_crypt).ok_or_else(|| format!("{} isn't unlocked", dev.path()))
}

fn lock(target: &str) -> Res<()> {
    let map = mapping(target)?;
    if !map.pluggable() {
        return Err(format!("{} isn't a removable drive", map.path()));
    }
    if map.is_mounted() || !map.holders().is_empty() {
        return Err("it's still mounted".into());
    }
    let name = attr(&map.sys.join("dm/name")).filter(|n| !n.is_empty() && !n.contains('/')).ok_or("unnamed mapping")?;
    match cryptsetup() {
        Ok(cs) => tool(&cs, &["close", &name], None),
        Err(e) => tool(&system_tool(&["dmsetup"]).ok_or(e)?, &["remove", &name], None),
    }.map(drop)
}

fn power_off(target: &str) -> Res<()> {
    let dev = Dev::open(target)?;
    if dev.name.starts_with("dm-") {
        return Err("power off the drive, not its unlocked mapping".into());
    }
    let disk = dev.disk();
    if !disk.pluggable() {
        return Err(format!("{} isn't a removable drive", disk.path()));
    }
    let parts: Vec<Dev> = list(&disk.sys).iter().filter(|n| disk.sys.join(n).join("partition").exists())
        .filter_map(|n| Dev::from_name(n)).collect();
    if std::iter::once(&disk).chain(&parts).any(|d| d.is_mounted() || !d.holders().is_empty()) {
        return Err("something on the drive is still mounted".into());
    }
    if let Ok(f) = fs::File::open(disk.path()) {
        let _ = f.sync_all();
    }
    // The USB device it's on: switching off its port powers it down
    let usb = disk.sys.ancestors().find(|a| a.join("idVendor").exists() && a.join("remove").exists());
    let remove = match usb {
        Some(u) => u.join("remove"),
        None => disk.sys.join("device/delete"),
    };
    fs::write(&remove, "1").map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "this drive can't be powered off; it's safe to pull out".to_string(),
        _ => format!("power off: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_stay_in_their_folder() {
        assert_eq!(folder_name("STICK"), "STICK");
        assert_eq!(folder_name("a/b"), "a_b");
        assert_eq!(folder_name(".."), "disk");
        assert_eq!(folder_name("  "), "disk");
        assert_eq!(folder_name("../x"), ".._x");
    }

    #[test]
    fn blkid_export() {
        let m = parse_export("DEVNAME=/dev/sdb1\nLABEL=My\\ Stick\nTYPE=vfat\n");
        assert_eq!(m["LABEL"], "My Stick");
        assert_eq!(m["TYPE"], "vfat");
    }

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape("/run/media/me/My\\040Stick"), "/run/media/me/My Stick");
        assert_eq!(unescape("/a\\"), "/a\\");
    }
}
