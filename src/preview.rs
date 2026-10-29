//! Asynchronous preview generation.
//!
//! Previews are produced on worker threads so a multi-megabyte PDF or a 4K
//! photo never blocks the UI. Raster previews are cached on disk, keyed by
//! path + size + modification time, so revisiting a folder is instant.
//!
//! The UI talks to the service through two channels: requests in, results out.

use async_channel::{Receiver, Sender};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::settings;

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub path: PathBuf,
    pub size: u32,
    pub max_bytes: u64,
}

#[derive(Clone, Debug)]
pub enum PreviewKind {
    /// Cached raster preview on disk.
    Image(PathBuf),
    /// Text snippet (already truncated).
    Text {
        snippet: String,
        truncated: bool,
    },
    /// Nothing visual, show metadata only.
    Info(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct PreviewResult {
    pub path: PathBuf,
    pub kind: PreviewKind,
}

#[derive(Clone)]
pub struct PreviewService {
    tx: Sender<PreviewRequest>,
    pub rx: Receiver<PreviewResult>,
}

impl PreviewService {
    pub fn new(workers: usize) -> Self {
        let (tx, request_rx) = async_channel::unbounded::<PreviewRequest>();
        let (result_tx, rx) = async_channel::unbounded::<PreviewResult>();
        let request_rx = Arc::new(request_rx);
        for _ in 0..workers.max(1) {
            let request_rx = request_rx.clone();
            let result_tx = result_tx.clone();
            std::thread::Builder::new()
                .name("nook-preview".into())
                .spawn(move || {
                    while let Ok(req) = request_rx.recv_blocking() {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            build_preview(&req)
                        }))
                        .unwrap_or_else(|_| PreviewResult {
                            path: req.path.clone(),
                            kind: PreviewKind::Error("Preview crashed".into()),
                        });
                        if result_tx.send_blocking(result).is_err() {
                            break;
                        }
                    }
                })
                .ok();
        }
        Self { tx, rx }
    }

    pub fn request(&self, req: PreviewRequest) {
        // A full queue is harmless: the UI re-requests on selection change.
        let _ = self.tx.try_send(req);
    }
}

fn cache_dir() -> PathBuf {
    settings::cache_dir().join("previews")
}

fn cache_key(path: &Path, size: u32) -> String {
    let meta = std::fs::metadata(path).ok();
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(len.to_le_bytes());
    hasher.update(mtime.to_le_bytes());
    hasher.update(size.to_le_bytes());
    let digest = hasher.finalize();
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn prune_cache() {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, e.path()))
        })
        .collect();
    if files.len() <= 400 {
        return;
    }
    files.sort_by_key(|(t, _)| *t);
    for (_, path) in files.iter().take(files.len() - 400) {
        let _ = std::fs::remove_file(path);
    }
}

fn build_preview(req: &PreviewRequest) -> PreviewResult {
    let path = req.path.clone();
    let Ok(meta) = std::fs::metadata(&path) else {
        return PreviewResult {
            path,
            kind: PreviewKind::Error("File is no longer available".into()),
        };
    };
    if meta.is_dir() {
        return PreviewResult {
            path,
            kind: PreviewKind::Info("Folder".into()),
        };
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mime = gtk::gio::content_type_guess(Some(name.as_str()), None::<&[u8]>)
        .0
        .to_string();

    let cached = cache_dir().join(format!("{}.png", cache_key(&path, req.size)));
    if cached.exists() {
        return PreviewResult {
            path,
            kind: PreviewKind::Image(cached),
        };
    }
    let _ = std::fs::create_dir_all(cache_dir());

    let raster = if mime.starts_with("image/") && ext != "svg" {
        render_image(&path, req.size, &cached)
    } else if ext == "svg" {
        render_svg(&path, req.size, &cached)
    } else if mime == "application/pdf" {
        render_pdf(&path, &cached)
    } else if mime.starts_with("video/") || mime.starts_with("audio/") {
        render_media(&path, &cached)
    } else {
        false
    };

    if raster {
        prune_cache();
        return PreviewResult {
            path,
            kind: PreviewKind::Image(cached),
        };
    }

    if mime.starts_with("text/")
        || name.ends_with(".rs")
        || name.ends_with(".toml")
        || name.ends_with(".json")
        || name.ends_with(".md")
        || name.ends_with(".yml")
        || name.ends_with(".yaml")
        || name.ends_with(".ini")
        || name.ends_with(".conf")
        || name.ends_with(".sh")
        || name.ends_with(".css")
        || name.ends_with(".py")
        || name.ends_with(".js")
        || name.ends_with(".ts")
        || name.ends_with(".c")
        || name.ends_with(".h")
        || name.ends_with(".cpp")
        || name.ends_with(".lua")
    {
        return match text_snippet(&path, req.max_bytes) {
            Ok((snippet, truncated)) => PreviewResult {
                path,
                kind: PreviewKind::Text { snippet, truncated },
            },
            Err(err) => PreviewResult {
                path,
                kind: PreviewKind::Error(err.to_string()),
            },
        };
    }

    PreviewResult {
        path,
        kind: PreviewKind::Info(if mime.is_empty() {
            "No preview available".into()
        } else {
            mime
        }),
    }
}

fn render_image(path: &Path, size: u32, out: &Path) -> bool {
    let size = size.clamp(64, 4096) as i32;
    match gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true) {
        Ok(pixbuf) => save_pixbuf(&pixbuf, out),
        Err(_) => false,
    }
}

fn render_svg(path: &Path, size: u32, out: &Path) -> bool {
    let size = size.clamp(64, 4096) as i32;
    match gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true) {
        Ok(pixbuf) => save_pixbuf(&pixbuf, out),
        Err(_) => false,
    }
}

fn save_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf, out: &Path) -> bool {
    if pixbuf.width() <= 0 || pixbuf.height() <= 0 {
        return false;
    }
    // Downscale very large previews so the cache stays small.
    let max = 1024;
    let scaled = if pixbuf.width() > max || pixbuf.height() > max {
        pixbuf.scale_simple(
            max,
            (pixbuf.height() * max / pixbuf.width().max(1)).max(1),
            gdk_pixbuf::InterpType::Bilinear,
        )
    } else {
        None
    };
    let target = scaled.as_ref().unwrap_or(pixbuf);
    target.savev(out, "png", &[] as &[(&str, &str)]).is_ok()
}

fn render_pdf(path: &Path, out: &Path) -> bool {
    let stem = out.with_extension("");
    let result = std::process::Command::new("pdftoppm")
        .args([
            "-f",
            "1",
            "-l",
            "1",
            "-scale-to",
            "1000",
            "-png",
            "-singlefile",
        ])
        .arg(path)
        .arg(&stem)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match result {
        Ok(status) if status.success() => {
            let produced = stem.with_extension("png");
            if produced.exists() && produced != out {
                let _ = std::fs::rename(&produced, out);
            }
            out.exists()
        }
        _ => false,
    }
}

fn render_media(path: &Path, out: &Path) -> bool {
    let attempt = |seek: &str| -> bool {
        let status = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y"])
            .args(["-ss", seek])
            .arg("-i")
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=800:-2:force_original_aspect_ratio=decrease",
            ])
            .arg(out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success()) && out.exists()
    };
    attempt("1") || attempt("0")
}

fn text_snippet(path: &Path, max_bytes: u64) -> std::io::Result<(String, bool)> {
    use std::io::Read;
    let limit = max_bytes.clamp(4096, 512 * 1024) as usize;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64).read_to_end(&mut buf)?;
    let truncated = buf.len() >= limit;
    if buf.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Binary file",
        ));
    }
    let mut text = String::from_utf8_lossy(&buf).to_string();
    if truncated {
        // Avoid ending mid-grapheme on a broken UTF-8 cut.
        while !text.is_char_boundary(text.len().min(limit)) && !text.is_empty() {
            text.pop();
        }
    }
    Ok((text, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_differ_by_size() {
        let p = Path::new("/tmp/does-not-exist-nook-preview");
        assert_ne!(cache_key(p, 128), cache_key(p, 256));
        assert_eq!(cache_key(p, 128), cache_key(p, 128));
    }

    #[test]
    fn text_snippet_limits_and_flags_truncation() {
        let dir = std::env::temp_dir().join("nook-preview-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        std::fs::write(&p, "hello world").unwrap();
        let (text, truncated) = text_snippet(&p, 4096).unwrap();
        assert_eq!(text, "hello world");
        assert!(!truncated);
    }
}
