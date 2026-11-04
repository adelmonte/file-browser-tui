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
/// Each slot has two, alternated on re-upload: kitty drops an image as soon
/// as a new upload under its id starts, so reusing one id blanks the
/// underlay until the upload finishes (a flicker while resizing).
const ID_BASE: u32 = 0x00F1_E1D0;

fn image_id(app: &App, slot: u32) -> u32 {
    ID_BASE + slot * 2 + u32::from(app.underlays_alt.contains(&slot))
}

/// Where an underlay is currently placed, to skip redundant re-placement.
/// Geometry in pixels from the window's top-left cell corner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    radius: u32,
    fill: (u8, u8, u8),
    border: Option<(u8, u8, u8)>,
    z: i32,
    /// Fill opacity (the border stays opaque).
    alpha: u8,
}

impl Placed {
    /// Same image, possibly elsewhere: only the placement needs moving.
    fn same_image(&self, o: &Placed) -> bool {
        (self.w, self.h, self.radius, self.fill, self.border, self.z, self.alpha) == (o.w, o.h, o.radius, o.fill, o.border, o.z, o.alpha)
    }
}

/// Under cell backgrounds (menus), panels below highlights.
const Z_PANEL: i32 = -1_073_741_830;
const Z_HIGHLIGHT: i32 = -1_073_741_825;
/// Fields go under icons: kitty draws placeholder images at z=-1, and an
/// equal z put the (later uploaded) field on top of the path bar's icon.
const Z_FIELD: i32 = -2;

/// How far (in pixels) a field reaches above and below its text row.
pub fn field_pad(app: &App) -> u32 {
    (app.picker.font_size().height as f32 * 0.32).round() as u32
}

fn cell(app: &App) -> (u32, u32) {
    let font = app.picker.font_size();
    (font.width.max(1) as u32, font.height.max(1) as u32)
}

/// Show a rounded field background behind `rect` (one text row), reaching
/// `pad` pixels into the rows above and below. `slot` identifies the field.
/// Returns false when it can't (not Kitty): fill the cells instead.
pub fn field(app: &mut App, slot: u32, rect: Rect, color: (u8, u8, u8)) -> bool {
    if rect.y == 0 {
        return false;
    }
    let pad = field_pad(app);
    let (cw, ch) = cell(app);
    let radius = (ch as f32 * 0.3).max(3.0) as u32;
    place(app, slot, Placed {
        x: rect.x as u32 * cw, y: rect.y as u32 * ch - pad,
        w: rect.width as u32 * cw, h: rect.height as u32 * ch + 2 * pad,
        radius, fill: color, border: None, z: Z_FIELD, alpha: 255,
    })
}

/// A rounded panel exactly covering `rect` with a 1px outline the same
/// distance from every edge (box-drawing borders sit mid-cell, so they're
/// twice as far from the top/bottom edges as from the sides). Drawn under
/// cell backgrounds, so highlighted cells still show on top.
pub fn panel(app: &mut App, slot: u32, rect: Rect, fill: (u8, u8, u8), border: (u8, u8, u8)) -> bool {
    let (cw, ch) = cell(app);
    let radius = (ch as f32 * 0.35).max(3.0) as u32;
    place(app, slot, Placed {
        x: rect.x as u32 * cw, y: rect.y as u32 * ch,
        w: rect.width as u32 * cw, h: rect.height as u32 * ch,
        radius, fill, border: Some(border), z: Z_PANEL, alpha: 255,
    })
}

/// Margin between a menu panel's edge and an item highlight, on every side.
pub fn highlight_inset(app: &App) -> u32 {
    (cell(app).1 as f32 * 0.15).round().max(2.0) as u32
}

/// A rounded selection highlight for `rect`, shrunk by `inset_x`/`inset_y`
/// pixels, above a panel.
pub fn highlight(app: &mut App, slot: u32, rect: Rect, inset_x: u32, inset_y: u32, color: (u8, u8, u8)) -> bool {
    let (cw, ch) = cell(app);
    let (w, h) = (rect.width as u32 * cw, rect.height as u32 * ch);
    if w <= 2 * inset_x || h <= 2 * inset_y {
        return false;
    }
    let radius = (ch as f32 * 0.25).max(3.0) as u32;
    place(app, slot, Placed {
        x: rect.x as u32 * cw + inset_x, y: rect.y as u32 * ch + inset_y,
        w: w - 2 * inset_x, h: h - 2 * inset_y,
        radius, fill: color, border: None, z: Z_HIGHLIGHT, alpha: 255,
    })
}

/// Ids of the rubber band's pieces: fill, then top, bottom, left, right edges.
const BAND_IDS: [u32; 5] = [ID_BASE + 100, ID_BASE + 101, ID_BASE + 102, ID_BASE + 103, ID_BASE + 104];

/// Show the rubber band over `rect`, or hide it with None. Five tiny images
/// (a translucent pixel, and one-cell edge strips with a 1px line) uploaded
/// once and stretched over the band by kitty, so dragging only re-places
/// them instead of uploading a band-sized image per mouse move.
pub fn selection_band(app: &mut App, rect: Option<Rect>, color: (u8, u8, u8)) -> bool {
    use ratatui_image::picker::ProtocolType;
    if app.picker.protocol_type() != ProtocolType::Kitty {
        return false;
    }
    if rect == app.band_shown {
        return true;
    }
    let mut out = String::new();
    let Some(r) = rect else {
        for id in BAND_IDS {
            // Lowercase: drop the placement, keep the image for next time
            let _ = write!(out, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\");
        }
        app.post_draw.push(out);
        app.band_shown = None;
        return true;
    };
    let (cw, ch) = cell(app);
    if !app.band_uploaded {
        let (c, o) = ([color.0, color.1, color.2, 70], [color.0, color.1, color.2, 255]);
        let edge = |on: &dyn Fn(u32, u32) -> bool| -> Vec<u8> {
            (0..ch).flat_map(|y| (0..cw).flat_map(move |x| if on(x, y) { o } else { [0; 4] })).collect()
        };
        let images: [(u32, u32, Vec<u8>); 5] = [
            (1, 1, c.to_vec()),
            (cw, ch, edge(&|_, y| y == 0)),
            (cw, ch, edge(&|_, y| y == ch - 1)),
            (cw, ch, edge(&|x, _| x == 0)),
            (cw, ch, edge(&|x, _| x == cw - 1)),
        ];
        for (id, (w, h, rgba)) in BAND_IDS.iter().zip(images) {
            let data = base64::engine::general_purpose::STANDARD.encode(&rgba);
            let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
            for (i, chunk) in chunks.iter().enumerate() {
                let more = u8::from(i + 1 < chunks.len());
                if i == 0 {
                    let _ = write!(out, "\x1b_Ga=t,f=32,s={w},v={h},i={id},q=2,m={more};");
                } else {
                    let _ = write!(out, "\x1b_Gm={more};");
                }
                out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
                out.push_str("\x1b\\");
            }
        }
        app.band_uploaded = true;
    }
    let (right, bottom) = (r.right() - 1, r.bottom() - 1);
    let places = [
        (BAND_IDS[0], r.x, r.y, r.width, r.height),
        (BAND_IDS[1], r.x, r.y, r.width, 1),
        (BAND_IDS[2], r.x, bottom, r.width, 1),
        (BAND_IDS[3], r.x, r.y, 1, r.height),
        (BAND_IDS[4], right, r.y, 1, r.height),
    ];
    out.push_str("\x1b7");
    for (id, x, y, c, rows) in places {
        let _ = write!(out, "\x1b[{};{}H\x1b_Ga=p,i={id},p=1,c={c},r={rows},z={Z_FIELD},C=1,q=2\x1b\\", y + 1, x + 1);
    }
    out.push_str("\x1b8");
    app.post_draw.push(out);
    app.band_shown = rect;
    true
}

/// A rounded bar at pixel geometry (x, y, w, h), under the text.
pub fn bar(app: &mut App, slot: u32, (x, y, w, h): (u32, u32, u32, u32), color: (u8, u8, u8)) -> bool {
    place(app, slot, Placed { x, y, w, h, radius: w / 2, fill: color, border: None, z: -1, alpha: 255 })
}

fn place(app: &mut App, slot: u32, want: Placed) -> bool {
    use ratatui_image::picker::ProtocolType;
    if app.picker.protocol_type() != ProtocolType::Kitty || want.w == 0 || want.h == 0 {
        return false;
    }
    app.underlays_seen.insert(slot);
    let prev = app.underlays.get(&slot).copied();
    if prev == Some(want) {
        return true;
    }

    let (cw, ch) = cell(app);
    let Placed { x, y, w, h, radius, fill, border, z, alpha } = want;
    let mut id = image_id(app, slot);
    let (col, row, x_off, y_off) = (x / cw, y / ch, x % cw, y % ch);
    let mut out = format!("\x1b7\x1b[{};{}H", row + 1, col + 1);

    if prev.is_some_and(|p| p.same_image(&want)) {
        // Moved only: re-place the uploaded image
        let _ = write!(out, "\x1b_Ga=p,i={id},p=1,X={x_off},Y={y_off},z={z},C=1,q=2\x1b\\");
    } else {
        // Upload under the other id, then drop the old image
        let old = id;
        if !app.underlays_alt.remove(&slot) {
            app.underlays_alt.insert(slot);
        }
        id = image_id(app, slot);
        let radius = radius as f32;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for py in 0..h {
            for px in 0..w {
                let a = rounded_alpha(px as f32, py as f32, w as f32, h as f32, radius);
                let (c, fa) = match border {
                    // Fill inside a 1px-inset rounded rect, outline around it
                    Some(b) if w > 2 && h > 2 => {
                        let t = rounded_alpha(px as f32 - 1.0, py as f32 - 1.0, w as f32 - 2.0, h as f32 - 2.0, radius - 1.0);
                        let mix = |o: u8, i: u8| (o as f32 + (i as f32 - o as f32) * t).round() as u8;
                        let fa = 1.0 + (alpha as f32 / 255.0 - 1.0) * t;
                        ([mix(b.0, fill.0), mix(b.1, fill.1), mix(b.2, fill.2)], fa)
                    }
                    _ => ([fill.0, fill.1, fill.2], alpha as f32 / 255.0),
                };
                rgba.extend_from_slice(&[c[0], c[1], c[2], (a * fa * 255.0) as u8]);
            }
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&rgba);
        let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let more = u8::from(i + 1 < chunks.len());
            if i == 0 {
                let _ = write!(
                    out,
                    "\x1b_Ga=T,f=32,s={w},v={h},i={id},p=1,X={x_off},Y={y_off},z={z},C=1,q=2,m={more};",
                );
            } else {
                let _ = write!(out, "\x1b_Gm={more};");
            }
            out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            out.push_str("\x1b\\");
        }
        if prev.is_some() {
            let _ = write!(out, "\x1b_Ga=d,d=I,i={old},q=2\x1b\\");
        }
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
        app.post_draw.push(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id(app, slot)));
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


