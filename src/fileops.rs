//! File operations (copy, move, trash, delete) on a background worker, with
//! progress for the status bar and a report of what failed. Done here
//! rather than by `gio`, so they work without GLib, never overwrite (name
//! clashes get " (2)"-style names) and can show progress and errors.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
    Trash,
    Delete,
    EmptyTrash,
}

impl Op {
    fn verb(self) -> (&'static str, &'static str) {
        match self {
            Op::Copy => ("Copying", "Copied"),
            Op::Move => ("Moving", "Moved"),
            Op::Trash => ("Moving to trash", "Moved to trash"),
            Op::Delete => ("Deleting", "Deleted"),
            Op::EmptyTrash => ("Emptying trash", "Emptied trash"),
        }
    }
}

struct Job {
    op: Op,
    sources: Vec<PathBuf>,
    dest: Option<PathBuf>,
}

/// A running job's progress, as last reported.
#[derive(Debug, Clone)]
pub struct Progress {
    pub op: Op,
    /// Still counting what there is to do.
    pub scanning: bool,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub total_items: usize,
    pub done_items: usize,
    /// Name of the file being worked on.
    pub current: String,
    pub started: Instant,
}

impl Progress {
    pub fn verb(&self) -> &'static str {
        self.op.verb().0
    }

    pub fn fraction(&self) -> f64 {
        if self.total_bytes > 0 {
            self.done_bytes as f64 / self.total_bytes as f64
        } else if self.total_items > 0 {
            self.done_items as f64 / self.total_items as f64
        } else {
            0.0
        }
    }

    /// Estimated time left, once there's enough to go on.
    pub fn remaining(&self) -> Option<Duration> {
        let f = self.fraction();
        let el = self.started.elapsed().as_secs_f64();
        (f > 0.02 && el > 1.0).then(|| Duration::from_secs_f64(el / f - el))
    }
}

/// How a finished job went, for the status bar.
#[derive(Debug, Clone)]
pub struct Report {
    pub op: Op,
    pub done: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

impl Report {
    pub fn message(&self) -> String {
        let past = self.op.verb().1;
        let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        if self.cancelled {
            return format!("Cancelled ({} done)", items(self.done));
        }
        match self.errors.as_slice() {
            [] if self.op == Op::EmptyTrash => past.to_string(),
            [] => format!("{past} {}", items(self.done)),
            [one] => format!("{past} {}; failed: {one}", items(self.done)),
            [first, rest @ ..] => format!("{past} {}; {} failed: {first} (+{} more)", items(self.done), rest.len() + 1, rest.len()),
        }
    }
}

enum Msg {
    Progress(Progress),
    Done(Report),
}

pub struct FileOps {
    jobs: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// Queued or running jobs.
    pending: usize,
    pub progress: Option<Progress>,
}

impl FileOps {
    pub fn new() -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                c.store(false, Ordering::Relaxed);
                let report = Worker::new(job.op, &tx, &c).run(&job);
                let _ = tx.send(Msg::Done(report));
            }
        });
        Self { jobs, rx, cancel, pending: 0, progress: None }
    }

    fn submit(&mut self, op: Op, sources: Vec<PathBuf>, dest: Option<PathBuf>) {
        if sources.is_empty() && op != Op::EmptyTrash {
            return;
        }
        self.pending += 1;
        let _ = self.jobs.send(Job { op, sources, dest });
    }

    pub fn copy(&mut self, sources: Vec<PathBuf>, dest: &Path) {
        self.submit(Op::Copy, sources, Some(dest.to_path_buf()));
    }

    pub fn move_to(&mut self, sources: Vec<PathBuf>, dest: &Path) {
        self.submit(Op::Move, sources, Some(dest.to_path_buf()));
    }

    pub fn trash(&mut self, paths: Vec<PathBuf>) {
        self.submit(Op::Trash, paths, None);
    }

    pub fn delete(&mut self, paths: Vec<PathBuf>) {
        self.submit(Op::Delete, paths, None);
    }

    pub fn empty_trash(&mut self) {
        self.submit(Op::EmptyTrash, Vec::new(), None);
    }

    /// Stop the running job after the current chunk.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn busy(&self) -> bool {
        self.pending > 0
    }

    /// Take the worker's news: progress updates, and finished jobs' reports.
    pub fn poll(&mut self) -> Vec<Report> {
        let mut done = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(p) => self.progress = Some(p),
                Msg::Done(r) => {
                    self.pending = self.pending.saturating_sub(1);
                    self.progress = None;
                    done.push(r);
                }
            }
        }
        done
    }
}

struct Worker<'a> {
    tx: &'a mpsc::Sender<Msg>,
    cancel: &'a AtomicBool,
    p: Progress,
    last_sent: Instant,
    report: Report,
}

const CHUNK: usize = 1 << 20;

impl<'a> Worker<'a> {
    fn new(op: Op, tx: &'a mpsc::Sender<Msg>, cancel: &'a AtomicBool) -> Self {
        let now = Instant::now();
        Self {
            tx,
            cancel,
            p: Progress { op, scanning: true, total_bytes: 0, done_bytes: 0, total_items: 0, done_items: 0, current: String::new(), started: now },
            last_sent: now - Duration::from_secs(1),
            report: Report { op, done: 0, errors: Vec::new(), cancelled: false },
        }
    }

    fn cancelled(&mut self) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            self.report.cancelled = true;
        }
        self.report.cancelled
    }

    /// Send progress at most every 50 ms (or when forced).
    fn tick(&mut self, force: bool) {
        if force || self.last_sent.elapsed() >= Duration::from_millis(50) {
            self.last_sent = Instant::now();
            let _ = self.tx.send(Msg::Progress(self.p.clone()));
        }
    }

    fn fail(&mut self, path: &Path, e: impl std::fmt::Display) {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        tracing::warn!("fileops: {}: {e}", path.display());
        self.report.errors.push(format!("{name}: {e}"));
    }

    fn run(mut self, job: &Job) -> Report {
        let op = job.op;
        // Byte totals only matter where bytes get copied
        if matches!(op, Op::Copy | Op::Move) {
            for s in &job.sources {
                let (bytes, items) = measure(s);
                self.p.total_bytes += bytes;
                self.p.total_items += items;
                self.tick(false);
            }
        } else {
            self.p.total_items = job.sources.len();
        }
        self.p.scanning = false;
        self.tick(true);

        match op {
            Op::EmptyTrash => {
                for dir in ["files", "info"] {
                    let root = home_trash().join(dir);
                    for e in fs::read_dir(&root).into_iter().flatten().flatten() {
                        if self.cancelled() {
                            break;
                        }
                        if let Err(err) = remove_any(&e.path()) {
                            self.fail(&e.path(), err);
                        }
                    }
                }
            }
            _ => {
                for src in &job.sources {
                    if self.cancelled() {
                        break;
                    }
                    self.p.current = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.tick(false);
                    let result = match op {
                        Op::Copy => self.copy_top(src, job.dest.as_deref().unwrap_or(Path::new("."))),
                        Op::Move => self.move_top(src, job.dest.as_deref().unwrap_or(Path::new("."))),
                        Op::Trash => trash(src).map(|_| ()),
                        Op::Delete => remove_any(src),
                        Op::EmptyTrash => Ok(()),
                    };
                    match result {
                        Ok(()) => self.report.done += 1,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted && self.report.cancelled => {}
                        Err(e) => self.fail(src, e),
                    }
                    if !matches!(op, Op::Copy | Op::Move) {
                        self.p.done_items += 1;
                    }
                }
            }
        }
        self.tick(true);
        self.report
    }

    fn copy_top(&mut self, src: &Path, dest_dir: &Path) -> io::Result<()> {
        let name = src.file_name().ok_or_else(|| io::Error::other("no file name"))?;
        if dest_dir.starts_with(src) {
            return Err(io::Error::other("can't copy a folder into itself"));
        }
        let target = free_name(&dest_dir.join(name));
        self.copy_tree(src, &target)
    }

    fn move_top(&mut self, src: &Path, dest_dir: &Path) -> io::Result<()> {
        let name = src.file_name().ok_or_else(|| io::Error::other("no file name"))?;
        if src.parent() == Some(dest_dir) {
            // Already there
            let (bytes, items) = measure(src);
            self.p.done_bytes += bytes;
            self.p.done_items += items;
            return Ok(());
        }
        if dest_dir.starts_with(src) {
            return Err(io::Error::other("can't move a folder into itself"));
        }
        let target = free_name(&dest_dir.join(name));
        match fs::rename(src, &target) {
            Ok(()) => {
                let (bytes, items) = measure(&target);
                self.p.done_bytes += bytes;
                self.p.done_items += items;
                Ok(())
            }
            // Another filesystem: copy, then remove the original
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let errors = self.report.errors.len();
                self.copy_tree(src, &target)?;
                // Anything inside that didn't copy keeps the original
                if self.report.errors.len() > errors {
                    return Err(io::Error::other("not everything copied; original kept"));
                }
                remove_any(src)
            }
            Err(e) => Err(e),
        }
    }

    fn copy_tree(&mut self, src: &Path, dst: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(src)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
        } else if ft.is_dir() {
            fs::create_dir(dst)?;
            for e in fs::read_dir(src)? {
                if self.cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let e = e?;
                if let Err(err) = self.copy_tree(&e.path(), &dst.join(e.file_name())) {
                    if err.kind() == io::ErrorKind::Interrupted {
                        return Err(err);
                    }
                    self.fail(&e.path(), err);
                }
            }
            let _ = fs::set_permissions(dst, fs::Permissions::from_mode(meta.mode()));
        } else {
            self.p.current = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Err(e) = self.copy_file(src, dst, &meta) {
                let _ = fs::remove_file(dst);
                return Err(e);
            }
        }
        self.p.done_items += 1;
        self.tick(false);
        Ok(())
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> io::Result<()> {
        let mut from = fs::File::open(src)?;
        let mut to = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if self.cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n])?;
            self.p.done_bytes += n as u64;
            self.tick(false);
        }
        let _ = to.set_permissions(fs::Permissions::from_mode(meta.mode()));
        if let Ok(m) = meta.modified() {
            let _ = to.set_modified(m);
        }
        Ok(())
    }
}

/// Bytes and items under `path` (not following symlinks).
fn measure(path: &Path) -> (u64, usize) {
    let Ok(meta) = fs::symlink_metadata(path) else { return (0, 0) };
    if !meta.is_dir() {
        return (if meta.is_file() { meta.len() } else { 0 }, 1);
    }
    let (mut bytes, mut items) = (0, 1);
    for e in fs::read_dir(path).into_iter().flatten().flatten() {
        let (b, i) = measure(&e.path());
        bytes += b;
        items += i;
    }
    (bytes, items)
}

fn remove_any(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// `path`, or the first free "name (2).ext", "name (3).ext", ...
pub fn free_name(path: &Path) -> PathBuf {
    if fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Extension stays at the end ("photo (2).jpg"); folders and dotfiles keep it whole
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !path.is_dir() => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Trash (freedesktop.org Trash spec)
// ---------------------------------------------------------------------------

pub fn home_trash() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("Trash")
}

/// Move `path` to the trash on its own filesystem: the home trash, or a
/// `.Trash-$uid` at the top of other mounts. Returns where it went.
pub fn trash(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let meta = fs::symlink_metadata(&path)?;
    let home = home_trash();
    let home_dev = fs::create_dir_all(&home).and_then(|_| fs::metadata(&home)).map(|m| m.dev());
    let (trash_dir, info_path) = if home_dev.is_ok_and(|d| d == meta.dev()) {
        (home, path.clone())
    } else {
        let top = mount_top(&path, meta.dev());
        let uid = nix::unistd::getuid().as_raw();
        let dir = top.join(format!(".Trash-{uid}"));
        // Path is relative to the mount's top in a topdir trash
        let rel = path.strip_prefix(&top).map(Path::to_path_buf).unwrap_or_else(|_| path.clone());
        (dir, rel)
    };
    trash_into(&trash_dir, &path, &info_path)
}

fn trash_into(trash_dir: &Path, path: &Path, info_path: &Path) -> io::Result<PathBuf> {
    let (files, info) = (trash_dir.join("files"), trash_dir.join("info"));
    for d in [trash_dir, &files, &info] {
        if !d.exists() {
            fs::create_dir_all(d)?;
            let _ = fs::set_permissions(d, fs::Permissions::from_mode(0o700));
        }
    }
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?.to_string_lossy().into_owned();
    // Reserve a name by creating its .trashinfo first (atomic, per spec)
    let encoded = crate::thumbs::file_uri(info_path);
    let encoded = encoded.strip_prefix("file://").unwrap_or(&encoded);
    let encoded = if info_path.is_relative() { encoded.trim_start_matches('/') } else { encoded };
    let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    for n in 1.. {
        let candidate = if n == 1 { name.clone() } else { format!("{name}.{n}") };
        let info_file = info.join(format!("{candidate}.trashinfo"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&info_file) {
            Ok(mut f) => {
                f.write_all(format!("[Trash Info]\nPath={encoded}\nDeletionDate={date}\n").as_bytes())?;
                let target = files.join(&candidate);
                if let Err(e) = fs::rename(path, &target) {
                    let _ = fs::remove_file(&info_file);
                    return Err(e);
                }
                return Ok(target);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// The top directory of the mount holding `path` (device `dev`).
fn mount_top(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.to_path_buf();
    while let Some(parent) = top.parent() {
        if fs::metadata(parent).map(|m| m.dev()).ok() != Some(dev) {
            break;
        }
        top = parent.to_path_buf();
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(ops: &mut FileOps) -> Report {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(r) = ops.poll().pop() {
                return r;
            }
            assert!(Instant::now() < deadline, "job didn't finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn copy_move_conflicts() {
        let root = std::env::temp_dir().join(format!("fbt-fileops-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (src, dst) = (root.join("src"), root.join("dst"));
        fs::create_dir_all(src.join("tree/inner")).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("tree/inner/a.txt"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        fs::write(src.join("note.txt"), "hi").unwrap();
        std::os::unix::fs::symlink("inner/a.txt", src.join("tree/link")).unwrap();
        fs::write(dst.join("note.txt"), "old").unwrap();

        let mut ops = FileOps::new();
        ops.copy(vec![src.join("tree"), src.join("note.txt")], &dst);
        let r = wait(&mut ops);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.done, 2);
        assert_eq!(fs::read(dst.join("tree/inner/a.txt")).unwrap().len(), 3 * CHUNK + 5);
        assert_eq!(fs::read_link(dst.join("tree/link")).unwrap(), PathBuf::from("inner/a.txt"));
        assert_eq!(fs::read_to_string(dst.join("note.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(dst.join("note (2).txt")).unwrap(), "hi");

        // Copy into itself is refused; a second copy of the folder gets a new name
        ops.copy(vec![src.clone()], &src.join("tree"));
        assert_eq!(wait(&mut ops).errors.len(), 1);
        ops.copy(vec![src.join("tree")], &dst);
        wait(&mut ops);
        assert!(dst.join("tree (2)/inner/a.txt").exists());

        ops.move_to(vec![src.join("note.txt")], &dst);
        assert!(wait(&mut ops).errors.is_empty());
        assert!(!src.join("note.txt").exists() && dst.join("note (3).txt").exists());

        ops.delete(vec![dst.join("tree (2)")]);
        wait(&mut ops);
        assert!(!dst.join("tree (2)").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_cleans_up() {
        let root = std::env::temp_dir().join(format!("fbt-cancel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dst")).unwrap();
        fs::write(root.join("big.bin"), vec![1u8; 4 * CHUNK]).unwrap();
        let (tx, _rx) = mpsc::channel();
        let cancel = AtomicBool::new(true);
        let job = Job { op: Op::Copy, sources: vec![root.join("big.bin")], dest: Some(root.join("dst")) };
        let r = Worker::new(Op::Copy, &tx, &cancel).run(&job);
        assert!(r.cancelled && r.done == 0 && r.errors.is_empty(), "{r:?}");
        assert!(!root.join("dst/big.bin").exists());
        assert_eq!(r.message(), "Cancelled (0 items done)");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trash_spec() {
        let root = std::env::temp_dir().join(format!("fbt-trash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let trash = root.join("Trash");
        for _ in 0..2 {
            let f = root.join("my file.txt");
            fs::write(&f, "x").unwrap();
            trash_into(&trash, &f, &f).unwrap();
        }
        assert!(trash.join("files/my file.txt").exists() && trash.join("files/my file.txt.2").exists());
        let info = fs::read_to_string(trash.join("info/my file.txt.2.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/") && info.contains("my%20file.txt\nDeletionDate=20"), "{info}");
        let _ = fs::remove_dir_all(&root);
    }
}
