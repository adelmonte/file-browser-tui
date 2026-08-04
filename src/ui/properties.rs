use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    sync::mpsc,
    time::SystemTime,
};

use chrono::{DateTime, Local};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

const PROPS_W: u16 = 58;
const PROPS_BG: Color = Color::Rgb(46, 52, 64); // nord0

pub struct PropertiesState {
    pub rows: Vec<(String, String)>,
    /// Background dir-size walk results: (items, bytes), final value wins.
    size_rx: Option<mpsc::Receiver<(u64, u64)>>,
}

impl PropertiesState {
    pub fn new(path: &Path) -> Self {
        let meta = std::fs::symlink_metadata(path).ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);

        let name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        let location = path.parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let type_str = if is_dir {
            "Folder (inode/directory)".to_string()
        } else if meta.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            "Symbolic link".to_string()
        } else {
            new_mime_guess::from_path(path)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .to_string()
        };

        let size_str = if is_dir {
            "Calculating\u{2026}".to_string()
        } else {
            meta.as_ref()
                .map(|m| format!("{} ({} bytes)", human_size(m.len()), m.len()))
                .unwrap_or_else(|| "\u{2014}".into())
        };

        let fmt_time = |t: Option<SystemTime>| t
            .map(|t| {
                let dt: DateTime<Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_else(|| "\u{2014}".into());

        let (perms, owner) = meta.as_ref().map(|m| {
            let mode = m.mode();
            let perms = format!("{} ({:03o})", mode_string(mode), mode & 0o7777);
            let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(m.uid()))
                .ok().flatten().map(|u| u.name)
                .unwrap_or_else(|| m.uid().to_string());
            let group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(m.gid()))
                .ok().flatten().map(|g| g.name)
                .unwrap_or_else(|| m.gid().to_string());
            (perms, format!("{user}:{group}"))
        }).unwrap_or_else(|| ("\u{2014}".into(), "\u{2014}".into()));

        let free = nix::sys::statvfs::statvfs(path)
            .map(|vfs| {
                let free = vfs.blocks_available() as u64 * vfs.fragment_size() as u64;
                let total = vfs.blocks() as u64 * vfs.fragment_size() as u64;
                format!("{} free of {}", human_size(free), human_size(total))
            })
            .unwrap_or_else(|_| "\u{2014}".into());

        let rows = vec![
            ("Name".into(), name),
            ("Type".into(), type_str),
            ("Location".into(), location),
            ("Size".into(), size_str),
            ("Modified".into(), fmt_time(meta.as_ref().and_then(|m| m.modified().ok()))),
            ("Created".into(), fmt_time(meta.as_ref().and_then(|m| m.created().ok()))),
            ("Accessed".into(), fmt_time(meta.as_ref().and_then(|m| m.accessed().ok()))),
            ("Permissions".into(), perms),
            ("Owner".into(), owner),
            ("Device".into(), free),
        ];

        let size_rx = if is_dir {
            let (tx, rx) = mpsc::channel();
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                let mut items: u64 = 0;
                let mut bytes: u64 = 0;
                walk(&path, &mut items, &mut bytes, &tx);
                let _ = tx.send((items, bytes));
            });
            Some(rx)
        } else {
            None
        };

        Self { rows, size_rx }
    }

    /// Drain async dir-size updates (called from App::drain_channels).
    pub fn poll(&mut self) {
        let Some(rx) = &self.size_rx else { return; };
        let mut latest = None;
        while let Ok(v) = rx.try_recv() {
            latest = Some(v);
        }
        if let Some((items, bytes)) = latest {
            if let Some(row) = self.rows.iter_mut().find(|(k, _)| k == "Size") {
                row.1 = format!("{} ({} items)", human_size(bytes), items);
            }
        }
    }
}

fn walk(dir: &Path, items: &mut u64, bytes: &mut u64, tx: &mpsc::Sender<(u64, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return; };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue; };
        *items += 1;
        if *items % 2048 == 0 {
            let _ = tx.send((*items, *bytes));
        }
        if meta.is_dir() {
            walk(&entry.path(), items, bytes, tx);
        } else {
            *bytes += meta.len();
        }
    }
}

fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

pub fn properties_rect(state: &PropertiesState, term: Rect) -> Rect {
    let width = PROPS_W.min(term.width);
    let height = (state.rows.len() as u16 + 2).min(term.height);
    let x = term.width.saturating_sub(width) / 2;
    let y = term.height.saturating_sub(height) / 2;
    Rect { x, y, width, height }
}

pub fn render_properties(f: &mut Frame, state: &PropertiesState) {
    let area = properties_rect(state, f.area());
    f.render_widget(Clear, area);

    let label_w = 12;
    let val_w = area.width.saturating_sub(label_w as u16 + 4) as usize;
    let lines: Vec<Line> = state.rows.iter().map(|(k, v)| {
        let mut val = v.clone();
        if val.chars().count() > val_w && val_w > 1 {
            val = val.chars().take(val_w - 1).collect::<String>() + "\u{2026}";
        }
        Line::from(vec![
            Span::styled(
                format!(" {k:<label_w$}"),
                Style::default().bg(PROPS_BG).add_modifier(Modifier::BOLD | Modifier::DIM),
            ),
            Span::styled(val, Style::default().bg(PROPS_BG)),
        ])
    }).collect();

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Properties (Esc to close) ")
                .border_style(Style::default().fg(Color::DarkGray).bg(PROPS_BG))
                .style(Style::default().bg(PROPS_BG)),
        ),
        area,
    );
}
