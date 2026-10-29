//! Preview-based icons: real thumbnails for images, videos and PDFs.
//!
//! Generation runs on worker threads (`ffmpeg` for videos, `pdftoppm` for PDFs,
//! GdkPixBuf for images) and writes PNGs into the cache directory, so the GTK
//! thread only ever reads a small finished file. Textures are remembered in a
//! thread-local map because list/grid items are recycled while scrolling.
//!
//! Every thumbnail is composited onto a square canvas: a texture is shown at
//! its intrinsic size, so a 16:9 video frame would otherwise make its icon cell
//! wider (or taller) than a folder's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use gtk::gdk;
use gtk::gdk_pixbuf;
use gtk::glib;
use crate::fsutil::FileEntry;
use crate::settings::{self, Config};

/// The three categories that can show a preview icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Pdf,
}

/// Which categories currently show preview icons.
#[derive(Clone, Copy, Debug)]
pub struct Flags {
    pub images: bool,
    pub videos: bool,
    pub pdfs: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            images: true,
            videos: true,
            pdfs: true,
        }
    }
}

impl Flags {
    pub fn from_config(config: &Config) -> Self {
        Self {
            images: config.thumbs_images,
            videos: config.thumbs_videos,
            pdfs: config.thumbs_pdfs,
        }
    }

    pub fn allows(&self, kind: Kind) -> bool {
        match kind {
            Kind::Image => self.images,
            Kind::Video => self.videos,
            Kind::Pdf => self.pdfs,
        }
    }

}

thread_local! {
    static TEXTURES: RefCell<HashMap<(PathBuf, u64, i32), Option<gdk::Texture>>> =
        RefCell::new(HashMap::new());
}

/// `kind` when `entry` belongs to a preview category, otherwise `None`.
pub fn kind_for(entry: &FileEntry) -> Option<Kind> {
    if entry.is_dir {
        return None;
    }
    let mime = entry.mime.as_str();
    let ext = entry.extension();
    if mime.starts_with("image/") || matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "svg" | "bmp" | "tiff" | "ico") {
        return Some(Kind::Image);
    }
    if mime.starts_with("video/")
        || matches!(
            ext.as_str(),
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpg" | "mpeg" | "ogv"
        )
    {
        return Some(Kind::Video);
    }
    if mime == "application/pdf" || ext == "pdf" {
        return Some(Kind::Pdf);
    }
    None
}

/// A ready texture for `path`, if it can be produced. Never blocks: the file
/// cache is only read when it exists, otherwise the caller queues generation.
pub fn texture_for(path: &Path, mtime: u64, size: i32, _kind: Kind) -> Option<gdk::Texture> {
    let key = (path.to_path_buf(), mtime, size);
    if let Some(hit) = TEXTURES.with(|cache| cache.borrow().get(&key).cloned()) {
        return hit;
    }
    let texture = read_cache(path, mtime, size)
        .and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(&bytes)).ok());
    TEXTURES.with(|cache| {
        cache.borrow_mut().insert(key, texture.clone());
    });
    texture
}

/// Build a missing thumbnail in the calling (worker) thread. The result lands
/// in the file cache, so the GTK thread picks it up on the next bind.
pub fn prefetch(path: &Path, mtime: u64, size: i32, kind: Kind) -> bool {
    if read_cache(path, mtime, size).is_some() {
        return true;
    }
    let Some(bytes) = generate(path, mtime, size, kind) else {
        return false;
    };
    let file = cache_file(path, mtime, size);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&file, &bytes).is_ok()
}

/// Drop every cached texture, for example when thumbnails are switched off.
pub fn clear_cache() {
    TEXTURES.with(|cache| cache.borrow_mut().clear());
}

/// Forget one entry (its file cache may have just appeared on disk).
pub fn invalidate(path: &Path, mtime: u64, size: i32) {
    TEXTURES.with(|cache| {
        cache.borrow_mut().remove(&(path.to_path_buf(), mtime, size));
    });
}

/// Modification time used as part of the cache key.
pub fn mtime_key(entry: &FileEntry) -> u64 {
    entry
        .modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache(path: &Path, mtime: u64, size: i32) -> Option<Vec<u8>> {
    let file = cache_file(path, mtime, size);
    std::fs::read(&file).ok()
}

fn cache_file(path: &Path, mtime: u64, size: i32) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(mtime.to_le_bytes());
    hasher.update(size.to_le_bytes());
    let digest = hasher.finalize();
    let name: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    settings::cache_dir().join("thumbs").join(format!("{name}.png"))
}

/// Produce a square PNG thumbnail. Runs on a worker thread.
pub fn generate(path: &Path, mtime: u64, size: i32, kind: Kind) -> Option<Vec<u8>> {
    let size = size.clamp(16, 512);
    let raw = match kind {
        Kind::Image => raw_image(path, size),
        Kind::Video => raw_video(path, size),
        Kind::Pdf => raw_pdf(path, size),
    }?;
    let _ = mtime;
    square_png(&raw, size)
}

fn raw_image(path: &Path, size: i32) -> Option<Vec<u8>> {
    let pixbuf =
        gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    encode(&pixbuf)
}

fn raw_video(path: &Path, size: i32) -> Option<Vec<u8>> {
    if which("ffmpeg").is_none() {
        return None;
    }
    let out = std::env::temp_dir().join(format!(
        "nook-thumb-{}-{}.png",
        std::process::id(),
        mtime_suffix()
    ));
    let filter = format!(
        "scale={size}:{size}:force_original_aspect_ratio=decrease,\
         pad={size}:{size}:(ow-iw)/2:(oh-ih)/2:color=black"
    );
    // Seek a couple of seconds in so the first frames of a clip are skipped.
    let mut attempts = vec!["-ss".to_string(), "00:00:02".to_string()];
    for _ in 0..2 {
        let mut args = vec![
            "-y".to_string(),
            "-v".to_string(),
            "error".to_string(),
        ];
        args.append(&mut attempts.clone());
        args.push("-i".to_string());
        args.push(path.display().to_string());
        args.push("-frames:v".to_string());
        args.push("1".to_string());
        args.push("-vf".to_string());
        args.push(filter.clone());
        args.push(out.display().to_string());
        let ok = std::process::Command::new("ffmpeg")
            .args(&args)
            .output()
            .map(|o| o.status.success() && out.exists())
            .unwrap_or(false);
        if ok {
            let bytes = std::fs::read(&out).ok();
            let _ = std::fs::remove_file(&out);
            return bytes;
        }
        // Retry from the very first frame, which also handles short clips.
        attempts = vec!["-ss".to_string(), "0".to_string()];
    }
    let _ = std::fs::remove_file(&out);
    None
}

fn raw_pdf(path: &Path, size: i32) -> Option<Vec<u8>> {
    if which("pdftoppm").is_none() {
        return None;
    }
    let prefix = std::env::temp_dir().join(format!(
        "nook-pdf-{}-{}",
        std::process::id(),
        mtime_suffix()
    ));
    let out = PathBuf::from(format!("{}.png", prefix.display()));
    let status = std::process::Command::new("pdftoppm")
        .args([
            "-png",
            "-singlefile",
            "-scale-to",
            &size.to_string(),
            &path.display().to_string(),
            &prefix.display().to_string(),
        ])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    bytes
}

fn mtime_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

fn which(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn encode(pixbuf: &gdk_pixbuf::Pixbuf) -> Option<Vec<u8>> {
    pixbuf.save_to_bufferv("png", &[]).ok()
}

/// Letterbox `raw` onto a square `size` canvas and encode it as PNG.
fn square_png(raw: &[u8], size: i32) -> Option<Vec<u8>> {
    let stream = gtk::gio::MemoryInputStream::from_bytes(&glib::Bytes::from(raw));
    let source = gdk_pixbuf::Pixbuf::from_stream(&stream, gtk::gio::Cancellable::NONE).ok()?;
    let source = if source.width() == size && source.height() == size {
        source
    } else {
        let canvas = gdk_pixbuf::Pixbuf::new(
            gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            size,
            size,
        )?;
        // Transparent black padding: a letterbox for non-square previews.
        canvas.fill(0);
        let (sw, sh) = (source.width(), source.height());
        let scale = (f64::from(size) / f64::from(sw)).min(f64::from(size) / f64::from(sh));
        let dw = (f64::from(sw) * scale).round().max(1.0) as i32;
        let dh = (f64::from(sh) * scale).round().max(1.0) as i32;
        let scaled = source.scale_simple(dw, dh, gdk_pixbuf::InterpType::Bilinear)?;
        scaled.copy_area(0, 0, dw, dh, &canvas, (size - dw) / 2, (size - dh) / 2);
        canvas
    };
    encode(&source)
}

