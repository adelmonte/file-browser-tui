use std::{path::Path, process::{Command, Stdio}};

pub fn ffmpegthumbnailer(source: &Path, out: &Path) -> anyhow::Result<()> {
    let status = Command::new("ffmpegthumbnailer")
        .args([
            "-i", source.to_str().unwrap_or(""),
            "-o", out.to_str().unwrap_or(""),
            "-s", "256",
            "-q", "8",
        ])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("ffmpegthumbnailer failed for {:?}", source);
    }
    Ok(())
}

/// Generate an image thumbnail in-process with the image crate (no magick spawn).
pub fn image_thumb(source: &Path, out: &Path) -> anyhow::Result<()> {
    let img = image::open(source)?;
    let thumb = img.thumbnail(256, 256);
    thumb.save_with_format(out, image::ImageFormat::Png)?;
    Ok(())
}
