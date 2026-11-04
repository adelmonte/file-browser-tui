//! Kitty images drawn *under* the text layer (z < 0): pixel-exact rounded
//! backgrounds for text fields. A cell background can only be whole rows;
//! an underlay can be any height, so a field can extend a few pixels above
//! and below its text row like Dolphin's, with the text still on a normal
//! row (the cells keep the default background so the image shows through).

use std::fmt::Write;

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::App;

/// Ids for underlay images, far from ratatui-image's random ids in practice.
const ID_BASE: u32 = 0x00F1_E1D0;

/// Where an underlay is currently placed, to skip redundant re-placement.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    rect: Rect,
    pad: u32,
    fill: (u8, u8, u8),
    border: Option<(u8, u8, u8)>,
}

/// How far (in pixels) a field reaches above and below its text row.
pub fn field_pad(app: &App) -> u32 {
    (app.picker.font_size().height as f32 * 0.32).round() as u32
}

/// Show a rounded field background behind `rect` (one text row), reaching
/// `pad` pixels into the rows above and below. `slot` identifies the field.
/// Returns false when it can't (not Kitty): fill the cells instead.
pub fn field(app: &mut App, slot: u32, rect: Rect, color: (u8, u8, u8)) -> bool {
    if rect.y == 0 {
        return false;
    }
    let pad = field_pad(app);
    place(app, slot, Placed { rect, pad, fill: color, border: None }, -1)
}

/// A rounded panel exactly covering `rect` with a 1px outline the same
/// distance from every edge (box-drawing borders sit mid-cell, so they're
/// twice as far from the top/bottom edges as from the sides). Drawn under
/// cell backgrounds, so highlighted cells still show on top.
pub fn panel(app: &mut App, slot: u32, rect: Rect, fill: (u8, u8, u8), border: (u8, u8, u8)) -> bool {
    place(app, slot, Placed { rect, pad: 0, fill, border: Some(border) }, -1_073_741_825)
}

fn place(app: &mut App, slot: u32, want: Placed, z: i32) -> bool {
    use ratatui_image::picker::ProtocolType;
    let Placed { rect, pad, fill, border } = want;
    if app.picker.protocol_type() != ProtocolType::Kitty || rect.width == 0 || rect.height == 0 {
        return false;
    }
    app.underlays_seen.insert(slot);
    if app.underlays.get(&slot) == Some(&want) {
        return true;
    }

    let font = app.picker.font_size();
    let (cw, ch) = (font.width.max(1) as u32, font.height.max(1) as u32);
    let (w, h) = (rect.width as u32 * cw, rect.height as u32 * ch + 2 * pad);
    let radius = (ch as f32 * if border.is_some() { 0.35 } else { 0.3 }).max(3.0);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = rounded_alpha(x as f32, y as f32, w as f32, h as f32, radius);
            let c = match border {
                // Fill inside a 1px-inset rounded rect, outline around it
                Some(b) if w > 2 && h > 2 => {
                    let t = rounded_alpha(x as f32 - 1.0, y as f32 - 1.0, w as f32 - 2.0, h as f32 - 2.0, radius - 1.0);
                    let mix = |o: u8, i: u8| (o as f32 + (i as f32 - o as f32) * t).round() as u8;
                    [mix(b.0, fill.0), mix(b.1, fill.1), mix(b.2, fill.2)]
                }
                _ => [fill.0, fill.1, fill.2],
            };
            rgba.extend_from_slice(&[c[0], c[1], c[2], (a * 255.0) as u8]);
        }
    }

    // A field is anchored on the row above, offset down so its box starts
    // `pad` px above the text row; a panel sits on its own cells
    let id = ID_BASE + slot;
    let (row, y_off) = if pad > 0 { (rect.y - 1, ch - pad) } else { (rect.y, 0) };
    let mut out = format!("\x1b7\x1b[{};{}H", row + 1, rect.x + 1);
    let data = base64::engine::general_purpose::STANDARD.encode(&rgba);
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            let _ = write!(
                out,
                "\x1b_Ga=T,f=32,s={w},v={h},i={id},p=1,Y={y_off},z={z},C=1,q=2,m={more};",
            );
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push_str("\x1b\\");
    }
    out.push_str("\x1b8");
    app.post_draw.push(out);
    app.underlays.insert(slot, want);
    true
}

/// Call once per frame after rendering: removes underlays whose field
/// wasn't drawn this frame (e.g. search closed).
pub fn finish_frame(app: &mut App) {
    let gone: Vec<u32> = app.underlays.keys().copied().filter(|s| !app.underlays_seen.contains(s)).collect();
    for slot in gone {
        app.underlays.remove(&slot);
        app.post_draw.push(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", ID_BASE + slot));
    }
    app.underlays_seen.clear();
}

/// Coverage of pixel (x, y) in a w×h rounded rectangle (antialiased).
fn rounded_alpha(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (px, py) = (x + 0.5, y + 0.5);
    if px < 0.0 || py < 0.0 || px > w || py > h {
        return 0.0;
    }
    let dx = (r - px).max(px - (w - r)).max(0.0);
    let dy = (r - py).max(py - (h - r)).max(0.0);
    let d = (dx * dx + dy * dy).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}
