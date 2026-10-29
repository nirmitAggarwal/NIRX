//! Dynamic theming.
//!
//! DankMaterialShell writes its generated Material 3 palette to
//! `~/.cache/DankMaterialShell/dms-colors.json` (a `mode` plus complete
//! `colors.dark` / `colors.light` token sets) and mirrors a subset into
//! `~/.config/gtk-4.0/dank-colors.css` for GTK apps.
//!
//! Nook prefers the JSON cache because it carries the full token set, falls
//! back to the GTK `@define-color` file, and finally to a built-in dark palette
//! so the app always starts. A `GioFileMonitor` watches both locations so a
//! wallpaper change is reflected without restarting Nook.

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Dark,
    Light,
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub mode: Mode,
    pub tokens: HashMap<String, String>,
    pub radius: i32,
    pub font: Option<String>,
    /// Human readable description of where the palette came from.
    pub source: String,
}

const DARK_TOKENS: &[(&str, &str)] = &[
    ("background", "#10141a"),
    ("surface", "#10141a"),
    ("surface_dim", "#10141a"),
    ("surface_bright", "#363941"),
    ("surface_container_lowest", "#0b0e15"),
    ("surface_container_low", "#181c22"),
    ("surface_container", "#10141a"),
    ("surface_container_high", "#1c2026"),
    ("surface_container_highest", "#262a31"),
    ("surface_variant", "#404753"),
    ("on_surface", "#e0e2ec"),
    ("on_surface_variant", "#c0c7d5"),
    ("on_background", "#e0e2ec"),
    ("outline", "#8a919f"),
    ("outline_variant", "#404753"),
    ("primary", "#b8d07a"),
    ("on_primary", "#273500"),
    ("primary_container", "#3b4d03"),
    ("on_primary_container", "#d4ec94"),
    ("secondary", "#e4bada"),
    ("on_secondary", "#43273f"),
    ("secondary_container", "#5c3d57"),
    ("on_secondary_container", "#ffd7f5"),
    ("tertiary", "#abc8f7"),
    ("on_tertiary", "#103158"),
    ("error", "#ffb4ab"),
    ("on_error", "#690005"),
    ("error_container", "#93000a"),
    ("on_error_container", "#ffdad6"),
    ("inverse_surface", "#e0e2ec"),
    ("inverse_on_surface", "#2d3038"),
    ("inverse_primary", "#52651c"),
    ("surface_tint", "#b8d07a"),
    ("scrim", "#000000"),
    ("shadow", "#000000"),
];

const LIGHT_TOKENS: &[(&str, &str)] = &[
    ("background", "#f9f9ff"),
    ("surface", "#f9f9ff"),
    ("surface_dim", "#d7dae3"),
    ("surface_bright", "#f9f9ff"),
    ("surface_container_lowest", "#ffffff"),
    ("surface_container_low", "#f1f3fd"),
    ("surface_container", "#f9f9ff"),
    ("surface_container_high", "#ebedf7"),
    ("surface_container_highest", "#e5e8f1"),
    ("surface_variant", "#dce2f2"),
    ("on_surface", "#181c22"),
    ("on_surface_variant", "#404753"),
    ("on_background", "#181c22"),
    ("outline", "#717785"),
    ("outline_variant", "#c0c7d5"),
    ("primary", "#52651c"),
    ("on_primary", "#ffffff"),
    ("primary_container", "#d4ec94"),
    ("on_primary_container", "#151f00"),
    ("secondary", "#75546f"),
    ("on_secondary", "#ffffff"),
    ("secondary_container", "#ffd7f5"),
    ("on_secondary_container", "#2c1229"),
    ("tertiary", "#435f89"),
    ("on_tertiary", "#ffffff"),
    ("error", "#ba1a1a"),
    ("on_error", "#ffffff"),
    ("error_container", "#ffdad6"),
    ("on_error_container", "#410002"),
    ("inverse_surface", "#2d3038"),
    ("inverse_on_surface", "#eef0fa"),
    ("inverse_primary", "#b8d07a"),
    ("surface_tint", "#52651c"),
    ("scrim", "#000000"),
    ("shadow", "#000000"),
];

pub fn dms_cache_path() -> PathBuf {
    crate::settings::home_dir().join(".cache/DankMaterialShell/dms-colors.json")
}

pub fn gtk_colors_path() -> PathBuf {
    crate::settings::home_dir().join(".config/gtk-4.0/dank-colors.css")
}

pub fn dms_settings_path() -> PathBuf {
    crate::settings::home_dir().join(".config/DankMaterialShell/settings.json")
}

impl Palette {
    pub fn fallback() -> Self {
        Self {
            mode: Mode::Dark,
            tokens: DARK_TOKENS
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            radius: 12,
            font: None,
            source: "built-in fallback".into(),
        }
    }

    /// Look up a token, returning a safe default when the palette omits it.
    pub fn c(&self, name: &str) -> String {
        if let Some(v) = self.tokens.get(name) {
            let v = v.trim();
            if v.starts_with('#') && (v.len() == 7 || v.len() == 4 || v.len() == 9) {
                return v.to_string();
            }
        }
        // Fall back to the built-in table for the active mode, then to dark.
        let table = if self.mode == Mode::Light {
            LIGHT_TOKENS
        } else {
            DARK_TOKENS
        };
        table
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
            .or_else(|| {
                DARK_TOKENS
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_else(|| "#000000".to_string())
    }

    fn with_mode_tokens(mode: Mode) -> Self {
        let table = if mode == Mode::Light {
            LIGHT_TOKENS
        } else {
            DARK_TOKENS
        };
        Self {
            mode,
            tokens: table
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            radius: 12,
            font: None,
            source: "built-in fallback".into(),
        }
    }

    /// Load the active palette using the DMS cache, the GTK colours file and
    /// finally the built-in theme as a fallback chain.
    pub fn load(force_mode: Option<Mode>) -> Self {
        let mut palette = Self::from_dms_cache()
            .or_else(Self::from_gtk_css)
            .unwrap_or_else(|| match force_mode {
                Some(mode) => Self::with_mode_tokens(mode),
                None => Self::fallback(),
            });

        if let Some(mode) = force_mode {
            if mode != palette.mode {
                // Re-read the other mode's tokens from the cache when available.
                if let Some(other) = Self::from_dms_cache_for(mode) {
                    palette.tokens = other.tokens;
                } else {
                    let table = if mode == Mode::Light {
                        LIGHT_TOKENS
                    } else {
                        DARK_TOKENS
                    };
                    for (k, v) in table {
                        palette.tokens.insert((*k).to_string(), (*v).to_string());
                    }
                }
                palette.mode = mode;
            }
        }

        palette.apply_dms_preferences();
        palette
    }

    fn from_dms_cache() -> Option<Self> {
        let text = std::fs::read_to_string(dms_cache_path()).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let mode = match value.get("mode").and_then(|m| m.as_str()) {
            Some("light") => Mode::Light,
            _ => Mode::Dark,
        };
        Self::from_dms_value(&value, mode)
    }

    fn from_dms_cache_for(mode: Mode) -> Option<Self> {
        let text = std::fs::read_to_string(dms_cache_path()).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        Self::from_dms_value(&value, mode)
    }

    fn from_dms_value(value: &serde_json::Value, mode: Mode) -> Option<Self> {
        let key = if mode == Mode::Light { "light" } else { "dark" };
        let colors = value.get("colors")?.get(key)?.as_object()?;
        let mut tokens = HashMap::new();
        for (k, v) in colors {
            if let Some(s) = v.as_str() {
                tokens.insert(k.clone(), s.to_string());
            }
        }
        if tokens.is_empty() {
            return None;
        }
        Some(Self {
            mode,
            tokens,
            radius: 12,
            font: None,
            source: format!("DankMaterialShell palette ({key})"),
        })
    }

    /// `dank-colors.css` only carries the Adwaita-style aliases; map them onto
    /// the Material token names the UI uses.
    fn from_gtk_css() -> Option<Self> {
        let text = std::fs::read_to_string(gtk_colors_path()).ok()?;
        let mut defines: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("@define-color ") else {
                continue;
            };
            let mut parts = rest.splitn(2, char::is_whitespace);
            let (Some(name), Some(value)) = (parts.next(), parts.next()) else {
                continue;
            };
            let value = value.trim().trim_end_matches(';').trim().to_string();
            defines.insert(name.to_string(), value);
        }
        if defines.is_empty() {
            return None;
        }

        let mut palette = Self::with_mode_tokens(Mode::Dark);
        let aliases = [
            ("window_bg_color", "background"),
            ("window_fg_color", "on_surface"),
            ("sidebar_bg_color", "surface_container_high"),
            ("sidebar_fg_color", "on_surface"),
            ("card_bg_color", "surface_container_high"),
            ("popover_bg_color", "surface_container_high"),
            ("view_bg_color", "surface"),
            ("view_fg_color", "on_surface"),
            ("accent_bg_color", "primary"),
            ("accent_fg_color", "on_primary"),
            ("destructive_bg_color", "error"),
            ("destructive_fg_color", "on_error"),
        ];
        for (from, to) in aliases {
            if let Some(v) = defines.get(from) {
                palette.tokens.insert(to.to_string(), v.clone());
            }
        }
        palette.source = "GTK dank-colors.css".into();
        // Crude light-mode detection: a light background means light tokens.
        if let Some(bg) = defines.get("window_bg_color") {
            if let Some(luma) = hex_luma(bg) {
                if luma > 0.6 {
                    palette.mode = Mode::Light;
                }
            }
        }
        Some(palette)
    }

    /// Pull window radius / font family out of DMS settings so Nook visually
    /// matches the shell. Read-only: we never write to these files.
    fn apply_dms_preferences(&mut self) {
        let Ok(text) = std::fs::read_to_string(dms_settings_path()) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        if let Some(r) = value.get("cornerRadius").and_then(|v| v.as_i64()) {
            self.radius = (r as i32).clamp(0, 28);
        }
        if let Some(f) = value.get("fontFamily").and_then(|v| v.as_str()) {
            if !f.is_empty() {
                self.font = Some(f.to_string());
            }
        }
    }

    pub fn is_dark(&self) -> bool {
        self.mode == Mode::Dark
    }

    /// Complete stylesheet for the application.
    pub fn css(&self) -> String {
        let mut out = String::with_capacity(16 * 1024);
        for (name, _) in DARK_TOKENS {
            out.push_str(&format!("@define-color nook_{} {};\n", name, self.c(name)));
        }
        // Derived tones used for hover/selection washes.
        out.push_str(&format!(
            "@define-color nook_selection alpha(@nook_primary, 0.16);\n\
             @define-color nook_hover alpha(@nook_on_surface, 0.06);\n\
             @define-color nook_hover_strong alpha(@nook_on_surface, 0.10);\n\
             @define-color nook_border alpha(@nook_outline_variant, 0.55);\n\
             @define-color nook_border_soft alpha(@nook_outline_variant, 0.32);\n\
             @define-color nook_muted alpha(@nook_on_surface_variant, 0.72);\n\
             @define-color nook_file_selection alpha(@nook_primary, 0.30);\n\
             @define-color nook_file_selection_border alpha(@nook_primary, 0.55);\n"
        ));

        let radius = self.radius.max(0);
        let inner = (radius - 4).max(2);
        let small = (radius - 6).max(2);
        let font_rule = match &self.font {
            Some(font) => format!(
                "window.nook {{ font-family: \"{}\"; }}\n",
                font.replace('"', "")
            ),
            None => String::new(),
        };

        out.push_str(
            &TEMPLATE
                .replace("$RADIUS", &radius.to_string())
                .replace("$INNER", &inner.to_string())
                .replace("$SMALL", &small.to_string()),
        );
        out.push_str(&font_rule);

        // Folder-colour swatches. One class per preset keeps the picker free of
        // per-widget style providers (deprecated since GTK 4.10).
        for (_name, color) in crate::icons::FOLDER_COLOR_PRESETS {
            out.push_str(&format!(
                ".nook-swatch-{} {{ background-color: {}; background-image: none; }}\n",
                color.trim_start_matches('#'),
                color
            ));
        }
        out
    }
}

fn hex_luma(value: &str) -> Option<f32> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

const TEMPLATE: &str = r#"
window.nook {
  background-color: @nook_background;
  color: @nook_on_surface;
}

headerbar.nook-header {
  background-color: @nook_surface_container_low;
  box-shadow: none;
  border: none;
  border-bottom: 1px solid @nook_border_soft;
  min-height: 0;
  padding: 4px 6px 5px 6px;
}

.nook-topbar { background: transparent; }

/* ---------- tabbed top bar ---------- */
.nook-tabbar {
  background: transparent;
  padding: 0 0 3px 0;
}
.nook-tab-scroll {
  background: transparent;
  border: none;
  box-shadow: none;
}
.nook-tab-scroll scrollbar {
  opacity: 0;
  min-height: 4px;
  transition: opacity 140ms ease;
}
/* At the minimum tab width the strip scrolls instead of shrinking further;
   the scrollbar becomes visible on hover so the extra tabs are discoverable. */
.nook-tab-scroll:hover scrollbar { opacity: 0.65; }
.nook-tab {
  background: transparent;
  border: 1px solid transparent;
  border-radius: $SMALLpx;
  min-height: 42px;
  /* Paint-only states: the 1px border is present in every state, so hovering
     or activating a tab can never change its width. */
  transition: background-color 120ms ease, border-color 120ms ease;
}
.nook-tab:hover { background-color: @nook_hover; }
.nook-tab.active {
  background-color: @nook_surface_container_high;
  border-color: @nook_border_soft;
}
.nook-tab-main {
  background: transparent;
  border: none;
  box-shadow: none;
  padding: 3px 4px;
  min-height: 34px;
}
/* Adwaita's `button:hover` rule is more specific than `.nook-tab-main`, so
   without this the hovered tab picks up a gradient, a border colour and a
   brighter icon — the jump the user sees. Everything here is paint-only, so
   the tab keeps its size on hover. */
.nook-tab-main:hover {
  background: transparent;
  background-image: none;
  border: none;
  box-shadow: none;
  color: @nook_on_surface;
}
.nook-tab-title {
  font-weight: 600;
  font-size: 0.9em;
  color: @nook_on_surface;
}
.nook-tab-location {
  font-size: 0.72em;
  color: @nook_muted;
}
.nook-tab.active .nook-tab-title { color: @nook_primary; }
.nook-tab-close {
  min-width: 20px;
  min-height: 20px;
  padding: 0;
  margin-right: 3px;
  border: none;
  border-radius: 6px;
  background: transparent;
  background-image: none;
  box-shadow: none;
  color: @nook_muted;
}
.nook-tab-close:hover {
  background-color: @nook_hover_strong;
  background-image: none;
  border: none;
  box-shadow: none;
  color: @nook_on_surface;
}
.nook-tab-add {
  min-width: 32px;
  min-height: 32px;
  margin-left: 3px;
}
.nook-toolbar { padding: 1px 0 0 0; }
.nook-toolbar-scroll {
  background: transparent;
  border: none;
  box-shadow: none;
}
.nook-toolbar-scroll scrollbar { min-height: 4px; }

.nook-root { background: transparent; }

/* ---------- sidebar ---------- */
.nook-sidebar {
  background-color: @nook_surface_container_low;
  border: 1px solid @nook_border_soft;
  border-radius: $RADIUSpx;
  margin: 4px 0 8px 8px;
  padding: 8px 6px;
}
.nook-sidebar scrollbar { opacity: 0; }
.nook-sidebar scrollbar:hover { opacity: 1; }

.nook-section-label {
  color: @nook_muted;
  font-size: 0.76em;
  font-weight: 700;
  letter-spacing: 0.06em;
  padding: 10px 10px 4px 10px;
}

/* Fixed header strip at the top of the sidebar, holding the collapse toggle
   so the button does not scroll away with the location list. */
.nook-sidebar-header {
  padding: 6px 6px 4px 6px;
  border-bottom: 1px solid @nook_border_soft;
}
.nook-sidebar-toggle {
  min-width: 30px;
  min-height: 30px;
  padding: 2px;
  border-radius: $SMALLpx;
  background: transparent;
  color: @nook_on_surface_variant;
  -gtk-icon-size: 18px;
}
.nook-sidebar-toggle:hover { background-color: @nook_hover; color: @nook_on_surface; }
.nook-sidebar-toggle.active { background-color: @nook_selection; color: @nook_primary; }

/* Collapsed rail: every row is the same square hit area and its icon sits in
   the middle of the button instead of against the left edge. The row height is
   pinned in code as well, so hover, active and pressed states cannot resize a
   row and nudge the ones below it. */
.nook-mini .nook-place,
.nook-mini .nook-place:hover,
.nook-mini .nook-place:active,
.nook-mini .nook-place.active {
  border: none;
  box-shadow: none;
  margin: 1px 0;
  padding: 8px 4px;
  min-height: 28px;
}
.nook-mini .nook-place image { margin: 0; }
.nook-mini .nook-sidebar-toggle,
.nook-mini .nook-sidebar-toggle:hover,
.nook-mini .nook-sidebar-toggle:active {
  min-width: 40px;
  min-height: 40px;
  padding: 0;
  border: none;
  box-shadow: none;
}

.nook-place {
  border-radius: $SMALLpx;
  border: none;
  box-shadow: none;
  /* A small margin between rows keeps the icons from looking cramped. */
  margin: 1px 0;
  padding: 6px 8px;
  min-height: 26px;
  /* Paint-only: every state below changes colour, never geometry, so hovering
     a row cannot move it or its neighbours. */
  transition: background-color 120ms cubic-bezier(0.2, 0, 0, 1),
              color 120ms cubic-bezier(0.2, 0, 0, 1);
}
.nook-place:hover,
.nook-place:active,
.nook-place.active {
  border: none;
  box-shadow: none;
  margin: 1px 0;
}
.nook-place:hover { background-color: @nook_hover; }
.nook-place.active {
  background-color: @nook_selection;
  color: @nook_primary;
}
.nook-place.active label { color: @nook_primary; }
.nook-place label { color: @nook_on_surface; }
.nook-place .nook-place-sub { color: @nook_muted; font-size: 0.82em; }

/* ---------- toolbar ---------- */
.nook-btn {
  min-width: 34px;
  min-height: 34px;
  padding: 4px;
  border-radius: $SMALLpx;
  background: transparent;
  color: @nook_on_surface_variant;
  -gtk-icon-size: 20px;
  transition: background-color 110ms ease, color 110ms ease;
}
.nook-btn:hover { background-color: @nook_hover; color: @nook_on_surface; }
.nook-btn:active { background-color: @nook_hover_strong; }
.nook-btn.active { background-color: @nook_selection; color: @nook_primary; }
.nook-btn.primary { color: @nook_primary; }

.nook-crumb-bar {
  background-color: transparent;
  border-radius: $SMALLpx;
  padding: 0 2px;
}
.nook-crumb {
  border-radius: $SMALLpx;
  padding: 4px 8px;
  min-height: 24px;
  background: transparent;
  color: @nook_on_surface_variant;
  transition: background-color 110ms ease, color 110ms ease;
}
.nook-crumb:hover { background-color: @nook_hover; color: @nook_on_surface; }
.nook-crumb.current { color: @nook_on_surface; font-weight: 600; background-color: alpha(@nook_on_surface, 0.03); }
.nook-crumb-sep { color: @nook_muted; opacity: 0.5; padding: 0 1px; }

.nook-search {
  border-radius: $SMALLpx;
  background-color: @nook_surface_container_high;
  border: 1px solid @nook_border_soft;
  min-height: 30px;
  padding: 0 2px;
  transition: border-color 120ms ease, background-color 120ms ease;
}
.nook-search:focus-within { border-color: @nook_primary; background-color: @nook_surface_container_highest; }
.nook-search entry { background: transparent; border: none; box-shadow: none; }

.nook-search-mode {
  background-color: @nook_surface_container_high;
  border: 1px solid @nook_border_soft;
  border-radius: $SMALLpx;
  min-height: 30px;
  padding: 0 6px;
  color: @nook_on_surface_variant;
}
.nook-search-mode:hover { background-color: @nook_surface_container_highest; }
.nook-search-mode button { border-radius: $INNERpx; }

/* Segmented icon / detailed view switch. */
.nook-view-switch {
  background-color: @nook_surface_container_high;
  border: 1px solid @nook_border_soft;
  border-radius: $SMALLpx;
  padding: 2px;
}
.nook-view-switch button.nook-btn {
  min-width: 28px;
  min-height: 26px;
  padding: 2px;
  border-radius: $INNERpx;
  background: transparent;
}
.nook-view-switch button.nook-btn:hover { background-color: @nook_hover; }
.nook-view-switch button.nook-btn:checked {
  background-color: @nook_selection;
  color: @nook_primary;
}

.nook-swatch {
  border-radius: 999px;
  border: 2px solid alpha(@nook_on_surface, 0.14);
  box-shadow: none;
  padding: 0;
  min-width: 26px;
  min-height: 26px;
}
.nook-swatch:hover { border-color: alpha(@nook_on_surface, 0.5); }
.nook-swatch.selected {
  border-color: @nook_on_surface;
  box-shadow: 0 0 0 2px alpha(@nook_primary, 0.6);
}

.nook-theme-list row { border-radius: $INNERpx; border-bottom: 1px solid @nook_border_soft; }
.nook-theme-list row:hover { background-color: @nook_hover; }
.nook-theme-list row:selected { background-color: @nook_selection; }

/* ---------- library views (Trash, recents, network) ---------- */
.nook-library-actions {
  padding: 8px 12px;
  border-top: 1px solid @nook_border_soft;
}
.nook-library { background: transparent; }
.nook-library row {
  border-radius: $INNERpx;
  border-bottom: 1px solid @nook_border_soft;
}
.nook-library row:hover { background-color: @nook_hover; }
.nook-library row:selected { background-color: @nook_selection; }
.nook-current-folder { background-color: alpha(@nook_primary, 0.10); }

/* ---------- git dashboard ---------- */
.nook-git-header { background: transparent; }
.nook-git-chip {
  background-color: @nook_surface_container_high;
  border: 1px solid @nook_border_soft;
  border-radius: 999px;
  padding: 2px 10px;
  font-size: 0.82em;
  color: @nook_on_surface_variant;
}
.nook-git-sidebar {
  background-color: @nook_surface_container_low;
  border: 1px solid @nook_border_soft;
  border-radius: $RADIUSpx;
  padding: 6px 4px;
}
.nook-git-branch {
  background: transparent;
  border: none;
  box-shadow: none;
  border-radius: $INNERpx;
  padding: 5px 8px;
  min-height: 26px;
  color: @nook_on_surface;
}
.nook-git-branch:hover { background-color: @nook_hover; }
.nook-git-branch.selected { background-color: @nook_selection; }
.nook-git-branch.current { color: @nook_primary; font-weight: 600; }
.nook-git-branch-dot { color: @nook_muted; }
.nook-git-branch-dot-current { color: @nook_primary; }
.nook-git-history { background: transparent; }
.nook-git-history row {
  border-radius: $INNERpx;
  padding: 0 2px;
}
.nook-git-history row:hover { background-color: @nook_hover; }
.nook-git-history row:selected { background-color: @nook_selection; }
.nook-git-graph {
  font-family: monospace;
  font-size: 0.86em;
  color: @nook_muted;
}
.nook-git-subject { color: @nook_on_surface; }
.nook-git-count {
  font-weight: 700;
  color: @nook_on_surface_variant;
  padding: 0 5px;
}
.nook-git-clean { color: @nook_primary; font-size: 0.86em; }
.nook-heat { border-radius: 2px; }
.nook-heat-0 { background-color: alpha(@nook_on_surface, 0.06); }
.nook-heat-1 { background-color: alpha(@nook_primary, 0.25); }
.nook-heat-2 { background-color: alpha(@nook_primary, 0.45); }
.nook-heat-3 { background-color: alpha(@nook_primary, 0.68); }
.nook-heat-4 { background-color: @nook_primary; }

/* ---------- file surfaces ---------- */
/* The working area is a fixed, very dark surface so selection and hover
   washes are equally readable regardless of the wallpaper palette. */
.nook-surface {
  background-color: #0f1318;
  border: 1px solid @nook_border_soft;
  border-radius: $RADIUSpx;
  margin: 0 8px 8px 8px;
}
.nook-surface-bare {
  background-color: #0f1318;
  border-radius: $RADIUSpx;
}
.nook-surface-bare gridview,
.nook-surface-bare columnview,
.nook-surface-bare columnview > listview,
.nook-surface-bare listview,
.nook-surface-bare listview > row,
.nook-surface-bare gridview > child {
  background-color: transparent;
}

.nook-file {
  border-radius: $INNERpx;
  padding: 5px 6px;
  transition: background-color 90ms ease;
}
.nook-file:hover { background-color: @nook_hover; }
/* Selection state is set on the list row and inherited by the cell. Both
   selectors are listed so a theme reload cannot make the highlight vanish. */
.nook-file:selected,
columnview.nook-list > listview > row:selected .nook-file {
  background-color: @nook_file_selection;
  color: @nook_on_surface;
  box-shadow: inset 0 0 0 1px @nook_file_selection_border;
}
.nook-file:selected:hover,
columnview.nook-list > listview > row:selected .nook-file:hover {
  background-color: @nook_file_selection;
}
.nook-file:selected .nook-file-name { color: @nook_primary; font-weight: 600; }
.nook-file:selected image { color: @nook_primary; }
.nook-file:focus-visible { outline: 1px solid @nook_primary; outline-offset: -1px; }

.nook-file-name { color: @nook_on_surface; }
.nook-file-parent { color: @nook_muted; font-size: 0.8em; font-style: italic; }
.nook-meta { color: @nook_muted; font-size: 0.85em; }

/* Expander used by the lazy recursive view (list rows and grid tiles). */
.nook-tree-toggle {
  min-width: 18px;
  min-height: 18px;
  padding: 0;
  margin: 0 1px;
  border-radius: 4px;
  background: transparent;
  color: @nook_muted;
  -gtk-icon-size: 12px;
}
.nook-tree-toggle:hover { background-color: @nook_hover_strong; color: @nook_on_surface; }
.nook-dim { color: @nook_muted; }
.nook-hidden { opacity: 0.62; }
.nook-git-modified { color: @nook_tertiary; font-weight: 600; }
.nook-git-added { color: @nook_primary; font-weight: 600; }
.nook-git-ignored { opacity: 0.55; }

.nook-grid-cell {
  border-radius: $INNERpx;
  /* Uniform padding keeps the fixed square request square once GTK adds the
     CSS padding on both axes (4px/6px mixed padding made tiles taller). */
  padding: 6px;
  transition: background-color 100ms ease;
}
.nook-grid-cell:hover { background-color: @nook_hover; }
.nook-grid-cell:selected,
gridview > child:selected .nook-grid-cell {
  background-color: @nook_file_selection;
  box-shadow: inset 0 0 0 1px @nook_file_selection_border;
}
.nook-grid-cell:selected:hover,
gridview > child:selected .nook-grid-cell:hover {
  background-color: @nook_file_selection;
}
.nook-grid-cell:selected label,
gridview > child:selected .nook-grid-cell label { color: @nook_primary; }
.nook-grid-cell label { font-size: 0.88em; }
/* A fixed square box for the preview keeps tiles square no matter how wide
   or tall the generated thumbnail is. */
.nook-grid-thumb {
  background: none;
  min-width: 64px;
  min-height: 64px;
}
/* Selection rectangle drawn while dragging over empty space. */
.nook-rubberband {
  background-color: alpha(@nook_primary, 0.18);
  border: 1px solid alpha(@nook_primary, 0.85);
  border-radius: 2px;
}
.nook-window-controls { margin-left: 6px; }

columnview.nook-list > listview > row { padding: 0; }
columnview.nook-list > header > button {
  background: transparent;
  border: none;
  box-shadow: none;
  border-radius: $SMALLpx;
  color: @nook_muted;
  font-weight: 600;
  font-size: 0.82em;
  min-height: 28px;
  padding: 0 6px;
}
columnview.nook-list > header > button:hover { background-color: @nook_hover; color: @nook_on_surface; }

/* ---------- preview pane ---------- */
.nook-preview {
  background-color: @nook_surface_container_low;
  border: 1px solid @nook_border_soft;
  border-radius: $RADIUSpx;
  margin: 0 8px 8px 0;
}
.nook-preview-header {
  padding: 10px 12px 6px 12px;
  border-bottom: 1px solid @nook_border_soft;
}
.nook-preview-title { font-weight: 600; font-size: 0.95em; }
.nook-preview-body { padding: 10px 12px 12px 12px; }
.nook-preview-image {
  background-color: alpha(@nook_on_surface, 0.04);
  border-radius: $INNERpx;
  min-height: 120px;
}
.nook-preview-text {
  background-color: @nook_surface_container_lowest;
  border-radius: $INNERpx;
  padding: 8px;
  font-family: monospace;
  font-size: 0.82em;
}
.nook-props { font-size: 0.86em; }
.nook-props .nook-prop-key { color: @nook_muted; }
.nook-props .nook-prop-value { color: @nook_on_surface; }

/* ---------- misc ---------- */
.nook-status {
  padding: 2px 14px 8px 14px;
  color: @nook_muted;
  font-size: 0.82em;
}
.nook-status .nook-chip {
  background-color: alpha(@nook_on_surface, 0.05);
  border-radius: 6px;
  padding: 1px 7px;
}
.nook-empty-title { font-size: 1.05em; font-weight: 600; color: @nook_on_surface; }
.nook-toast {
  background-color: @nook_surface_container_highest;
  color: @nook_on_surface;
  border: 1px solid @nook_border;
  border-radius: $RADIUSpx;
  padding: 8px 16px;
  box-shadow: 0 6px 20px alpha(@nook_shadow, 0.45);
}
.nook-empty-sub { color: @nook_muted; }

.nook-drop-target {
  background-color: alpha(@nook_primary, 0.22);
  box-shadow: inset 0 0 0 2px alpha(@nook_primary, 0.85);
  border-radius: $RADIUSpx;
}
.nook-drop-target label { color: @nook_primary; font-weight: 600; }

button.nook-action {
  border-radius: $SMALLpx;
  padding: 5px 12px;
  background-color: alpha(@nook_on_surface, 0.06);
  background-image: none;
  border: 1px solid @nook_border_soft;
  transition: background-color 110ms ease;
}
button.nook-action:hover { background-color: alpha(@nook_on_surface, 0.11); }
button.nook-suggested {
  background-color: @nook_primary;
  background-image: none;
  color: @nook_on_primary;
  border-radius: $SMALLpx;
  padding: 5px 12px;
}
button.nook-suggested:hover { background-color: alpha(@nook_primary, 0.88); }
button.nook-destructive {
  background-color: alpha(@nook_error, 0.18);
  background-image: none;
  color: @nook_error;
  border-radius: $SMALLpx;
  padding: 5px 12px;
}
button.nook-destructive:hover { background-color: alpha(@nook_error, 0.28); }

entry.nook-entry {
  border-radius: $SMALLpx;
  background-color: @nook_surface_container_high;
  border: 1px solid @nook_border_soft;
  padding: 4px 8px;
}
entry.nook-entry:focus { border-color: @nook_primary; }
entry.nook-path-entry {
  min-height: 30px;
  /* Same type size as the rest of the toolbar, and a wide natural width so a
     long path shows as much text as possible before it scrolls. */
  font-size: 1em;
  padding: 4px 10px;
}
entry.nook-path-error {
  border-color: @nook_error;
  color: @nook_error;
}
entry.nook-path-error:focus { border-color: @nook_error; }

.popover.nook-popover, popover.nook-popover > contents {
  background-color: @nook_surface_container_high;
  border-radius: $RADIUSpx;
  border: 1px solid @nook_border_soft;
}
.nook-dialog { background-color: @nook_surface_container_low; }
.nook-progress { border-radius: 6px; min-height: 6px; }
.nook-progress trough { background-color: alpha(@nook_on_surface, 0.08); border-radius: 6px; min-height: 6px; }
.nook-progress progress { background-color: @nook_primary; border-radius: 6px; min-height: 6px; }

.nook-quickshift {
  background-color: @nook_surface_container_low;
  border: 1px solid @nook_border_soft;
  border-radius: $RADIUSpx;
}
.nook-qs-dest {
  border-radius: $INNERpx;
  padding: 6px 8px;
}
.nook-qs-dest:hover { background-color: @nook_hover; }
.nook-qs-dest.active { background-color: @nook_selection; }

tooltip { border-radius: 6px; }
"#;

/// What changed in the palette, so the window can refresh mode-dependent bits.
pub struct ThemeWatcher {
    monitors: Vec<gio::FileMonitor>,
    callback: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

impl ThemeWatcher {
    /// Watch the DMS cache and GTK colour files. The callback fires (debounced)
    /// whenever any of them changes on disk.
    pub fn new<F: Fn() + 'static>(callback: F) -> Self {
        let cb: Rc<RefCell<Option<Box<dyn Fn()>>>> =
            Rc::new(RefCell::new(Some(Box::new(callback))));
        let mut watcher = ThemeWatcher {
            monitors: Vec::new(),
            callback: cb.clone(),
        };
        watcher.install();
        watcher
    }

    fn install(&mut self) {
        let cb = self.callback.clone();
        let pending = Rc::new(RefCell::new(false));
        let mut targets: Vec<PathBuf> =
            vec![dms_cache_path(), gtk_colors_path(), dms_settings_path()];
        for t in targets.drain(..) {
            let Some(dir) = t.parent().map(Path::to_path_buf) else {
                continue;
            };
            if !dir.exists() {
                continue;
            }
            let file = gio::File::for_path(&dir);
            let monitor =
                match file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
                    Ok(m) => m,
                    Err(err) => {
                        eprintln!("nook: cannot watch {}: {err}", dir.display());
                        continue;
                    }
                };
            let target = t.clone();
            let cb = cb.clone();
            let pending = pending.clone();
            monitor.connect_changed(move |_monitor, changed, _other, _event| {
                if changed.equal(&gio::File::for_path(&target)) {
                    if *pending.borrow() {
                        return;
                    }
                    *pending.borrow_mut() = true;
                    let cb = cb.clone();
                    let pending = pending.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(180),
                        move || {
                            *pending.borrow_mut() = false;
                            if let Some(f) = cb.borrow().as_ref() {
                                f();
                            }
                        },
                    );
                }
            });
            self.monitors.push(monitor);
        }
    }
}

/// Apply a palette to the whole application. Returns the provider so the
/// caller can remove it when a newer palette arrives.
pub fn apply(palette: &Palette) -> Option<gtk::CssProvider> {
    let display = gdk::Display::default()?;
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&palette.css());
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(palette.is_dark());
    }
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_surface_is_the_fixed_dark_colour() {
        let css = Palette::fallback().css();
        assert!(
            css.contains("background-color: #0f1318"),
            "the file surface must stay #0f1318"
        );
    }

    #[test]
    fn selected_rows_have_a_persistent_highlight() {
        let css = Palette::fallback().css();
        assert!(css.contains("@define-color nook_file_selection"));
        // Both the cell's own :selected state and the inherited row state are
        // styled, so the highlight survives theme reloads and hover.
        assert!(css.contains(".nook-file:selected"));
        assert!(css.contains("row:selected .nook-file"));
        assert!(css.contains(".nook-grid-cell:selected"));
        assert!(css.contains("child:selected .nook-grid-cell"));
    }

    #[test]
    fn grid_tiles_have_a_fixed_square_thumbnail() {
        let css = Palette::fallback().css();
        assert!(css.contains("min-width: 64px"));
        assert!(css.contains("min-height: 64px"));
        // Non-uniform padding made the themed tile taller than wide, so the
        // hover background looked stretched. Padding must stay symmetric.
        let block = css
            .split(".nook-grid-cell {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .unwrap_or_default();
        assert!(
            block.contains("padding: 6px;"),
            "grid tile padding must be uniform: {block}"
        );
        assert!(!block.contains("padding: 6px 4px"));
    }
}
