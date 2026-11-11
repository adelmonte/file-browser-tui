//! Android phones (and cameras) on a USB cable, browsed like folders: the
//! app speaks MTP itself (mtp-rs), nothing is mounted. A phone set to File
//! transfer shows under Devices; its storage is at /mtp/<phone>/<storage>/.
//! One thread owns the USB sessions and does what the rest ask, one thing
//! at a time. A phone is opened when it's used and let go after a few idle
//! seconds: only one program can have it open, so another window (or the
//! file dialog) can open it in between.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use mtp_rs::mtp::{ByteRange, HotplugEvent, MtpDevice, MtpDeviceInfo, NewObjectInfo, ObjectHandle, ObjectInfo, Storage};

/// Where phones appear in the folder tree. Not on disk.
pub const ROOT: &str = "/mtp";

/// How long a phone stays open with nothing asked of it.
const IDLE: Duration = Duration::from_secs(8);

/// A phone that's plugged in, for the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct PhoneInfo {
    pub name: String,
    /// Fraction of its storage in use, once it has been opened.
    pub usage: Option<f32>,
}

impl PhoneInfo {
    pub fn path(&self) -> PathBuf {
        Path::new(ROOT).join(&self.name)
    }
}

/// Something in a folder on a phone.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// A running download or upload: bytes done so far, and a flag to stop it.
#[derive(Debug, Clone, Default)]
pub struct Transfer {
    pub bytes: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
}

enum Op {
    List,
    Stat,
    Download(PathBuf, Transfer),
    Upload(PathBuf, Transfer),
    Mkdir,
    Delete,
    Rename(String),
    /// Copy (true) or move into a folder on the same phone.
    Transfer(PathBuf, bool),
    Thumbnail,
}

enum Answer {
    Items(Vec<Item>),
    Item(Option<Item>),
    Bytes(Vec<u8>),
    Done,
}

type Reply = mpsc::Sender<Result<Answer, String>>;

enum Msg {
    Ask(PathBuf, Op, Reply),
    Plug(HotplugEvent),
}

static TX: OnceLock<mpsc::Sender<Msg>> = OnceLock::new();
/// Set while a download or upload runs: the phone does one thing at a time,
/// so a folder listing would wait for the whole transfer.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Start watching for phones; `on_change` gets the list whenever it changes.
pub fn start(on_change: mpsc::Sender<Vec<PhoneInfo>>) {
    let (tx, rx) = mpsc::channel();
    if TX.set(tx.clone()).is_err() {
        return;
    }
    std::thread::spawn(move || Worker { phones: Vec::new(), on_change }.run(rx));
    std::thread::spawn(move || {
        futures::executor::block_on(async {
            let mut watch = match mtp_rs::mtp::watch_devices() {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("phones: can't watch USB: {e}");
                    return;
                }
            };
            while let Some(event) = watch.next().await {
                if tx.send(Msg::Plug(event)).is_err() {
                    return;
                }
            }
        });
    });
}

/// A path on a phone (or the phones' top, /mtp).
pub fn is_phone(path: &Path) -> bool {
    path.starts_with(ROOT)
}

pub fn busy() -> bool {
    BUSY.load(Ordering::Relaxed)
}

fn ask(path: &Path, op: Op) -> Result<Answer, String> {
    start_asking(path, op)?.recv().map_err(|_| "the phone connection stopped".to_string())?
}

fn start_asking(path: &Path, op: Op) -> Result<mpsc::Receiver<Result<Answer, String>>, String> {
    let tx = TX.get().ok_or("phones aren't being watched")?;
    let (reply, rx) = mpsc::channel();
    tx.send(Msg::Ask(path.to_path_buf(), op, reply)).map_err(|_| "the phone connection stopped".to_string())?;
    Ok(rx)
}

pub fn read_dir(path: &Path) -> Result<Vec<Item>, String> {
    if busy() {
        return Err("The phone is busy copying; its folders show again when that's done".into());
    }
    match ask(path, Op::List)? {
        Answer::Items(items) => Ok(items),
        _ => Err("unexpected answer".into()),
    }
}

/// What's at `path`, or None if nothing is.
pub fn stat(path: &Path) -> Result<Option<Item>, String> {
    match ask(path, Op::Stat)? {
        Answer::Item(item) => Ok(item),
        _ => Err("unexpected answer".into()),
    }
}

/// A file's thumbnail as the phone made it (a JPEG, usually).
pub fn thumbnail(path: &Path) -> Result<Vec<u8>, String> {
    match ask(path, Op::Thumbnail)? {
        Answer::Bytes(b) => Ok(b),
        _ => Err("unexpected answer".into()),
    }
}

pub fn mkdir(path: &Path) -> Result<(), String> {
    ask(path, Op::Mkdir).map(|_| ())
}

/// Delete a file, or a folder with everything in it.
pub fn delete(path: &Path) -> Result<(), String> {
    ask(path, Op::Delete).map(|_| ())
}

pub fn rename(path: &Path, new_name: &str) -> Result<(), String> {
    ask(path, Op::Rename(new_name.to_string())).map(|_| ())
}

/// Copy or move `path` into `dest_dir` on the same phone, without the
/// bytes coming over the cable.
pub fn transfer(path: &Path, dest_dir: &Path, copy: bool) -> Result<(), String> {
    ask(path, Op::Transfer(dest_dir.to_path_buf(), copy)).map(|_| ())
}

/// Start downloading the file at `path` to `dest` (on disk); the answer
/// comes on the returned channel, progress in `t`.
pub fn download(path: &Path, dest: &Path, t: &Transfer) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    finished(start_asking(path, Op::Download(dest.to_path_buf(), t.clone()))?)
}

/// Start uploading the file `src` (on disk) to `path` on the phone.
pub fn upload(src: &Path, path: &Path, t: &Transfer) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    finished(start_asking(path, Op::Upload(src.to_path_buf(), t.clone()))?)
}

/// Upload a file and wait for it.
pub fn upload_now(src: &Path, path: &Path) -> Result<(), String> {
    upload(src, path, &Transfer::default())?.recv().map_err(|_| "the phone connection stopped".to_string())?
}

fn finished(rx: mpsc::Receiver<Result<Answer, String>>) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    let (tx, out) = mpsc::channel();
    std::thread::spawn(move || {
        let r = rx.recv().map_err(|_| "the phone connection stopped".to_string()).and_then(|r| r.map(|_| ()));
        let _ = tx.send(r);
    });
    Ok(out)
}

/// `path`'s parts after /mtp: the phone's name, the storage's, and the rest.
fn parts(path: &Path) -> Option<Vec<String>> {
    let rest = path.strip_prefix(ROOT).ok()?;
    rest.components()
        .map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

struct Phone {
    info: MtpDeviceInfo,
    name: String,
    usage: Option<f32>,
    session: Option<Session>,
}

struct Session {
    device: MtpDevice,
    storages: Vec<Storage>,
    /// Folder listings, by storage and folder (ROOT for the storage's top).
    listings: HashMap<(usize, ObjectHandle), Vec<ObjectInfo>>,
}

/// What a path names.
enum Target {
    Top,
    Phone(usize),
    Storage(usize, usize),
    Object(usize, usize, ObjectInfo),
}

struct Worker {
    phones: Vec<Phone>,
    on_change: mpsc::Sender<Vec<PhoneInfo>>,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        loop {
            let open = self.phones.iter().any(|p| p.session.is_some());
            let msg = match rx.recv_timeout(if open { IDLE } else { Duration::from_secs(3600) }) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    for p in &mut self.phones {
                        if let Some(s) = p.session.take() {
                            let _ = futures::executor::block_on(s.device.close());
                        }
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            match msg {
                Msg::Plug(HotplugEvent::Arrived(info)) => {
                    if !self.phones.iter().any(|p| p.info.location_id == info.location_id) {
                        let base = info.product.clone().or_else(|| info.manufacturer.clone()).unwrap_or_else(|| "Phone".into());
                        let base = base.replace('/', "-");
                        let name = (1..)
                            .map(|n| if n == 1 { base.clone() } else { format!("{base} ({n})") })
                            .find(|n| !self.phones.iter().any(|p| &p.name == n))
                            .unwrap_or(base);
                        tracing::info!("phone plugged in: {name}");
                        self.phones.push(Phone { info, name, usage: None, session: None });
                        self.publish();
                    }
                }
                Msg::Plug(HotplugEvent::Left(info)) => {
                    self.phones.retain(|p| p.info.location_id != info.location_id);
                    self.publish();
                }
                Msg::Ask(path, mut op, reply) => {
                    let transfer = matches!(op, Op::Download(..) | Op::Upload(..));
                    BUSY.store(transfer, Ordering::Relaxed);
                    let mut r = futures::executor::block_on(self.answer(&path, &mut op));
                    // A listing out of date (changed on the phone since): once more, fresh
                    if r.is_err() && matches!(op, Op::List | Op::Stat | Op::Download(..) | Op::Thumbnail) {
                        for p in &mut self.phones {
                            if let Some(s) = &mut p.session {
                                s.listings.clear();
                            }
                        }
                        r = futures::executor::block_on(self.answer(&path, &mut op));
                    }
                    BUSY.store(false, Ordering::Relaxed);
                    let _ = reply.send(r);
                }
            }
        }
    }

    fn publish(&self) {
        let list: Vec<PhoneInfo> = self.phones.iter().map(|p| PhoneInfo { name: p.name.clone(), usage: p.usage }).collect();
        let _ = self.on_change.send(list);
    }

    /// Phone `i`, opened if it isn't.
    async fn session(&mut self, i: usize) -> Result<&mut Session, String> {
        if self.phones[i].session.is_none() {
            let device = MtpDevice::open_by_location(self.phones[i].info.location_id).await.map_err(|e| describe(&e))?;
            let storages = device.storages().await.map_err(|e| describe(&e))?;
            let (total, free) = storages.iter().fold((0u64, 0u64), |(t, f), s| (t + s.info().total_capacity, f + s.info().free_space));
            let usage = (total > 0).then(|| 1.0 - free as f32 / total as f32);
            let changed = self.phones[i].usage != usage;
            self.phones[i].usage = usage;
            self.phones[i].session = Some(Session { device, storages, listings: HashMap::new() });
            if changed {
                self.publish();
            }
        }
        let session = self.phones[i].session.as_mut().unwrap();
        // Locked, it shows no storage; unlocked since, it does
        if session.storages.is_empty() {
            session.storages = session.device.storages().await.map_err(|e| describe(&e))?;
        }
        if session.storages.is_empty() {
            return Err("Unlock the phone to see its files".into());
        }
        Ok(session)
    }

    /// A storage's folder name: its description, numbered if two match.
    fn storage_names(session: &Session) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for s in &session.storages {
            let base = s.info().description.replace('/', "-");
            let base = if base.is_empty() { "Storage".to_string() } else { base };
            let name = (1..)
                .map(|n| if n == 1 { base.clone() } else { format!("{base} ({n})") })
                .find(|n| !names.contains(n))
                .unwrap_or(base);
            names.push(name);
        }
        names
    }

    async fn listing(&mut self, p: usize, s: usize, folder: ObjectHandle, fresh: bool) -> Result<Vec<ObjectInfo>, String> {
        let session = self.session(p).await?;
        if !fresh && let Some(l) = session.listings.get(&(s, folder)) {
            return Ok(l.clone());
        }
        let parent = (folder != ObjectHandle::ROOT).then_some(folder);
        let list = session.storages[s].list_objects(parent).await.map_err(|e| describe(&e))?;
        session.listings.insert((s, folder), list.clone());
        Ok(list)
    }

    /// What `path` names; None if nothing there.
    async fn resolve(&mut self, path: &Path) -> Result<Option<Target>, String> {
        let parts = parts(path).ok_or("not a phone path")?;
        let Some(name) = parts.first() else { return Ok(Some(Target::Top)) };
        let Some(p) = self.phones.iter().position(|ph| &ph.name == name) else { return Ok(None) };
        let Some(storage) = parts.get(1) else { return Ok(Some(Target::Phone(p))) };
        let names = Self::storage_names(self.session(p).await?);
        let Some(s) = names.iter().position(|n| n == storage) else { return Ok(None) };
        let mut folder = ObjectHandle::ROOT;
        let mut found = None;
        for (i, part) in parts[2..].iter().enumerate() {
            let list = self.listing(p, s, folder, false).await?;
            let Some(obj) = list.into_iter().find(|o| &o.filename == part) else { return Ok(None) };
            if i + 3 < parts.len() {
                if !obj.is_folder() {
                    return Ok(None);
                }
                folder = obj.handle;
            }
            found = Some(obj);
        }
        Ok(Some(match found {
            Some(obj) => Target::Object(p, s, obj),
            None => Target::Storage(p, s),
        }))
    }

    /// The folder `path` names, as (phone, storage, handle).
    async fn folder(&mut self, path: &Path) -> Result<(usize, usize, ObjectHandle), String> {
        match self.resolve(path).await? {
            Some(Target::Storage(p, s)) => Ok((p, s, ObjectHandle::ROOT)),
            Some(Target::Object(p, s, o)) if o.is_folder() => Ok((p, s, o.handle)),
            Some(_) => Err(format!("{} isn't a folder you can put files in", name_of(path))),
            None => Err(format!("{} isn't on the phone", name_of(path))),
        }
    }

    fn forget(&mut self, p: usize, s: usize, folder: ObjectHandle) {
        if let Some(session) = &mut self.phones[p].session {
            session.listings.remove(&(s, folder));
        }
    }

    async fn answer(&mut self, path: &Path, op: &mut Op) -> Result<Answer, String> {
        match op {
            Op::List => {
                let items = match self.resolve(path).await? {
                    Some(Target::Top) => self.phones.iter().map(|p| folder_item(&p.name)).collect(),
                    Some(Target::Phone(p)) => Self::storage_names(self.session(p).await?).iter().map(|n| folder_item(n)).collect(),
                    Some(Target::Storage(p, s)) => self.listing(p, s, ObjectHandle::ROOT, true).await?.iter().map(item).collect(),
                    Some(Target::Object(p, s, o)) if o.is_folder() => self.listing(p, s, o.handle, true).await?.iter().map(item).collect(),
                    Some(Target::Object(..)) => return Err(format!("{} is not a folder", name_of(path))),
                    None => return Err(format!("{} isn't on the phone", name_of(path))),
                };
                Ok(Answer::Items(items))
            }
            Op::Stat => Ok(Answer::Item(match self.resolve(path).await? {
                Some(Target::Object(_, _, o)) => Some(item(&o)),
                Some(_) => Some(folder_item(&name_of(path))),
                None => None,
            })),
            Op::Thumbnail => {
                let Some(Target::Object(p, s, o)) = self.resolve(path).await? else { return Err("no such file".into()) };
                let session = self.session(p).await?;
                session.storages[s].thumbnail(o.handle).await.map(Answer::Bytes).map_err(|e| describe(&e))
            }
            Op::Download(dest, t) => {
                let Some(Target::Object(p, s, o)) = self.resolve(path).await? else { return Err(format!("{} isn't on the phone", name_of(path))) };
                if o.is_folder() {
                    return Err("can't download a folder in one go".into());
                }
                let part = dest.with_file_name(format!(".{}.part", name_of(dest)));
                let result = async {
                    use std::io::Write;
                    let session = self.session(p).await?;
                    let mut dl = session.storages[s].download(o.handle, ByteRange::Full).await.map_err(|e| describe(&e))?;
                    let mut file = std::io::BufWriter::new(std::fs::File::create(&part).map_err(|e| e.to_string())?);
                    let mut done = 0u64;
                    while let Some(chunk) = dl.next_chunk().await {
                        let chunk = chunk.map_err(|e| describe(&e))?;
                        file.write_all(&chunk).map_err(|e| e.to_string())?;
                        done += chunk.len() as u64;
                        t.bytes.store(done, Ordering::Relaxed);
                        if t.stop.load(Ordering::Relaxed) {
                            let _ = dl.cancel(Duration::from_secs(2)).await;
                            return Err("cancelled".to_string());
                        }
                    }
                    file.flush().map_err(|e| e.to_string())?;
                    drop(file);
                    if let Some(m) = o.modified.and_then(|m| m.to_unix_seconds_with_fallback(local_offset())).filter(|&s| s > 0)
                        && let Ok(f) = std::fs::File::options().write(true).open(&part)
                    {
                        let _ = f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(m as u64));
                    }
                    std::fs::rename(&part, &*dest).map_err(|e| e.to_string())
                }.await;
                if result.is_err() {
                    let _ = std::fs::remove_file(&part);
                }
                result.map(|_| Answer::Done)
            }
            Op::Upload(src, t) => {
                let name = name_of(path);
                let parent = path.parent().ok_or("no folder")?.to_path_buf();
                let (p, s, folder) = self.folder(&parent).await?;
                let meta = std::fs::metadata(&*src).map_err(|e| e.to_string())?;
                let mut info = NewObjectInfo::file(name, meta.len());
                if let Some(m) = meta.modified().ok().and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok()) {
                    if let Some(d) = mtp_rs::mtp::DateTime::from_unix_seconds(m.as_secs() as i64, local_offset()) {
                        info = info.with_modified(d);
                    }
                }
                let file = std::fs::File::open(&*src).map_err(|e| e.to_string())?;
                let stream = Box::pin(futures::stream::unfold(file, |mut f| async move {
                    use std::io::Read;
                    let mut buf = vec![0u8; 256 * 1024];
                    match f.read(&mut buf) {
                        Ok(0) => None,
                        Ok(n) => {
                            buf.truncate(n);
                            Some((Ok(bytes::Bytes::from(buf)), f))
                        }
                        Err(e) => Some((Err(e), f)),
                    }
                }));
                let t = t.clone();
                let session = self.session(p).await?;
                let parent_handle = (folder != ObjectHandle::ROOT).then_some(folder);
                let result = session.storages[s].upload_with_progress(parent_handle, info, stream, move |pr| {
                    t.bytes.store(pr.bytes_transferred, Ordering::Relaxed);
                    if t.stop.load(Ordering::Relaxed) { std::ops::ControlFlow::Break(()) } else { std::ops::ControlFlow::Continue(()) }
                }).await;
                if let Err(e) = &result
                    && let Some(partial) = e.partial
                {
                    let _ = session.storages[s].delete(partial).await;
                }
                self.forget(p, s, folder);
                result.map(|_| Answer::Done).map_err(|e| describe(&e.source))
            }
            Op::Mkdir => {
                let parent = path.parent().ok_or("no folder")?.to_path_buf();
                let (p, s, folder) = self.folder(&parent).await?;
                let session = self.session(p).await?;
                let parent_handle = (folder != ObjectHandle::ROOT).then_some(folder);
                let r = session.storages[s].create_folder(parent_handle, &name_of(path)).await.map_err(|e| describe(&e));
                self.forget(p, s, folder);
                r.map(|_| Answer::Done)
            }
            Op::Delete => {
                let Some(Target::Object(p, s, o)) = self.resolve(path).await? else { return Err(format!("{} isn't on the phone", name_of(path))) };
                self.delete_tree(p, s, &o).await?;
                self.forget(p, s, o.parent);
                self.forget(p, s, ObjectHandle::ROOT);
                Ok(Answer::Done)
            }
            Op::Rename(new_name) => {
                let Some(Target::Object(p, s, o)) = self.resolve(path).await? else { return Err(format!("{} isn't on the phone", name_of(path))) };
                let session = self.session(p).await?;
                let r = session.storages[s].rename(o.handle, new_name).await.map_err(|e| describe(&e));
                self.forget(p, s, o.parent);
                self.forget(p, s, ObjectHandle::ROOT);
                r.map(|_| Answer::Done)
            }
            Op::Transfer(dest_dir, copy) => {
                let Some(Target::Object(p, s, o)) = self.resolve(path).await? else { return Err(format!("{} isn't on the phone", name_of(path))) };
                let (dp, ds, folder) = self.folder(dest_dir).await?;
                if dp != p {
                    return Err("can't copy straight from one phone to another".into());
                }
                let session = self.session(p).await?;
                let storage_id = session.storages[ds].id();
                let r = if *copy {
                    session.storages[s].copy_object(o.handle, folder, Some(storage_id)).await.map(|_| ())
                } else {
                    session.storages[s].move_object(o.handle, folder, Some(storage_id)).await
                };
                self.forget(p, s, o.parent);
                self.forget(p, s, ObjectHandle::ROOT);
                self.forget(dp, ds, folder);
                r.map(|_| Answer::Done).map_err(|e| describe(&e))
            }
        }
    }

    /// Delete an object; a folder the phone won't delete whole is emptied first.
    async fn delete_tree(&mut self, p: usize, s: usize, o: &ObjectInfo) -> Result<(), String> {
        let first = self.session(p).await?.storages[s].delete(o.handle).await;
        if first.is_ok() || !o.is_folder() {
            return first.map_err(|e| describe(&e));
        }
        for child in self.listing(p, s, o.handle, true).await? {
            Box::pin(self.delete_tree(p, s, &child)).await?;
        }
        self.forget(p, s, o.handle);
        self.session(p).await?.storages[s].delete(o.handle).await.map_err(|e| describe(&e))
    }
}

/// Phones mostly give times as wall-clock time with no zone: ours.
fn local_offset() -> mtp_rs::mtp::UtcOffset {
    use chrono::Offset;
    let minutes = chrono::Local::now().offset().fix().local_minus_utc() / 60;
    mtp_rs::mtp::UtcOffset::from_minutes(minutes as i16).unwrap_or(mtp_rs::mtp::UtcOffset::UTC)
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn folder_item(name: &str) -> Item {
    Item { name: name.to_string(), is_dir: true, size: None, modified: None }
}

fn item(o: &ObjectInfo) -> Item {
    let is_dir = o.is_folder();
    Item {
        name: o.filename.clone(),
        is_dir,
        size: (!is_dir).then_some(o.size),
        modified: o.modified.and_then(|m| m.to_unix_seconds_with_fallback(local_offset())).filter(|&s| s > 0)
            .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s as u64)),
    }
}

/// An MTP error as something to tell the user.
fn describe(e: &mtp_rs::Error) -> String {
    use mtp_rs::Error as E;
    match e {
        E::ExclusiveAccess | E::Busy => "The phone is in use by another program; close it, or unplug the phone and plug it in again".into(),
        E::AccessDenied | E::PermissionDenied => "The phone said no: unlock it, and allow file access if it asks".into(),
        E::Disconnected | E::NoDevice => "The phone was disconnected".into(),
        E::StorageFull => "The phone is full".into(),
        E::AlreadyExists => "Something with that name is already there".into(),
        E::Cancelled => "cancelled".into(),
        e => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(is_phone(Path::new("/mtp/Pixel 9/Internal shared storage/DCIM")));
        assert!(!is_phone(Path::new("/mtpx/a")) && !is_phone(Path::new("/home/mtp")));
        assert_eq!(parts(Path::new("/mtp/Pixel 9/Internal/DCIM")).unwrap(), ["Pixel 9", "Internal", "DCIM"]);
        assert_eq!(parts(Path::new("/mtp")).unwrap(), Vec::<String>::new());
    }
}
