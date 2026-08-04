use std::{path::PathBuf, sync::mpsc, thread};

use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};

#[derive(Debug)]
pub enum DirEvent {
    Changed,
}

pub fn spawn_dir_watcher(path: PathBuf, tx: mpsc::Sender<DirEvent>) {
    thread::spawn(move || {
        let inotify = match Inotify::init(InitFlags::empty()) {
            Ok(i) => i,
            Err(e) => { tracing::error!("inotify init: {e}"); return; }
        };

        let flags = AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_DELETE
            | AddWatchFlags::IN_MOVED_FROM
            | AddWatchFlags::IN_MOVED_TO
            | AddWatchFlags::IN_CLOSE_WRITE;

        if inotify.add_watch(&path, flags).is_err() {
            tracing::warn!("inotify add_watch failed for {:?}", path);
            return;
        }

        loop {
            match inotify.read_events() {
                Ok(events) if !events.is_empty() => {
                    let _ = tx.send(DirEvent::Changed);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("inotify read_events: {e}");
                    return;
                }
            }
        }
    });
}
