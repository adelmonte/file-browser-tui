use std::{
    io::ErrorKind,
    path::Path,
    process::{Command, Stdio},
};

use image::DynamicImage;

/// Video frame via ffmpegthumbnailer, falling back to plain ffmpeg when it
/// isn't installed.
pub fn video_thumb(source: &Path, tmp: &Path) -> anyhow::Result<DynamicImage> {
    match ffmpegthumbnailer(source, tmp) {
        Err(e) if is_not_found(&e) => ffmpeg(source, tmp)?,
        r => r?,
    }
    let img = image::open(tmp);
    let _ = std::fs::remove_file(tmp);
    Ok(img?)
}

fn ffmpegthumbnailer(source: &Path, out: &Path) -> anyhow::Result<()> {
    let status = Command::new("ffmpegthumbnailer")
        .arg("-i").arg(source)
        .arg("-o").arg(out)
        .args(["-s", "256", "-q", "8", "-c", "png"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("ffmpegthumbnailer failed for {:?}", source);
    }
    Ok(())
}

fn ffmpeg(source: &Path, out: &Path) -> anyhow::Result<()> {
    // Seek ~15% in to skip intros/black leaders; the thumbnail filter then
    // picks the most representative of the next few frames.
    let seek = duration_secs(source).map(|d| d * 0.15).unwrap_or(0.0);
    let run = |seek: f64| {
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-ss", &format!("{seek:.2}"), "-i"])
            .arg(source)
            .args([
                "-frames:v", "1",
                "-vf", "thumbnail=30,scale=256:256:force_original_aspect_ratio=decrease",
                "-f", "image2", "-c:v", "png",
            ])
            .arg(out)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status()
    };
    let mut ok = run(seek)?.success() && out.exists();
    // Seeking past the end of very short clips yields no frame; retry from 0
    if !ok && seek > 0.0 {
        ok = run(0.0)?.success() && out.exists();
    }
    if !ok {
        anyhow::bail!("ffmpeg failed for {:?}", source);
    }
    Ok(())
}

fn duration_secs(source: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(source)
        .stdin(Stdio::null()).stderr(Stdio::null())
        .output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == ErrorKind::NotFound)
}

/// Image thumbnail in-process with the image crate (no magick spawn).
pub fn image_thumb(source: &Path) -> anyhow::Result<DynamicImage> {
    Ok(image::open(source)?.thumbnail(256, 256))
}
