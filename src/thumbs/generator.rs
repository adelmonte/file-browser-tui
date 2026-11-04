use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
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

/// EPUB cover, read straight from the archive: the OPF's declared cover
/// image (EPUB 3 `cover-image` property or EPUB 2 `<meta name="cover">`),
/// else an image named like a cover.
pub fn epub_thumb(source: &Path) -> anyhow::Result<DynamicImage> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(source)?)?;
    let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::new();
        zip.by_name(name)?.take(64 << 20).read_to_end(&mut buf)?;
        Ok(buf)
    };

    let container = String::from_utf8_lossy(&read(&mut zip, "META-INF/container.xml")?).into_owned();
    let opf_path = tags(&container, "rootfile").into_iter()
        .find_map(|t| attr(&t, "full-path"))
        .ok_or_else(|| anyhow::anyhow!("no rootfile"))?;
    let opf = String::from_utf8_lossy(&read(&mut zip, &opf_path)?).into_owned();
    let base = opf_path.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));

    let items = tags(&opf, "item");
    let is_image = |t: &str| attr(t, "media-type").is_some_and(|m| m.starts_with("image/"));
    let cover_id = tags(&opf, "meta").into_iter()
        .find(|t| attr(t, "name").as_deref() == Some("cover"))
        .and_then(|t| attr(&t, "content"));
    let href = items.iter()
        .find(|t| attr(t, "properties").is_some_and(|p| p.split_whitespace().any(|p| p == "cover-image")))
        .or_else(|| items.iter().find(|t| cover_id.is_some() && attr(t, "id") == cover_id && is_image(t)))
        .or_else(|| items.iter().find(|t| is_image(t)
            && [attr(t, "id"), attr(t, "href")].iter().flatten().any(|v| v.to_lowercase().contains("cover"))))
        .and_then(|t| attr(t, "href"))
        .ok_or_else(|| anyhow::anyhow!("no cover"))?;

    let name = resolve_href(&base, &percent_decode(&href));
    Ok(image::load_from_memory(&read(&mut zip, &name)?)?.thumbnail(256, 256))
}

/// The opening tags named `name` (with or without a namespace prefix).
fn tags(xml: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let tag_name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        if tag_name == name || tag_name.rsplit(':').next() == Some(name) {
            out.push(tag.to_string());
        }
        rest = &rest[end..];
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest[..i].ends_with(|c: char| c.is_whitespace());
        let after = rest[i + name.len()..].trim_start();
        if before_ok && let Some(v) = after.strip_prefix('=') {
            let v = v.trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let v = &v[1..];
                return Some(v[..v.find(q)?].replace("&amp;", "&"));
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// Join an OPF-relative href onto the OPF's directory, folding "..".
fn resolve_href(base: &str, href: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Embedded album art via ffmpeg.
pub fn audio_thumb(source: &Path, tmp: &Path) -> anyhow::Result<DynamicImage> {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(source)
        .args([
            "-an", "-map", "0:v:0", "-frames:v", "1",
            "-vf", "scale=256:256:force_original_aspect_ratio=decrease",
            "-f", "image2", "-c:v", "png",
        ])
        .arg(tmp)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status()?;
    let img = (status.success() && tmp.exists()).then(|| image::open(tmp));
    let _ = std::fs::remove_file(tmp);
    match img {
        Some(img) => Ok(img?),
        None => anyhow::bail!("no album art in {:?}", source),
    }
}

/// The preview OpenDocument files carry at Thumbnails/thumbnail.png.
pub fn opendocument_thumb(source: &Path) -> anyhow::Result<DynamicImage> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(source)?)?;
    let mut buf = Vec::new();
    zip.by_name("Thumbnails/thumbnail.png")?.take(16 << 20).read_to_end(&mut buf)?;
    Ok(image::load_from_memory(&buf)?.thumbnail(256, 256))
}

/// First page (by name) of a CBZ comic.
pub fn comic_thumb(source: &Path) -> anyhow::Result<DynamicImage> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(source)?)?;
    let mut pages: Vec<String> = zip.file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .filter(|n| {
            let n = n.to_lowercase();
            [".jpg", ".jpeg", ".png", ".webp", ".gif"].iter().any(|e| n.ends_with(e)) && !n.starts_with("__macosx")
        })
        .collect();
    pages.sort();
    let first = pages.first().ok_or_else(|| anyhow::anyhow!("no pages"))?;
    let mut buf = Vec::new();
    zip.by_name(first)?.take(64 << 20).read_to_end(&mut buf)?;
    Ok(image::load_from_memory(&buf)?.thumbnail(256, 256))
}

/// First page of a PDF via poppler's pdftoppm.
pub fn pdf_thumb(source: &Path, tmp: &Path) -> anyhow::Result<DynamicImage> {
    // pdftoppm appends ".png" to the output prefix
    let prefix = tmp.with_extension("");
    let status = Command::new("pdftoppm")
        .args(["-png", "-f", "1", "-l", "1", "-singlefile", "-scale-to", "256"])
        .arg(source).arg(&prefix)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status()?;
    let out = PathBuf::from(format!("{}.png", prefix.display()));
    let img = status.success().then(|| image::open(&out));
    let _ = std::fs::remove_file(&out);
    match img {
        Some(img) => Ok(img?),
        None => anyhow::bail!("pdftoppm failed for {:?}", source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opf_parsing() {
        let opf = r#"<package><manifest>
            <opf:item id="c" href="images/My%20Cover.jpg" media-type="image/jpeg"/>
            <item id="x" href="text/a.xhtml" media-type="application/xhtml+xml" properties="nav"/>
            </manifest><metadata><meta name="cover" content="c"/></metadata></package>"#;
        let items = tags(opf, "item");
        assert_eq!(items.len(), 2);
        assert_eq!(attr(&items[0], "id").as_deref(), Some("c"));
        assert_eq!(attr(&items[1], "properties").as_deref(), Some("nav"));
        assert_eq!(attr(&tags(opf, "meta")[0], "content").as_deref(), Some("c"));
        assert_eq!(resolve_href("OEBPS/", &percent_decode("../images/My%20Cover.jpg")), "images/My Cover.jpg");
    }
}

