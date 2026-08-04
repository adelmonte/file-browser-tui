use std::io::Write;

use ratatui::layout::Rect;

/// Delete Kitty graphics placements that intersect the top-left cell of each
/// rect (placements are anchored there, so one point per image suffices).
/// Coordinates are 1-based cells per the kitty graphics protocol.
pub fn delete_at(out: &mut impl Write, rects: &[Rect]) -> std::io::Result<()> {
    for r in rects {
        write!(out, "\x1b_Ga=d,d=p,x={},y={}\x1b\\", r.x + 1, r.y + 1)?;
    }
    out.flush()
}

/// Delete every visible placement (bulk changes: navigation, view switch, resize).
pub fn delete_all(out: &mut impl Write) -> std::io::Result<()> {
    write!(out, "\x1b_Ga=d\x1b\\")?;
    out.flush()
}
