//! Archives as folders, like Ark inside Dolphin: `photos.zip/2024/a.jpg`
//! lists and opens like any path. Archives are read with libarchive (zip,
//! tar.*, 7z, rar, iso, ...); their contents are read-only. A file opened
//! or copied out is unpacked first, into a per-run cache folder removed on
//! exit.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

mod ffi {
    use std::ffi::{c_char, c_int, c_uint, c_void};
    #[repr(C)]
    pub struct Archive {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct Entry {
        _p: [u8; 0],
    }
    pub const OK: c_int = 0;
    pub const EOF: c_int = 1;
    pub const WARN: c_int = -20;
    pub const IFMT: c_uint = 0o170000;
    pub const IFDIR: c_uint = 0o040000;
    pub const IFLNK: c_uint = 0o120000;
    pub const IFREG: c_uint = 0o100000;
    #[link(name = "archive")]
    unsafe extern "C" {
        pub fn archive_read_new() -> *mut Archive;
        pub fn archive_read_support_filter_all(a: *mut Archive) -> c_int;
        pub fn archive_read_support_format_all(a: *mut Archive) -> c_int;
        pub fn archive_read_open_filename(a: *mut Archive, name: *const c_char, block: usize) -> c_int;
        pub fn archive_read_next_header(a: *mut Archive, e: *mut *mut Entry) -> c_int;
        pub fn archive_read_data_skip(a: *mut Archive) -> c_int;
        pub fn archive_read_data(a: *mut Archive, buf: *mut c_void, len: usize) -> isize;
        pub fn archive_read_free(a: *mut Archive) -> c_int;
        pub fn archive_error_string(a: *mut Archive) -> *const c_char;
        pub fn archive_entry_pathname(e: *mut Entry) -> *const c_char;
        pub fn archive_entry_pathname_utf8(e: *mut Entry) -> *const c_char;
        pub fn archive_entry_symlink(e: *mut Entry) -> *const c_char;
        pub fn archive_entry_filetype(e: *mut Entry) -> c_uint;
        pub fn archive_entry_perm(e: *mut Entry) -> c_uint;
        pub fn archive_entry_size(e: *mut Entry) -> i64;
        pub fn archive_entry_size_is_set(e: *mut Entry) -> c_int;
        pub fn archive_entry_mtime(e: *mut Entry) -> i64;
        pub fn archive_entry_mtime_is_set(e: *mut Entry) -> c_int;
    }
}

/// An open archive being read entry by entry.
struct Reader(*mut ffi::Archive);

impl Reader {
    fn open(path: &Path) -> Result<Self, String> {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        unsafe {
            let a = ffi::archive_read_new();
            if a.is_null() {
                return Err("out of memory".into());
            }
            ffi::archive_read_support_filter_all(a);
            ffi::archive_read_support_format_all(a);
            let r = Reader(a);
            if ffi::archive_read_open_filename(a, name.as_ptr(), 64 * 1024) != ffi::OK {
                return Err(r.error());
            }
            Ok(r)
        }
    }

    fn error(&self) -> String {
        unsafe {
            let s = ffi::archive_error_string(self.0);
            if s.is_null() { "can't read the archive".into() } else { CStr::from_ptr(s).to_string_lossy().into_owned() }
        }
    }

    /// The next entry, or None at the end.
    fn next(&mut self) -> Result<Option<*mut ffi::Entry>, String> {
        let mut e = std::ptr::null_mut();
        match unsafe { ffi::archive_read_next_header(self.0, &mut e) } {
            ffi::EOF => Ok(None),
            ffi::OK | ffi::WARN => Ok(Some(e)),
            _ => Err(self.error()),
        }
    }

    fn skip(&mut self) {
        unsafe { ffi::archive_read_data_skip(self.0) };
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let n = unsafe { ffi::archive_read_data(self.0, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 { Err(self.error()) } else { Ok(n as usize) }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { ffi::archive_read_free(self.0) };
    }
}

fn entry_path(e: *mut ffi::Entry) -> Option<PathBuf> {
    let raw = unsafe {
        let utf8 = ffi::archive_entry_pathname_utf8(e);
        let p = if utf8.is_null() { ffi::archive_entry_pathname(e) } else { utf8 };
        if p.is_null() { return None; }
        CStr::from_ptr(p).to_bytes().to_vec()
    };
    clean(Path::new(OsStr::from_bytes(&raw)))
}

/// An archive member's path made relative and safe: no root, no `.`,
/// nothing climbing out with `..`.
fn clean(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[derive(Debug, Clone)]
pub struct Item {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// What an archive holds: every member by path, and each folder's children.
pub struct Index {
    items: HashMap<PathBuf, Item>,
    children: HashMap<PathBuf, Vec<PathBuf>>,
}

impl Index {
    fn read(archive: &Path) -> Result<Index, String> {
        let mut r = Reader::open(archive)?;
        let mut items: HashMap<PathBuf, Item> = HashMap::new();
        while let Some(e) = r.next()? {
            let Some(path) = entry_path(e) else { r.skip(); continue; };
            let kind = unsafe { ffi::archive_entry_filetype(e) } & ffi::IFMT;
            let size = (unsafe { ffi::archive_entry_size_is_set(e) } != 0)
                .then(|| unsafe { ffi::archive_entry_size(e) }.max(0) as u64);
            let modified = (unsafe { ffi::archive_entry_mtime_is_set(e) } != 0)
                .then(|| unsafe { ffi::archive_entry_mtime(e) })
                .filter(|&t| t > 0)
                .map(|t| SystemTime::UNIX_EPOCH + Duration::from_secs(t as u64));
            let is_dir = kind == ffi::IFDIR;
            items.insert(path, Item {
                is_dir,
                is_symlink: kind == ffi::IFLNK,
                size: if is_dir { None } else { size },
                modified,
            });
            r.skip();
        }
        // Folders only implied by their contents' paths
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for p in items.keys() {
            let mut at = p.parent();
            while let Some(d) = at.filter(|d| !d.as_os_str().is_empty()) {
                if !dirs.insert(d.to_path_buf()) {
                    break;
                }
                at = d.parent();
            }
        }
        for d in dirs {
            items.entry(d).or_insert(Item { is_dir: true, is_symlink: false, size: None, modified: None });
        }
        let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for p in items.keys() {
            let parent = p.parent().map(Path::to_path_buf).unwrap_or_default();
            children.entry(parent).or_default().push(p.clone());
        }
        Ok(Index { items, children })
    }

    pub fn is_dir(&self, inner: &Path) -> bool {
        inner.as_os_str().is_empty() || self.items.get(inner).is_some_and(|i| i.is_dir)
    }

    /// The members directly in `inner` ("" for the top).
    pub fn list(&self, inner: &Path) -> Vec<(&PathBuf, &Item)> {
        self.children.get(inner).into_iter().flatten()
            .filter_map(|p| self.items.get(p).map(|i| (p, i)))
            .collect()
    }
}

type Cached = (SystemTime, u64, Arc<Index>);

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The archive's index, read once and again only when the file changes.
pub fn index(archive: &Path) -> Result<Arc<Index>, String> {
    let meta = std::fs::metadata(archive).map_err(|e| e.to_string())?;
    let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
    if let Some((t, n, idx)) = cache().lock().unwrap().get(archive)
        && (*t, *n) == stamp
    {
        return Ok(idx.clone());
    }
    let idx = Arc::new(Index::read(archive)?);
    cache().lock().unwrap().insert(archive.to_path_buf(), (stamp.0, stamp.1, idx.clone()));
    Ok(idx)
}

/// A file we browse into: an archive by its name.
pub fn browsable(path: &Path) -> bool {
    crate::archive::is_archive(path)
}

/// Split a path inside an archive into the archive and the path within
/// it ("" for the archive itself). None for ordinary paths.
pub fn split(path: &Path) -> Option<(PathBuf, PathBuf)> {
    if let Ok(meta) = std::fs::metadata(path) {
        return (meta.is_file() && browsable(path)).then(|| (path.to_path_buf(), PathBuf::new()));
    }
    for anc in path.ancestors().skip(1) {
        match std::fs::metadata(anc) {
            Ok(m) if m.is_file() => {
                if !browsable(anc) {
                    return None;
                }
                let inner = path.strip_prefix(anc).ok()?;
                return Some((anc.to_path_buf(), clean(inner)?));
            }
            Ok(_) => return None,
            Err(_) => continue,
        }
    }
    None
}

/// Inside an archive (the archive itself counts as its top folder).
pub fn is_virtual(path: &Path) -> bool {
    split(path).is_some()
}

/// A folder to show: a real one, or a folder in an archive.
pub fn is_folder(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    match split(path) {
        Some((archive, inner)) => index(&archive).is_ok_and(|i| i.is_dir(&inner)),
        None => false,
    }
}

/// A folder in an archive listed like `fs::read_dir` lists a real one.
pub fn read_dir(path: &Path, show_hidden: bool) -> anyhow::Result<Vec<crate::fs::DirEntry>> {
    let (archive, inner) = split(path).ok_or_else(|| anyhow::anyhow!("not in an archive"))?;
    let idx = index(&archive).map_err(|e| anyhow::anyhow!("{}: {e}", archive.display()))?;
    if !idx.is_dir(&inner) {
        anyhow::bail!("{} is not a folder", path.display());
    }
    Ok(idx.list(&inner).into_iter()
        .filter_map(|(member, item)| {
            let name = member.file_name()?.to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            Some(crate::fs::DirEntry {
                path: path.join(&name),
                name,
                is_dir: item.is_dir,
                size: item.size,
                modified: item.modified,
                created: None,
                accessed: None,
                mime_type: None,
                is_symlink: item.is_symlink,
                depth: 0,
            })
        })
        .collect())
}

/// This run's folder for unpacked files.
fn unpack_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir)
        .join("file-browser-tui/archives")
        .join(std::process::id().to_string())
}

/// Remove this run's unpacked files, and any left by runs that crashed.
pub fn clean_up() {
    let _ = std::fs::remove_dir_all(unpack_root());
    let Some(base) = unpack_root().parent().map(Path::to_path_buf) else { return; };
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let alive = e.file_name().to_str().and_then(|p| p.parse::<u32>().ok())
            .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
        if !alive {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// A real copy of a path inside an archive (a file, or a folder with all
/// it holds), unpacked into this run's cache; ordinary paths as they are.
pub fn materialize(path: &Path) -> Result<PathBuf, String> {
    let Some((archive, inner)) = split(path) else { return Ok(path.to_path_buf()) };
    // One folder per archive (by path and size, so a changed one unpacks anew)
    let size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&archive, size).hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let root = unpack_root().join(key).join(name);
    let target = root.join(&inner);
    let idx = index(&archive)?;
    let whole_dir = idx.is_dir(&inner);
    if !whole_dir && target.is_file() {
        return Ok(target);
    }
    std::fs::create_dir_all(&target.parent().unwrap_or(&root)).map_err(|e| e.to_string())?;
    unpack(&archive, &inner, &root)?;
    if whole_dir {
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    }
    if target.exists() || target.is_symlink() { Ok(target) } else { Err(format!("{} isn't in the archive", inner.display())) }
}

/// Unpack the members at or under `inner` ("" for all) into `root`.
fn unpack(archive: &Path, inner: &Path, root: &Path) -> Result<(), String> {
    let mut r = Reader::open(archive)?;
    let mut buf = vec![0u8; 256 * 1024];
    while let Some(e) = r.next()? {
        let Some(path) = entry_path(e) else { r.skip(); continue; };
        if !path.starts_with(inner) {
            r.skip();
            continue;
        }
        let dest = root.join(&path);
        let kind = unsafe { ffi::archive_entry_filetype(e) } & ffi::IFMT;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        match kind {
            ffi::IFDIR => {
                std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                r.skip();
            }
            ffi::IFLNK => {
                let to = unsafe {
                    let p = ffi::archive_entry_symlink(e);
                    (!p.is_null()).then(|| CStr::from_ptr(p).to_bytes().to_vec())
                };
                if let Some(to) = to {
                    let _ = std::fs::remove_file(&dest);
                    let _ = std::os::unix::fs::symlink(OsStr::from_bytes(&to), &dest);
                }
                r.skip();
            }
            ffi::IFREG => {
                use std::io::Write;
                use std::os::unix::fs::PermissionsExt;
                let perm = unsafe { ffi::archive_entry_perm(e) } & 0o777;
                let mut f = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
                loop {
                    let n = r.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                }
                let mode = if perm == 0 { 0o644 } else { perm | 0o600 };
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode));
            }
            _ => r.skip(),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_and_unpack() {
        let d = std::env::temp_dir().join(format!("fbt-vfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/proj/sub")).unwrap();
        std::fs::write(d.join("src/proj/a b.txt"), "hello").unwrap();
        std::fs::write(d.join("src/proj/sub/deep.txt"), "deep").unwrap();
        std::fs::write(d.join("src/.hidden"), "h").unwrap();
        for (name, args) in [("t.tar.gz", vec!["-czf"]), ("t.zip", vec!["--format", "zip", "-cf"])] {
            let out = d.join(name);
            let mut c = std::process::Command::new("bsdtar");
            c.current_dir(d.join("src")).args(&args).arg(&out).arg("proj").arg(".hidden");
            assert!(c.status().unwrap().success());

            let (archive, inner) = split(&out.join("proj/sub")).unwrap();
            assert_eq!((archive.as_path(), inner.as_path()), (out.as_path(), Path::new("proj/sub")));
            assert_eq!(split(&out).unwrap().1, PathBuf::new());
            assert!(split(&d.join("src/proj")).is_none());
            assert!(is_folder(&out) && is_folder(&out.join("proj")) && !is_folder(&out.join("proj/a b.txt")));

            let mut top: Vec<String> = read_dir(&out, false).unwrap().into_iter().map(|e| e.name).collect();
            top.sort();
            assert_eq!(top, ["proj"], "{name}");
            assert_eq!(read_dir(&out, true).unwrap().len(), 2);
            let proj = read_dir(&out.join("proj"), false).unwrap();
            let file = proj.iter().find(|e| e.name == "a b.txt").unwrap();
            assert_eq!((file.size, file.is_dir), (Some(5), false));
            assert!(proj.iter().any(|e| e.name == "sub" && e.is_dir));

            let real = materialize(&out.join("proj/a b.txt")).unwrap();
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "hello");
            let dir = materialize(&out.join("proj/sub")).unwrap();
            assert_eq!(std::fs::read_to_string(dir.join("deep.txt")).unwrap(), "deep");
        }
        assert_eq!(clean(Path::new("../../etc/passwd")), None);
        assert_eq!(clean(Path::new("./a/./b")), Some(PathBuf::from("a/b")));
        clean_up();
        let _ = std::fs::remove_dir_all(&d);
    }
}
