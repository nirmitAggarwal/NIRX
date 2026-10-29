//! Icon theme selection and recoloured folder icons.
//!
//! Nook uses the icon theme from the desktop (Colloid is preferred when it is
//! installed) for file-type icons, and can tint the theme's folder icon to a
//! user-chosen colour. Tinting is done once per (colour, size) pair on the raw
//! RGBA pixels and cached, because list/grid items are recycled while scrolling.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk::gdk;
use gtk::gdk_pixbuf;
use gtk::gio;
use gtk::prelude::*;

/// Folder icon looked up from the active icon theme before being recoloured.
pub const FOLDER_ICON: &str = "folder";

/// Default colour used when the user has not chosen one, matching Colloid's
/// folder blue.
pub const DEFAULT_FOLDER_COLOR: &str = "#5294e2";

/// Preset swatches offered by the folder colour dialog.
pub const FOLDER_COLOR_PRESETS: &[(&str, &str)] = &[
    ("Blue", "#5294e2"),
    ("Teal", "#16a085"),
    ("Green", "#3fa34d"),
    ("Lime", "#8bbf3f"),
    ("Yellow", "#e5b73b"),
    ("Orange", "#e08a3c"),
    ("Red", "#d64545"),
    ("Pink", "#d4568f"),
    ("Purple", "#8e5bc4"),
    ("Indigo", "#5c6bc0"),
    ("Slate", "#6b7a8f"),
    ("Graphite", "#4a4f57"),
];

thread_local! {
    static FOLDER_TEXTURES: RefCell<HashMap<(u32, i32), Option<gdk::Texture>>> =
        RefCell::new(HashMap::new());
}

/// Per-folder colour configuration: an optional default for every folder plus
/// explicit overrides keyed by absolute path.
#[derive(Default, Clone, Debug)]
pub struct FolderStyle {
    pub default: Option<String>,
    pub overrides: HashMap<PathBuf, String>,
    /// Subfolders inherit the nearest ancestor's colour when they have none of
    /// their own.
    pub inherit: bool,
}

impl FolderStyle {
    /// Colour to paint `path` with, if any.
    ///
    /// Resolution order: the folder's own colour, then (when inheritance is on)
    /// the nearest ancestor with a colour, then the global default.
    pub fn color_for(&self, path: &Path) -> Option<&str> {
        if let Some(color) = self.overrides.get(path) {
            return Some(color);
        }
        if self.inherit {
            for ancestor in path.ancestors().skip(1) {
                if let Some(color) = self.overrides.get(ancestor) {
                    return Some(color);
                }
            }
        }
        self.default.as_deref()
    }

    /// True when `path` gets its colour from an ancestor rather than itself.
    pub fn inherits_from_parent<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        if !self.inherit || self.overrides.contains_key(path) {
            return None;
        }
        path.ancestors()
            .skip(1)
            .find(|ancestor| self.overrides.contains_key(*ancestor))
    }

    /// True when `path` has an explicit colour of its own.
    pub fn has_override(&self, path: &Path) -> bool {
        self.overrides.contains_key(path)
    }
}

/// Icon themes installed in the XDG icon directories.
pub fn installed_themes() -> Vec<String> {
    let mut out = Vec::new();
    for dir in icon_roots() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.join("index.theme").is_file() {
                if let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
    }
    out.sort_by_key(|a| a.to_lowercase());
    out
}

fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data).join("icons"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/icons"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".icons"));
    }
    roots.push(PathBuf::from("/usr/share/icons"));
    if let Some(dirs) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&dirs) {
            roots.push(dir.join("icons"));
        }
    }
    roots
}

/// Pick the icon theme Nook should use: the explicit setting when present,
/// otherwise Colloid (dark variant first) when installed.
pub fn preferred_theme(configured: Option<&str>, prefer_dark: bool) -> Option<String> {
    if let Some(name) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(name.to_string());
    }
    pick_colloid(&installed_themes(), prefer_dark)
}

/// Choose the best Colloid variant from `installed`, preferring the dark (or
/// light) family and the plain theme over colour variants.
fn pick_colloid(installed: &[String], prefer_dark: bool) -> Option<String> {
    let is_dark = |name: &str| {
        let lower = name.to_lowercase();
        lower.contains("dark") || lower.contains("darker")
    };
    let mut colloid: Vec<&String> = installed
        .iter()
        .filter(|theme| theme.to_lowercase().contains("colloid"))
        .collect();
    colloid.sort_by_key(|name| {
        let lower = name.to_lowercase();
        let base = matches!(lower.as_str(), "colloid" | "colloid-dark" | "colloid-light");
        let dark_mismatch = if prefer_dark {
            !is_dark(name)
        } else {
            is_dark(name)
        };
        (!base, dark_mismatch as u8, lower)
    });
    colloid.first().map(|found| (*found).clone())
}

/// Apply an icon theme to this process only (never to the system).
pub fn apply_icon_theme(name: &str) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_icon_theme_name(Some(name));
    }
    FOLDER_TEXTURES.with(|cache| cache.borrow_mut().clear());
}

/// Parse `#rgb` / `#rrggbb` (the leading `#` is optional).
pub fn parse_hex(input: &str) -> Option<(u8, u8, u8)> {
    let s = input.trim().trim_start_matches('#');
    let expand = |c: char| -> Option<u8> {
        let v = c.to_digit(16)? as u8;
        Some(v * 17)
    };
    match s.len() {
        3 => {
            let mut chars = s.chars();
            let r = expand(chars.next()?)?;
            let g = expand(chars.next()?)?;
            let b = expand(chars.next()?)?;
            Some((r, g, b))
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

/// Normalise any accepted colour spelling to `#rrggbb`.
pub fn normalize_hex(input: &str) -> Option<String> {
    let (r, g, b) = parse_hex(input)?;
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// A recoloured folder icon for the given colour and pixel size.
///
/// Returns `None` when the active theme has no raster/SVG folder icon we can
/// tint (for example a symbolic-only theme); callers then fall back to the
/// plain themed icon.
pub fn folder_texture(color: &str, size: i32) -> Option<gdk::Texture> {
    let (r, g, b) = parse_hex(color)?;
    let key = (
        (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        size,
    );
    if let Some(hit) = FOLDER_TEXTURES.with(|cache| cache.borrow().get(&key).cloned()) {
        return hit;
    }
    let texture = build_folder_texture((r, g, b), size);
    FOLDER_TEXTURES.with(|cache| {
        cache.borrow_mut().insert(key, texture.clone());
    });
    texture
}

fn build_folder_texture(rgb: (u8, u8, u8), size: i32) -> Option<gdk::Texture> {
    let display = gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    let paint = theme.lookup_icon(
        FOLDER_ICON,
        &["folder-symbolic"],
        size,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    );
    if paint.is_symbolic() {
        return None;
    }
    let file = paint.file()?;
    let stream = file.read(gio::Cancellable::NONE).ok()?;
    // Render at exactly the size the view asks for: `GtkImage` does not resize
    // paintables the way it resizes icon names, so the texture's intrinsic size
    // must match the row/cell size.
    let render_size = size.clamp(16, 256);
    let pixbuf = gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        render_size,
        render_size,
        true,
        gio::Cancellable::NONE,
    )
    .ok()?;
    let pixbuf = if pixbuf.n_channels() == 4 {
        pixbuf
    } else {
        pixbuf.add_alpha(false, 0, 0, 0).ok()?
    };

    let width = pixbuf.width();
    let height = pixbuf.height();
    let rowstride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();
    let mut data = bytes.as_ref().to_vec();
    tint_rgba(&mut data, rgb);

    let out = gdk_pixbuf::Pixbuf::from_bytes(
        &gtk::glib::Bytes::from(&data),
        pixbuf.colorspace(),
        true,
        8,
        width,
        height,
        rowstride as i32,
    );
    Some(gdk::Texture::for_pixbuf(&out))
}

/// Map the luminance of every pixel onto the target colour, preserving alpha
/// and the icon's own highlights/shadows.
fn tint_rgba(data: &mut [u8], rgb: (u8, u8, u8)) {
    let (tr, tg, tb) = (f64::from(rgb.0), f64::from(rgb.1), f64::from(rgb.2));
    for px in data.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let lum = (0.299 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.114 * f64::from(px[2]))
            / 255.0;
        // The theme folder is mostly mid-tone; this keeps the light top and
        // dark bottom edges while shifting the hue to the chosen colour.
        let shade = (lum / 0.62).clamp(0.28, 1.18);
        px[0] = (tr * shade).clamp(0.0, 255.0) as u8;
        px[1] = (tg * shade).clamp(0.0, 255.0) as u8;
        px[2] = (tb * shade).clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colours() {
        assert_eq!(parse_hex("#5294e2"), Some((0x52, 0x94, 0xe2)));
        assert_eq!(parse_hex("5294e2"), Some((0x52, 0x94, 0xe2)));
        assert_eq!(parse_hex("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#ffff"), None);
        assert_eq!(parse_hex("not-a-colour"), None);
        assert_eq!(normalize_hex("#ABC").as_deref(), Some("#aabbcc"));
    }

    #[test]
    fn tint_keeps_alpha_and_shifts_hue() {
        let mut data = vec![255, 255, 255, 255, 0, 0, 0, 0, 128, 128, 128, 255];
        tint_rgba(&mut data, (255, 0, 0));
        assert_eq!(data[3], 255);
        assert!(data[0] > data[1] && data[0] > data[2]);
        // Fully transparent pixels are left untouched.
        assert_eq!(&data[4..8], &[0, 0, 0, 0]);
        // Grey mid-tone becomes a red mid-tone.
        assert!(data[8] > data[9] && data[8] > data[10]);
    }

    #[test]
    fn colloid_variant_preference() {
        let installed = vec![
            "Colloid-Teal-Dark".to_string(),
            "Colloid".to_string(),
            "Papirus".to_string(),
        ];
        assert_eq!(pick_colloid(&installed, true).as_deref(), Some("Colloid"));
        let with_dark = vec!["Colloid".to_string(), "Colloid-Dark".to_string()];
        assert_eq!(
            pick_colloid(&with_dark, true).as_deref(),
            Some("Colloid-Dark")
        );
        assert_eq!(pick_colloid(&with_dark, false).as_deref(), Some("Colloid"));
        let no_colloid = vec!["Papirus".to_string()];
        assert_eq!(pick_colloid(&no_colloid, true), None);
    }

    #[test]
    fn style_prefers_overrides_over_default() {
        let mut style = FolderStyle {
            default: Some("#112233".into()),
            overrides: HashMap::new(),
            inherit: true,
        };
        let path = PathBuf::from("/tmp/one");
        assert_eq!(style.color_for(&path), Some("#112233"));
        style.overrides.insert(path.clone(), "#445566".into());
        assert_eq!(style.color_for(&path), Some("#445566"));
        assert!(!style.has_override(Path::new("/tmp/two")));
    }

    #[test]
    fn subfolders_inherit_nearest_ancestor_colour() {
        let mut style = FolderStyle {
            default: Some("#000000".into()),
            overrides: HashMap::new(),
            inherit: true,
        };
        style
            .overrides
            .insert(PathBuf::from("/repo/design"), "#ff0000".into());
        style
            .overrides
            .insert(PathBuf::from("/repo/design/icons"), "#00ff00".into());

        // Own colour wins.
        assert_eq!(
            style.color_for(Path::new("/repo/design/icons")),
            Some("#00ff00")
        );
        // Deeper folder inherits the nearest ancestor.
        assert_eq!(
            style.color_for(Path::new("/repo/design/icons/16x16")),
            Some("#00ff00")
        );
        assert_eq!(
            style.color_for(Path::new("/repo/design/src")),
            Some("#ff0000")
        );
        // Outside any coloured ancestor -> default.
        assert_eq!(style.color_for(Path::new("/repo/other")), Some("#000000"));
        assert_eq!(
            style
                .inherits_from_parent(Path::new("/repo/design/icons/16x16"))
                .and_then(|p| p.to_str()),
            Some("/repo/design/icons")
        );

        style.inherit = false;
        assert_eq!(
            style.color_for(Path::new("/repo/design/icons/16x16")),
            Some("#000000")
        );
    }
}
