use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
};

#[cfg(target_os = "linux")]
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify, WatchDescriptor};

#[derive(Debug)]
pub enum DirEvent {
    Changed,
}

/// Single long-lived inotify watcher; `watch` swaps the watched directory.
#[cfg(target_os = "linux")]
pub struct DirWatcher {
    inotify: Arc<Inotify>,
    current: Arc<Mutex<Option<WatchDescriptor>>>,
}

#[cfg(target_os = "linux")]
impl DirWatcher {
    pub fn spawn(tx: mpsc::Sender<DirEvent>) -> Option<Self> {
        let inotify = match Inotify::init(InitFlags::IN_CLOEXEC) {
            Ok(i) => Arc::new(i),
            Err(e) => { tracing::error!("inotify init: {e}"); return None; }
        };
        let current: Arc<Mutex<Option<WatchDescriptor>>> = Arc::new(Mutex::new(None));

        let (ino, cur) = (inotify.clone(), current.clone());
        thread::spawn(move || loop {
            match ino.read_events() {
                Ok(events) => {
                    // Ignore events still queued for a previously watched dir
                    let wd = *cur.lock().unwrap();
                    if events.iter().any(|e| Some(e.wd) == wd) && tx.send(DirEvent::Changed).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::error!("inotify read_events: {e}");
                    return;
                }
            }
        });

        Some(Self { inotify, current })
    }

    pub fn watch(&self, path: &Path) {
        let flags = AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_DELETE
            | AddWatchFlags::IN_MOVED_FROM
            | AddWatchFlags::IN_MOVED_TO
            | AddWatchFlags::IN_CLOSE_WRITE;

        let mut cur = self.current.lock().unwrap();
        if let Some(old) = cur.take() {
            let _ = self.inotify.rm_watch(old);
        }
        match self.inotify.add_watch(path, flags) {
            Ok(wd) => *cur = Some(wd),
            Err(e) => tracing::warn!("inotify add_watch {:?}: {e}", path),
        }
    }
}

/// Single long-lived kqueue watcher (macOS/BSD); `watch` swaps the watched directory.
#[cfg(not(target_os = "linux"))]
pub struct DirWatcher {
    kq: Arc<nix::sys::event::Kqueue>,
    current: Arc<Mutex<Option<std::fs::File>>>,
}

#[cfg(not(target_os = "linux"))]
impl DirWatcher {
    pub fn spawn(tx: mpsc::Sender<DirEvent>) -> Option<Self> {
        let kq = match nix::sys::event::Kqueue::new() {
            Ok(k) => Arc::new(k),
            Err(e) => { tracing::error!("kqueue init: {e}"); return None; }
        };
        let current: Arc<Mutex<Option<std::fs::File>>> = Arc::new(Mutex::new(None));

        let (kq2, cur) = (kq.clone(), current.clone());
        thread::spawn(move || {
            use nix::sys::event::KEvent;
            use std::os::fd::AsRawFd;
            let empty = KEvent::new(0, nix::sys::event::EventFilter::EVFILT_VNODE,
                nix::sys::event::EvFlags::empty(), nix::sys::event::FilterFlag::empty(), 0, 0);
            let mut out = [empty; 16];
            loop {
                match kq2.kevent(&[], &mut out, None) {
                    Ok(n) => {
                        // Ignore events still queued for a previously watched dir
                        let fd = cur.lock().unwrap().as_ref().map(|f| f.as_raw_fd() as usize);
                        if out[..n].iter().any(|e| Some(e.ident()) == fd) && tx.send(DirEvent::Changed).is_err() {
                            return;
                        }
                    }
                    Err(nix::errno::Errno::EINTR) => {}
                    Err(e) => {
                        tracing::error!("kevent: {e}");
                        return;
                    }
                }
            }
        });

        Some(Self { kq, current })
    }

    pub fn watch(&self, path: &Path) {
        use nix::sys::event::{EventFilter, EvFlags, FilterFlag, KEvent};
        use std::os::fd::AsRawFd;

        let mut cur = self.current.lock().unwrap();
        // Dropping the old file closes its fd, which removes its kqueue event
        cur.take();
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => { tracing::warn!("kqueue open {:?}: {e}", path); return; }
        };
        let ev = KEvent::new(
            file.as_raw_fd() as usize,
            EventFilter::EVFILT_VNODE,
            EvFlags::EV_ADD | EvFlags::EV_CLEAR,
            FilterFlag::NOTE_WRITE | FilterFlag::NOTE_DELETE | FilterFlag::NOTE_RENAME
                | FilterFlag::NOTE_EXTEND | FilterFlag::NOTE_ATTRIB,
            0,
            0,
        );
        match self.kq.kevent(&[ev], &mut [], Some(nix::libc::timespec { tv_sec: 0, tv_nsec: 0 })) {
            Ok(_) => *cur = Some(file),
            Err(e) => tracing::warn!("kqueue add {:?}: {e}", path),
        }
    }
}
