use ratatui::style::Color;

/// Nord palette, as RGB tuples (images get composited onto these) and Colors.
pub const NORD0: (u8, u8, u8) = (46, 52, 64);
pub const NORD2: (u8, u8, u8) = (67, 76, 94);
pub const NORD3: (u8, u8, u8) = (76, 86, 106);
pub const NORD8: (u8, u8, u8) = (136, 192, 208);
pub const NORD9: (u8, u8, u8) = (129, 161, 193);
pub const NORD11: (u8, u8, u8) = (191, 97, 106);
pub const NORD14: (u8, u8, u8) = (163, 190, 140);
pub const NORD13: (u8, u8, u8) = (235, 203, 139);
/// Selection highlight: 50% blend of nord9 with nord0.
pub const SEL: (u8, u8, u8) = (88, 107, 128);
/// Hover highlight: a faint lift, clearly weaker than the selection.
pub const HOVER: (u8, u8, u8) = (60, 68, 84);

pub const fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Recessed fields and popups.
pub const FIELD_BG: Color = rgb(NORD0);
/// Text fields on the window background (location, search): light enough
/// to stand out on a dark translucent terminal.
pub const INPUT: (u8, u8, u8) = (72, 82, 102);
pub const INPUT_BG: Color = rgb(INPUT);
/// Borders, separators, de-emphasized text.
pub const BORDER: Color = rgb(NORD3);
/// Secondary text: dimmer than the body text but readable on dark and
/// translucent terminal backgrounds (nord3 is too dark for text).
pub const MUTED_RGB: (u8, u8, u8) = (150, 160, 180);
pub const MUTED: Color = rgb(MUTED_RGB);
/// Section headers and column titles.
pub const ACCENT: Color = rgb(NORD8);
pub const SEL_BG: Color = rgb(SEL);
pub const HOVER_BG: Color = rgb(HOVER);
/// Body text (nord4).
pub const TEXT_RGB: (u8, u8, u8) = (216, 222, 233);
pub const TEXT: Color = rgb(TEXT_RGB);
