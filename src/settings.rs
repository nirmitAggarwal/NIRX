//! Persistent configuration: tabs, bookmarks, window state, view preferences,
//! preview visibility, application overrides, shortcuts and Quick Shift history.
//!
//! Everything lives in a single JSON document so that a partially written file
//! can never leave the user without a usable configuration: writes go to a
//! temporary file and are atomically renamed into place.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const APP_DIR_NAME: &str = "nirx";

/// `$XDG_CONFIG_HOME/nirx`, falling back to `~/.config/nirx`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(dir);
        if p.is_absolute() {
            return p.join(APP_DIR_NAME);
        }
    }
    home_dir().join(".config").join(APP_DIR_NAME)
}

/// `$XDG_CACHE_HOME/nirx`, falling back to `~/.cache/nirx`.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        let p = PathBuf::from(dir);
        if p.is_absolute() {
            return p.join(APP_DIR_NAME);
        }
    }
    home_dir().join(".cache").join(APP_DIR_NAME)
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Previous Nook configuration location, read only as a compatibility fallback.
fn legacy_config_path() -> PathBuf {
    let dir = if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            path
        } else {
            home_dir().join(".config")
        }
    } else {
        home_dir().join(".config")
    };
    dir.join("nook").join("config.json")
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Grid,
    List,
    /// Every descendant below the current folder in one indented listing.
    Recursive,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode::List
    }
}

impl ViewMode {
    /// Cycle through the three layouts: icons → details → recursive.
    pub fn toggled(self) -> Self {
        match self {
            ViewMode::Grid => ViewMode::List,
            ViewMode::List => ViewMode::Recursive,
            ViewMode::Recursive => ViewMode::Grid,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

impl Default for SortKey {
    fn default() -> Self {
        SortKey::Name
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
}

/// A named shell command that can be started in a folder's terminal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RunCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TabConfig {
    pub path: String,
    #[serde(default)]
    pub view: ViewMode,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WindowState {
    #[serde(default = "default_width")]
    pub width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
}

fn default_width() -> i32 {
    1180
}
fn default_height() -> i32 {
    760
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            maximized: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "config_version")]
    pub version: u32,
    #[serde(default)]
    pub window: WindowState,
    #[serde(default)]
    pub tabs: Vec<TabConfig>,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub view: ViewMode,
    #[serde(default)]
    pub show_preview: bool,
    #[serde(default = "yes")]
    pub show_sidebar: bool,
    #[serde(default)]
    pub sort: SortKey,
    #[serde(default)]
    pub sort_desc: bool,
    #[serde(default = "default_terminal")]
    pub terminal: Vec<String>,
    #[serde(default = "default_editor")]
    pub editor: String,
    #[serde(default)]
    pub recent_destinations: Vec<String>,
    /// `"<mime or extension>" -> desktop file id`, user-chosen overrides that
    /// take precedence over the system default without modifying it.
    #[serde(default)]
    pub app_overrides: HashMap<String, String>,
    #[serde(default)]
    pub shortcuts: HashMap<String, String>,
    #[serde(default = "yes")]
    pub confirm_delete: bool,
    #[serde(default = "yes")]
    pub follow_theme_mode: bool,
    #[serde(default)]
    pub preview_max_bytes: u64,
    /// Icon theme used by NIRX only. Empty/absent means auto-detect: Colloid
    /// when installed, otherwise the desktop's theme.
    #[serde(default)]
    pub icon_theme: Option<String>,
    /// Colour applied to every folder icon; `None` keeps the theme icon.
    #[serde(default = "default_folder_color")]
    pub folder_color: Option<String>,
    /// Per-folder colours keyed by absolute path.
    #[serde(default)]
    pub folder_colors: HashMap<String, String>,
    /// When true, a folder without its own colour inherits the nearest ancestor
    /// folder's colour instead of jumping straight to the default.
    #[serde(default = "yes")]
    pub folder_color_inherit: bool,
    /// Per-folder run commands keyed by absolute folder path.
    #[serde(default)]
    pub folder_commands: HashMap<String, Vec<RunCommand>>,
    /// Shell commands used to open matching files: keys are an absolute path, an
    /// extension (`.psd`) or a MIME type (`image/x-xcf`). `%f` expands to the
    /// selected file; without a placeholder the quoted paths are appended.
    #[serde(default)]
    pub custom_openers: HashMap<String, String>,
    /// Sidebar shown as icons only instead of the full width list.
    #[serde(default)]
    pub sidebar_mini: bool,
    /// Preview-based icons (real thumbnails) per file category.
    #[serde(default = "yes")]
    pub thumbs_images: bool,
    #[serde(default = "yes")]
    pub thumbs_videos: bool,
    #[serde(default = "yes")]
    pub thumbs_pdfs: bool,
    /// Files opened recently, most recent first.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// LocalSend: name other devices see. Empty means the hostname.
    #[serde(default)]
    pub transfer_alias: Option<String>,
    /// LocalSend: port for the local HTTPS server and multicast discovery.
    #[serde(default = "default_transfer_port")]
    pub transfer_port: u16,
    /// LocalSend: accept transfers without asking. Only trusted devices are
    /// accepted automatically; everyone else still asks.
    #[serde(default)]
    pub transfer_auto_accept: bool,
    /// LocalSend: folder incoming files are written to.
    #[serde(default = "default_receive_dir")]
    pub transfer_receive_dir: Option<String>,
    /// LocalSend: fingerprints of devices allowed to send without asking.
    #[serde(default)]
    pub transfer_trusted: Vec<String>,
    /// LocalSend: participate in discovery and receiving at all.
    #[serde(default = "yes")]
    pub transfer_enabled: bool,
}

fn yes() -> bool {
    true
}
fn default_transfer_port() -> u16 {
    crate::localsend::DEFAULT_PORT
}
fn default_receive_dir() -> Option<String> {
    Some("~/Downloads".to_string())
}
fn config_version() -> u32 {
    1
}

fn default_terminal() -> Vec<String> {
    vec!["kitty".to_string()]
}

fn default_editor() -> String {
    "zed".to_string()
}

fn default_folder_color() -> Option<String> {
    Some(crate::icons::DEFAULT_FOLDER_COLOR.to_string())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            window: WindowState::default(),
            tabs: Vec::new(),
            active_tab: 0,
            bookmarks: Vec::new(),
            show_hidden: false,
            view: ViewMode::List,
            show_preview: false,
            show_sidebar: true,
            sort: SortKey::Name,
            sort_desc: false,
            terminal: default_terminal(),
            editor: default_editor(),
            recent_destinations: Vec::new(),
            app_overrides: HashMap::new(),
            shortcuts: HashMap::new(),
            confirm_delete: true,
            follow_theme_mode: true,
            preview_max_bytes: 8 * 1024 * 1024,
            icon_theme: None,
            folder_color: default_folder_color(),
            folder_colors: HashMap::new(),
            folder_color_inherit: true,
            folder_commands: HashMap::new(),
            custom_openers: HashMap::new(),
            sidebar_mini: false,
            thumbs_images: true,
            thumbs_videos: true,
            thumbs_pdfs: true,
            recent_files: Vec::new(),
            transfer_alias: None,
            transfer_port: default_transfer_port(),
            transfer_auto_accept: false,
            transfer_receive_dir: default_receive_dir(),
            transfer_trusted: Vec::new(),
            transfer_enabled: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        // Keep existing Nook settings readable after the brand change. The old
        // file is deliberately left untouched; the next normal save writes the
        // current configuration under the NIRX directory.
        let legacy_path = legacy_config_path();
        let source = if path.exists() {
            path.clone()
        } else if legacy_path.exists() {
            legacy_path
        } else {
            path.clone()
        };
        match std::fs::read_to_string(&source) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(err) => {
                    eprintln!(
                        "nirx: config {} is invalid ({err}); using defaults",
                        source.display()
                    );
                    // A bad NIRX file can be quarantined, but preserve the
                    // legacy file in place: it may belong to an older install.
                    if source == path {
                        let broken_path = source.with_extension("json.broken");
                        let _ = std::fs::rename(&source, broken_path);
                    }
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let dir = config_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = config_path();
        let tmp = dir.join("config.json.tmp");
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            Err(err) => eprintln!("nirx: could not serialise config: {err}"),
        }
    }

    /// Record a Quick Shift destination, most recent first, bounded in size.
    pub fn push_recent_destination(&mut self, path: &Path) {
        let s = path.to_string_lossy().to_string();
        self.recent_destinations.retain(|p| p != &s);
        self.recent_destinations.insert(0, s);
        self.recent_destinations.truncate(10);
    }

    /// Record an opened file, most recent first, bounded in size.
    pub fn push_recent_file(&mut self, path: &Path) {
        let s = path.to_string_lossy().to_string();
        self.recent_files.retain(|p| p != &s);
        self.recent_files.insert(0, s);
        self.recent_files.truncate(25);
    }

    /// Run commands configured for `path`, ordered as saved.
    pub fn commands_for(&self, path: &Path) -> Vec<RunCommand> {
        self.folder_commands
            .get(&path.to_string_lossy().to_string())
            .cloned()
            .unwrap_or_default()
    }

    /// Shortcut lookup with the built-in default applied by the caller.
    pub fn shortcut(&self, action: &str, fallback: &str) -> String {
        self.shortcuts
            .get(action)
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    }
}

/// Expand a leading `~` and environment variables are not expanded (kept
/// predictable); only the home shortcut is supported.
pub fn expand_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}
