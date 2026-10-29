//! Main window: header toolbar, breadcrumbs, sidebar, tabs, preview pane,
//! status bar, actions and keyboard shortcuts.

pub mod dialogs;
pub mod git_pane;
pub mod library;
pub mod pane;
pub mod preview_pane;
pub mod quickshift;
pub mod send;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::apps;
use crate::fsutil::{self, FileEntry};
use crate::icons::{self, FolderStyle};
use crate::localsend;
use crate::preview::PreviewService;
use crate::search::SearchMode;
use crate::settings::{self, Config, SortKey, ViewMode};
use crate::theme::{self, Palette};

use dialogs::{ConflictChoice, ProgressDialog, ProgressMsg};
use library::LibraryKind;
use pane::Pane;
use preview_pane::PreviewPane;

thread_local! {
    /// Strong references to live window states, released when the window is
    /// destroyed. GTK callbacks and actions only capture `Weak` handles, so this
    /// is what actually gives `AppUi` its lifetime.
    static LIVE_STATE: RefCell<Vec<Rc<AppUi>>> = const { RefCell::new(Vec::new()) };
}

/// Sidebar widths: the full list and the icon-only rail.
const SIDEBAR_WIDTH: i32 = 222;
const SIDEBAR_MINI_WIDTH: i32 = 56;

/// Square hit area for an icon in the collapsed rail.
const MINI_ROW_HEIGHT: i32 = 44;

/// Window widths at which the layout adapts. The sidebar becomes an icon rail
/// and finally disappears, so a narrow window keeps the files usable instead
/// of pushing the working surface out of view.
const SIDEBAR_RAIL_BELOW: i32 = 820;
const SIDEBAR_HIDE_BELOW: i32 = 560;
/// Restoring the full sidebar waits for a little more room than collapsing it,
/// so dragging the window edge around the threshold does not flicker.
const SIDEBAR_RESTORE_ABOVE: i32 = 880;


/// Tabs share the available width equally down to this floor, then scroll.
/// The floor stays small so a handful of tabs still fit without scrolling.
const MIN_TAB_WIDTH: i32 = 120;

pub struct Tab {
    /// The notebook page: a stack showing either the folder view or the library
    /// view the tab currently presents.
    pub page: gtk::Widget,
    stack: gtk::Stack,
    /// The directory pane. Every tab owns one, even while a library view is on
    /// screen, so Places, breadcrumbs and the tab strip always have a folder
    /// to fall back to.
    pub pane: Rc<Pane>,
    /// The library view (Trash, recents, network) shown instead of the folder,
    /// if the tab is in that state.
    library: Cell<Option<LibraryKind>>,
    /// Folder the pane showed when the library view was opened. Browsing
    /// anywhere else takes the tab back to its folder.
    library_path: RefCell<Option<PathBuf>>,
    /// Rebuilds the library rows when the tab is shown again.
    library_refresh: RefCell<Option<Rc<dyn Fn()>>>,
    /// Custom tab strip widgets. `title` shows the folder name and `location`
    /// the elided path, so a tab never collapses to an anonymous close box.
    pub strip: gtk::Box,
    pub title: gtk::Label,
    pub location: gtk::Label,
    pub icon: gtk::Image,
}

#[derive(Clone)]
struct ClipboardState {
    paths: Vec<PathBuf>,
    cut: bool,
}

pub struct AppUi {
    pub window: gtk::ApplicationWindow,
    pub config: Rc<RefCell<Config>>,
    pub palette: RefCell<Palette>,
    css_provider: RefCell<Option<gtk::CssProvider>>,
    notebook: gtk::Notebook,
    tabs: RefCell<Vec<Rc<Tab>>>,
    /// Custom native tab strip shown in the top bar.
    tab_strip: gtk::Box,
    tab_scroller: gtk::ScrolledWindow,
    sidebar_box: gtk::Box,
    /// Sidebar column: fixed header with the collapse toggle plus the list.
    sidebar_container: gtk::Box,
    sidebar_toggle: gtk::Button,
    sidebar_paned: gtk::Paned,
    crumb_box: gtk::Box,
    /// Switches the path display between breadcrumbs and an editable field.
    crumb_stack: gtk::Stack,
    path_entry: gtk::Entry,
    path_editing: Cell<bool>,
    status_left: gtk::Label,
    status_right: gtk::Label,
    preview: Rc<PreviewPane>,
    preview_revealer: gtk::Revealer,
    search_revealer: gtk::Revealer,
    search_entry: gtk::SearchEntry,
    search_recursive: gtk::ToggleButton,
    search_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    toast_revealer: gtk::Revealer,
    toast_label: gtk::Label,
    toast_gen: Cell<u64>,
    clipboard: RefCell<Option<ClipboardState>>,
    /// Colours used for folder icons, shared by every pane.
    folder_style: Rc<RefCell<FolderStyle>>,
    /// The entry the currently open context menu belongs to.
    context_target: RefCell<Option<FileEntry>>,
    search_mode: Cell<SearchMode>,
    search_mode_dropdown: RefCell<Option<gtk::DropDown>>,
    /// Sidebar collapsed to an icon rail instead of the full list.
    sidebar_mini: Cell<bool>,
    /// Last adaptive width bucket; the layout only reacts when it changes.
    width_bucket: Cell<i32>,
    view_button: RefCell<Option<gtk::Button>>,
    view_icon: RefCell<gtk::Image>,
    hidden_button: RefCell<gtk::ToggleButton>,
    hidden_icon: RefCell<gtk::Image>,
    preview_button: RefCell<gtk::ToggleButton>,
    run_button: RefCell<Option<gtk::Button>>,
    /// LocalSend service, when it managed to start.
    transfer: RefCell<Option<localsend::Service>>,
    /// Kept so the keyboard can open both menus without a mouse.
    menu_button: RefCell<Option<gtk::MenuButton>>,
    settings_button: RefCell<Option<gtk::MenuButton>>,
    /// Toolbar controls that only make sense for a folder: view mode, hidden
    /// files, search, terminal, run and git. They are greyed out while a
    /// library view is shown, so a click cannot silently act on the folder
    /// hidden behind it.
    folder_controls: RefCell<Vec<gtk::Widget>>,
    nav_back: RefCell<Option<gtk::Button>>,
    nav_fwd: RefCell<Option<gtk::Button>>,
    /// Directory the sidebar/breadcrumb chrome was last built for. Selection
    /// changes fire `refresh_chrome` constantly, so the expensive parts are
    /// only rebuilt when the folder actually changes.
    chrome_path: RefCell<Option<PathBuf>>,
    /// Whether the current folder has run commands, so the toolbar button can
    /// be shown without touching the disk on every selection change.
    run_available: Cell<bool>,
    theme_watcher: RefCell<Option<theme::ThemeWatcher>>,
}

impl AppUi {
    pub fn active_tab(self: &Rc<Self>) -> Option<Rc<Tab>> {
        let idx = self.notebook.current_page()? as usize;
        self.tabs.borrow().get(idx).cloned()
    }

    pub fn active_pane(self: &Rc<Self>) -> Option<Rc<Pane>> {
        self.active_tab().map(|tab| tab.pane.clone())
    }

    fn all_panes(self: &Rc<Self>) -> Vec<Rc<Pane>> {
        self.tabs
            .borrow()
            .iter()
            .map(|tab| tab.pane.clone())
            .collect()
    }

    /// Status-bar text while a library view is on screen.
    pub fn set_status_text(self: &Rc<Self>, text: &str) {
        self.status_left.set_text(text);
    }

    /// Browse a folder in the active tab, leaving a library view if the tab is
    /// currently showing one.
    pub fn open_folder_here(self: &Rc<Self>, path: PathBuf) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        self.leave_library(&tab);
        tab.pane.navigate(path, true);
    }

    /// Show a transient message at the bottom of the window.
    fn toast(self: &Rc<Self>, message: &str) {
        self.toast_label.set_text(message);
        self.toast_revealer.set_reveal_child(true);
        let generation = self.toast_gen.get() + 1;
        self.toast_gen.set(generation);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(2600), move || {
            if let Some(ui) = weak.upgrade() {
                if ui.toast_gen.get() == generation {
                    ui.toast_revealer.set_reveal_child(false);
                }
            }
        });
    }

    /* ---------------------------- local network -------------------------- */

    /// The running LocalSend service, if it started.
    pub fn transfer_service(&self) -> Option<localsend::Service> {
        self.transfer.borrow().clone()
    }

    /// Folder incoming transfers are written to.
    pub fn receive_dir(&self) -> PathBuf {
        self.config
            .borrow()
            .transfer_receive_dir
            .as_deref()
            .map(settings::expand_path)
            .unwrap_or_else(|| settings::home_dir().join("Downloads"))
    }

    /// Start (or leave off) the LocalSend receiver for this window.
    fn start_transfer_service(self: &Rc<Self>) {
        if !self.config.borrow().transfer_enabled {
            return;
        }
        let (sender, receiver) = async_channel::unbounded::<localsend::Event>();
        let config = self.config.borrow().clone();
        match localsend::Service::start(&config, sender) {
            Ok(service) => {
                *self.transfer.borrow_mut() = Some(service);
                // Events arrive from worker threads; this future runs on the GTK
                // main loop, so widgets are only ever touched there.
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    while let Ok(event) = receiver.recv().await {
                        let Some(ui) = weak.upgrade() else {
                            break;
                        };
                        ui.handle_transfer_event(event);
                    }
                });
            }
            Err(err) => eprintln!("nook: LocalSend unavailable: {err}"),
        }
    }

    /// Restart the service after a settings change that binds sockets. The old
    /// listener is shut down first and the new one starts a moment later, so
    /// the port is definitely free when it binds again.
    fn restart_transfer(self: &Rc<Self>) {
        if let Some(service) = self.transfer.borrow_mut().take() {
            service.shutdown();
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.start_transfer_service();
            if ui.config.borrow().transfer_enabled && ui.transfer.borrow().is_none() {
                ui.toast("LocalSend could not start — is the port already in use?");
            }
        });
    }

    fn handle_transfer_event(self: &Rc<Self>, event: localsend::Event) {
        match event {
            localsend::Event::Incoming(request) => send::incoming(self, request),
            localsend::Event::Saved { from, files } => {
                for pane in self.all_panes() {
                    pane.soft_refresh();
                }
                let noun = if files.len() == 1 { "item" } else { "items" };
                self.toast(&format!("Received {} {noun} from {from}", files.len()));
            }
            localsend::Event::Notice(message) => self.toast(&message),
        }
    }

    /// Send the selection with LocalSend.
    fn send_selection(self: &Rc<Self>) {
        send::send_selection(self);
    }

    /// Leave a library view and show the folder the tab holds.
    pub fn close_library(self: &Rc<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if tab.library.get().is_none() {
            return;
        }
        self.leave_library(&tab);
        self.refresh_chrome();
    }

    /// Shift+F10 / Menu key: the context menu for the selection, or for the
    /// current folder when nothing is selected.
    fn context_menu_keyboard(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let entry = pane
            .selected_entries()
            .into_iter()
            .next()
            .or_else(|| FileEntry::from_path(&pane.path(), None));
        let Some(entry) = entry else {
            return;
        };
        let x = f64::from(self.window.width().max(480)) / 3.0;
        let y = f64::from(self.window.height().max(360)) / 3.0;
        self.show_context_menu(&entry, x, y);
    }

    /// Move the keyboard focus into the sidebar.
    fn focus_sidebar(self: &Rc<Self>) {
        if !self.sidebar_container.is_visible() {
            self.toast("The sidebar is hidden at this width");
            return;
        }
        if let Some(first) = sidebar_rows(&self.sidebar_box).first() {
            first.grab_focus();
        }
    }

    /* ----------------------------- theming ----------------------------- */

    fn reload_theme(self: &Rc<Self>) {
        let palette = Palette::load(None);
        if let Some(old) = self.css_provider.borrow_mut().take() {
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_remove_provider_for_display(&display, &old);
            }
        }
        let provider = theme::apply(&palette);
        *self.palette.borrow_mut() = palette;
        *self.css_provider.borrow_mut() = provider;
    }

    /* ----------------------------- chrome ------------------------------ */

    fn refresh_chrome(self: &Rc<Self>) {
        let Some(tab) = self.active_tab() else { return };
        // A library view is a state of its tab: as soon as the folder behind it
        // moves (Places, path field, history, reveal), the tab shows that folder
        // again instead of keeping a view that no longer matches.
        if tab.library.get().is_some() {
            let unchanged = tab
                .library_path
                .borrow()
                .as_deref()
                .map(|p| p == tab.pane.path())
                .unwrap_or(false);
            if !unchanged {
                self.leave_library(&tab);
            }
        }
        if let Some(kind) = tab.library.get() {
            self.refresh_library_chrome(&tab, kind);
            return;
        }
        self.set_folder_controls(true);
        let pane = tab.pane.clone();
        let path = pane.path();

        let path_changed = self.chrome_path.borrow().as_deref() != Some(path.as_path());
        if path_changed {
            self.rebuild_breadcrumbs(&pane);
            self.rebuild_sidebar(&pane);
            *self.chrome_path.borrow_mut() = Some(path.clone());
        }

        let total = pane.item_count();
        let entries = pane.selected_entries();
        let selected = entries.len();
        let noun = if total == 1 { "item" } else { "items" };
        if selected > 0 {
            let size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            self.status_left.set_text(&format!(
                "{total} {noun} · {selected} selected · {}",
                fsutil::human_size(size)
            ));
        } else {
            self.status_left.set_text(&format!("{total} {noun}"));
        }
        self.status_right.set_text(&dialogs::pretty_path(&path));
        if !self.path_editing.get() {
            self.path_entry.set_text(&path.display().to_string());
        }

        // Every tab keeps its own name and location up to date, so switching
        // tabs immediately shows which folder each one holds.
        let active_page = self.notebook.current_page();
        for (index, other) in self.tabs.borrow().iter().enumerate() {
            // Tabs presenting a library view keep their own title and icon.
            if other.library.get().is_none() {
                let other_pane = &other.pane;
                other.title.set_text(&other_pane.current_dir_name());
                other
                    .location
                    .set_text(&dialogs::pretty_path(&other_pane.path()));
                other
                    .strip
                    .set_tooltip_text(Some(&other_pane.path().display().to_string()));
                if let Some(color) = self.folder_style.borrow().color_for(&other_pane.path()) {
                    if let Some(texture) = icons::folder_texture(color, 18) {
                        other.icon.set_paintable(Some(&texture));
                    } else {
                        other.icon.set_icon_name(Some(icons::FOLDER_ICON));
                    }
                } else {
                    other.icon.set_icon_name(Some(icons::FOLDER_ICON));
                }
            }
            if active_page == Some(index as u32) {
                other.strip.add_css_class("active");
            } else {
                other.strip.remove_css_class("active");
            }
        }

        if path_changed {
            // Track visited folders for Recent Locations without writing the
            // config on every navigation; the window close saves it.
            self.config.borrow_mut().push_recent_destination(&path);
            let has_file = crate::run_tasks::locate(&path).is_some();
            let has_legacy = !self.config.borrow().commands_for(&path).is_empty();
            self.run_available.set(has_file || has_legacy);
        }
        if let Some(button) = self.view_button.borrow().as_ref() {
            match pane.view_mode.get() {
                ViewMode::Grid => {
                    self.view_icon
                        .borrow()
                        .set_icon_name(Some("view-grid-symbolic"));
                    button.set_tooltip_text(Some("Switch to detailed view (Ctrl+1)"));
                }
                ViewMode::List => {
                    self.view_icon
                        .borrow()
                        .set_icon_name(Some("view-list-symbolic"));
                    button.set_tooltip_text(Some("Switch to recursive view (Ctrl+1)"));
                }
                ViewMode::Recursive => {
                    self.view_icon
                        .borrow()
                        .set_icon_name(Some("format-indent-more-symbolic"));
                    button.set_tooltip_text(Some("Switch to icon view (Ctrl+1)"));
                }
            }
        }
        {
            let hidden = self.hidden_button.borrow();
            let show_hidden = pane.show_hidden.get();
            if hidden.is_active() != show_hidden {
                hidden.set_active(show_hidden);
            }
            self.hidden_icon.borrow().set_icon_name(Some(if show_hidden {
                "view-reveal-symbolic"
            } else {
                "view-conceal-symbolic"
            }));
            hidden.set_tooltip_text(Some(if show_hidden {
                "Hide hidden files (Ctrl+H)"
            } else {
                "Show hidden files (Ctrl+H)"
            }));
        }
        self.preview_button
            .borrow()
            .set_active(self.preview_revealer.reveals_child());
        if let Some(run) = self.run_button.borrow().as_ref() {
            run.set_visible(self.run_available.get());
        }
        pane.set_search_mode(self.search_mode.get());
        if let Some(button) = self.nav_back.borrow().as_ref() {
            button.set_sensitive(pane.can_go_back());
        }
        if let Some(button) = self.nav_fwd.borrow().as_ref() {
            button.set_sensitive(pane.can_go_forward());
        }
        self.preview.update(&entries, &path);
    }

    /// Chrome for a tab showing a library view, which has no history, folder
    /// toolbar or file selection.
    fn refresh_library_chrome(self: &Rc<Self>, tab: &Rc<Tab>, kind: LibraryKind) {
        // A library page is not a folder: don't let the next refresh treat the
        // library title as an unchanged directory path.
        self.chrome_path.borrow_mut().take();
        self.set_folder_controls(false);

        while let Some(child) = self.crumb_box.first_child() {
            self.crumb_box.remove(&child);
        }
        let crumb = gtk::Button::with_label(kind.title());
        crumb.add_css_class("nook-crumb");
        crumb.add_css_class("current");
        self.crumb_box.append(&crumb);
        self.crumb_stack.set_visible_child_name("crumbs");
        if !self.path_editing.get() {
            self.path_entry.set_text(kind.title());
        }

        self.status_right.set_text(kind.title());
        if let Some(button) = self.nav_back.borrow().as_ref() {
            button.set_sensitive(false);
        }
        if let Some(button) = self.nav_fwd.borrow().as_ref() {
            button.set_sensitive(false);
        }
        self.preview.show_note(kind.title(), kind.hint());
        // Rows change on disk (new trash entries, new recents) while the tab is
        // in the background, so rebuild them whenever it is shown again.
        if let Some(refresh) = tab.library_refresh.borrow().clone() {
            refresh();
        }

        let active_page = self.notebook.current_page();
        for (index, other) in self.tabs.borrow().iter().enumerate() {
            if active_page == Some(index as u32) {
                other.strip.add_css_class("active");
            } else {
                other.strip.remove_css_class("active");
            }
        }
    }

    /// Turn the path display into a text field prefilled with the current path.
    fn start_path_edit(self: &Rc<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        // Typing a location is a request for a folder, not for the library view.
        self.leave_library(&tab);
        self.refresh_chrome();
        self.path_entry.remove_css_class("nook-path-error");
        self.path_entry
            .set_text(&tab.pane.path().display().to_string());
        self.path_editing.set(true);
        self.crumb_stack.set_visible_child_name("edit");
        self.path_entry.grab_focus();
        self.path_entry.select_region(0, -1);
    }

    fn cancel_path_edit(self: &Rc<Self>) {
        self.path_editing.set(false);
        self.path_entry.remove_css_class("nook-path-error");
        self.crumb_stack.set_visible_child_name("crumbs");
    }

    /// Validate the typed or pasted path and navigate only when it is usable.
    fn submit_path_edit(self: &Rc<Self>) {
        let text = self.path_entry.text().to_string();
        let text = text.trim();
        if text.is_empty() {
            self.cancel_path_edit();
            return;
        }
        let path = settings::expand_path(text);
        if path.is_dir() {
            if let Err(err) = std::fs::read_dir(&path) {
                self.path_error(&format!("Cannot open {}: {err}", path.display()));
                return;
            }
            self.cancel_path_edit();
            if let Some(pane) = self.active_pane() {
                pane.navigate(path.clone(), true);
            }
            self.config.borrow_mut().push_recent_destination(&path);
            self.config.borrow().save();
            return;
        }
        if path.is_file() {
            let parent = path
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(settings::home_dir);
            self.cancel_path_edit();
            if let Some(pane) = self.active_pane() {
                pane.navigate(parent, true);
                pane.reveal(path);
            }
            return;
        }
        self.path_error(&format!("No such folder: {}", path.display()));
    }

    fn path_error(self: &Rc<Self>, message: &str) {
        self.path_entry.add_css_class("nook-path-error");
        self.path_entry.grab_focus();
        self.toast(message);
    }

    fn rebuild_breadcrumbs(self: &Rc<Self>, pane: &Rc<Pane>) {
        while let Some(child) = self.crumb_box.first_child() {
            self.crumb_box.remove(&child);
        }
        let crumbs = fsutil::crumbs(&pane.path());
        let len = crumbs.len();
        for (i, (label, path)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::from_icon_name("go-next-symbolic");
                sep.set_pixel_size(11);
                sep.add_css_class("nook-crumb-sep");
                self.crumb_box.append(&sep);
            }
            let btn = gtk::Button::with_label(&label);
            btn.add_css_class("nook-crumb");
            if i + 1 == len {
                btn.add_css_class("current");
            }
            btn.set_tooltip_text(Some(&path.display().to_string()));
            let weak = Rc::downgrade(self);
            let target = path.clone();
            btn.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        pane.navigate(target.clone(), true);
                    }
                }
            });
            self.crumb_box.append(&btn);
        }
    }

    fn rebuild_sidebar(self: &Rc<Self>, pane: &Rc<Pane>) {
        while let Some(child) = self.sidebar_box.first_child() {
            self.sidebar_box.remove(&child);
        }
        let current = pane.path();
        let mini = self.sidebar_mini.get();
        let section = |title: &str| {
            if mini {
                return;
            }
            let label = gtk::Label::new(Some(title));
            label.add_css_class("nook-section-label");
            label.set_xalign(0.0);
            self.sidebar_box.append(&label);
        };

        let home = settings::home_dir();
        section("PLACES");
        let places: Vec<(String, String, PathBuf)> = vec![
            ("Home".into(), "user-home-symbolic".into(), home.clone()),
            (
                "Desktop".into(),
                "user-desktop-symbolic".into(),
                home.join("Desktop"),
            ),
            (
                "Documents".into(),
                "folder-documents-symbolic".into(),
                home.join("Documents"),
            ),
            (
                "Downloads".into(),
                "folder-download-symbolic".into(),
                home.join("Downloads"),
            ),
            (
                "Pictures".into(),
                "folder-pictures-symbolic".into(),
                home.join("Pictures"),
            ),
            (
                "Videos".into(),
                "folder-videos-symbolic".into(),
                home.join("Videos"),
            ),
            (
                "Music".into(),
                "folder-music-symbolic".into(),
                home.join("Music"),
            ),
        ];
        for (name, icon, path) in places {
            if !path.exists() {
                continue;
            }
            self.sidebar_entry(&name, &icon, &path, &current);
        }

        if !self.config.borrow().bookmarks.is_empty() {
            section("BOOKMARKS");
            let bookmarks = self.config.borrow().bookmarks.clone();
            for bm in bookmarks {
                let path = settings::expand_path(&bm.path);
                self.sidebar_entry(&bm.name, "starred-symbolic", &path, &current);
            }
        }

        section("LIBRARY");
        self.sidebar_action(
            "Trash",
            "user-trash-symbolic",
            "Items moved to trash, with restore",
            |ui| ui.show_trash(),
        );
        self.sidebar_action(
            "Recent Files",
            "document-open-recent-symbolic",
            "Files opened recently",
            |ui| ui.show_recent_files(),
        );
        self.sidebar_action(
            "Recent Locations",
            "go-jump-symbolic",
            "Folders visited recently",
            |ui| ui.show_recent_locations(),
        );
        self.sidebar_action(
            "Network",
            "network-server-symbolic",
            "Network shares and remote locations",
            |ui| ui.show_network(),
        );

        section("SYSTEM");
        self.sidebar_entry(
            "File System",
            "drive-harddisk-symbolic",
            &PathBuf::from("/"),
            &current,
        );
        if PathBuf::from("/tmp").exists() {
            self.sidebar_entry(
                "Temporary",
                "folder-temp-symbolic",
                &PathBuf::from("/tmp"),
                &current,
            );
        }
    }

    fn sidebar_entry(self: &Rc<Self>, name: &str, icon: &str, path: &PathBuf, current: &PathBuf) {
        let tip = path.display().to_string();
        let btn = self.sidebar_row(name, icon, &tip);
        if path == current {
            btn.add_css_class("active");
        }
        let weak = Rc::downgrade(self);
        let target = path.clone();
        btn.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_folder_here(target.clone());
            }
        });
        self.sidebar_box.append(&btn);
    }

    /// A sidebar row that runs an action instead of navigating.
    fn sidebar_action(
        self: &Rc<Self>,
        name: &str,
        icon: &str,
        tooltip: &str,
        run: impl Fn(Rc<AppUi>) + 'static,
    ) {
        let btn = self.sidebar_row(name, icon, tooltip);
        let weak = Rc::downgrade(self);
        btn.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                run(ui);
            }
        });
        self.sidebar_box.append(&btn);
    }

    /// Shared layout for a sidebar row: icon first, then an optional label.
    fn sidebar_row(&self, name: &str, icon: &str, tooltip: &str) -> gtk::Button {
        let btn = gtk::Button::new();
        btn.add_css_class("nook-place");
        btn.set_has_frame(false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(16);
        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&image);
        row.append(&label);
        btn.set_child(Some(&row));
        if self.sidebar_mini.get() {
            label.set_visible(false);
            // Icon rail: no text, so the icon takes the whole row and centres
            // itself in the button instead of sitting against the left edge.
            image.set_halign(gtk::Align::Center);
            image.set_hexpand(true);
            btn.set_size_request(-1, MINI_ROW_HEIGHT);
            btn.set_tooltip_text(Some(name));
        } else {
            btn.set_tooltip_text(Some(tooltip));
        }
        btn
    }

    /// Switch between the full sidebar and the icon-only rail. This is the
    /// user's explicit choice, so it is persisted.
    fn set_sidebar_mini(self: &Rc<Self>, mini: bool) {
        {
            let mut cfg = self.config.borrow_mut();
            cfg.sidebar_mini = mini;
            cfg.save();
        }
        self.apply_sidebar_mini(mini);
    }

    /// Apply a rail/full state without touching the saved preference.
    fn apply_sidebar_mini(self: &Rc<Self>, mini: bool) {
        self.sidebar_mini.set(mini);
        let width = if mini { SIDEBAR_MINI_WIDTH } else { SIDEBAR_WIDTH };
        self.sync_sidebar_chrome();
        // Always restore the position: a hidden paned child can leave the
        // divider at zero, which would otherwise bring the sidebar back at no
        // width at all.
        self.sidebar_paned.set_position(width);
        self.rebuild_active_sidebar();
    }

    /// Adapt the sidebar to the window width so the file view always keeps
    /// usable space: full list when there is room, an icon rail when there is
    /// less, and no sidebar at all in a very narrow window.
    fn apply_adaptive_layout(self: &Rc<Self>, width: i32) {
        let bucket = if width < SIDEBAR_HIDE_BELOW {
            0
        } else if width < SIDEBAR_RAIL_BELOW {
            1
        } else {
            2
        };
        let current = self.width_bucket.get();
        if current == bucket {
            return;
        }
        // Coming back to the full-width layout waits until the window is
        // clearly wide again, so dragging around the threshold does not make
        // the sidebar flicker between its two shapes.
        if bucket == 2 && current >= 0 && width < SIDEBAR_RESTORE_ABOVE {
            return;
        }
        self.width_bucket.set(bucket);
        let pref = self.config.borrow().sidebar_mini;
        match bucket {
            0 => {
                self.sidebar_container.set_visible(false);
            }
            1 => {
                self.sidebar_container.set_visible(true);
                if !self.sidebar_mini.get() {
                    self.apply_sidebar_mini(true);
                }
            }
            _ => {
                self.sidebar_container.set_visible(true);
                self.apply_sidebar_mini(pref);
            }
        }
    }

    /// Keep the sidebar's width and the header toggle in sync with the mode.
    fn sync_sidebar_chrome(&self) {
        let mini = self.sidebar_mini.get();
        self.sidebar_container
            .set_width_request(if mini { SIDEBAR_MINI_WIDTH } else { SIDEBAR_WIDTH });
        if mini {
            self.sidebar_container.add_css_class("nook-mini");
        } else {
            self.sidebar_container.remove_css_class("nook-mini");
        }
        let icon = gtk::Image::from_icon_name(if mini {
            "pan-end-symbolic"
        } else {
            "pan-start-symbolic"
        });
        icon.set_pixel_size(18);
        self.sidebar_toggle.set_child(Some(&icon));
        self.sidebar_toggle.set_halign(if mini {
            gtk::Align::Center
        } else {
            gtk::Align::Fill
        });
        self.sidebar_toggle.set_hexpand(mini);
        self.sidebar_toggle
            .set_size_request(-1, if mini { MINI_ROW_HEIGHT } else { -1 });
        self.sidebar_toggle.set_tooltip_text(Some(if mini {
            "Expand the sidebar (Ctrl+B)"
        } else {
            "Collapse the sidebar to icons (Ctrl+B)"
        }));
    }

    /// Grey out the folder-only toolbar controls while a library view is up.
    fn set_folder_controls(&self, enabled: bool) {
        for widget in self.folder_controls.borrow().iter() {
            widget.set_sensitive(enabled);
        }
    }

    /// Rebuild the sidebar for the folder that is currently shown.
    fn rebuild_active_sidebar(self: &Rc<Self>) {
        if let Some(pane) = self.active_pane() {
            self.rebuild_sidebar(&pane);
        }
    }

    fn update_selection_preview(self: &Rc<Self>) {
        if let Some(pane) = self.active_pane() {
            self.preview.update(&pane.selected_entries(), &pane.path());
        }
    }

    /* ------------------------------- tabs ------------------------------ */

    fn add_tab(self: &Rc<Self>, path: PathBuf, view: ViewMode, activate: bool) -> Rc<Tab> {
        let config = self.config.borrow();
        let show_hidden = config.show_hidden;
        let sort = config.sort;
        let sort_desc = config.sort_desc;
        drop(config);

        let pane = Pane::new(
            path.clone(),
            view,
            show_hidden,
            self.folder_style.clone(),
            self.config.clone(),
        );
        pane.set_sort(sort, sort_desc);
        pane.set_search_mode(self.search_mode.get());
        self.wire_pane(&pane);

        // The tab page is a stack: the folder view is always there, and a
        // library view can take its place without disturbing the folder, its
        // scroll position or its selection.
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        stack.add_named(&pane.root, Some("folder"));
        stack.set_visible_child_name("folder");

        self.add_page_tab(
            stack.clone().upcast(),
            stack,
            pane.clone(),
            &pane.current_dir_name(),
            &dialogs::pretty_path(&path),
            icons::FOLDER_ICON,
            Some(&path.display().to_string()),
            activate,
        )
    }

    /// Create the tab strip entry and notebook page for a tab.
    fn add_page_tab(
        self: &Rc<Self>,
        page: gtk::Widget,
        stack: gtk::Stack,
        pane: Rc<Pane>,
        title_text: &str,
        location_text: &str,
        icon_name: &str,
        tooltip: Option<&str>,
        activate: bool,
    ) -> Rc<Tab> {
        let title = gtk::Label::new(Some(title_text));
        title.add_css_class("nook-tab-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(20);
        let location = gtk::Label::new(Some(location_text));
        location.add_css_class("nook-tab-location");
        location.set_xalign(0.0);
        location.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        location.set_max_width_chars(26);
        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text_box.append(&title);
        text_box.append(&location);

        let icon = gtk::Image::from_icon_name(icon_name);
        icon.set_pixel_size(20);
        icon.set_valign(gtk::Align::Center);
        let main_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        main_row.append(&icon);
        main_row.append(&text_box);

        let tab_button = gtk::Button::new();
        tab_button.add_css_class("nook-tab-main");
        tab_button.set_has_frame(false);
        // Expanding pushes the close button to the tab's right edge instead of
        // leaving it next to the folder name.
        tab_button.set_hexpand(true);
        tab_button.set_child(Some(&main_row));

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("nook-tab-close");
        close.set_tooltip_text(Some("Close tab"));
        close.set_focus_on_click(false);

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        strip.add_css_class("nook-tab");
        // Tabs share the strip equally (GTK expands every hexpanding child) but
        // never shrink below this floor; past it the tab bar scrolls instead.
        strip.set_size_request(MIN_TAB_WIDTH, -1);
        strip.set_hexpand(true);
        strip.append(&tab_button);
        strip.append(&close);
        // Hovering a tab always shows the full absolute path, never the
        // elided `~/…` label. Set it on every child as well, so the tooltip
        // appears whichever part of the tab the pointer is over.
        if let Some(tooltip) = tooltip {
            strip.set_tooltip_text(Some(tooltip));
            tab_button.set_tooltip_text(Some(tooltip));
            title.set_tooltip_text(Some(tooltip));
            location.set_tooltip_text(Some(tooltip));
        }
        close.set_tooltip_text(Some("Close tab"));

        let tab = Rc::new(Tab {
            page: page.clone(),
            stack,
            pane,
            library: Cell::new(None),
            library_path: RefCell::new(None),
            library_refresh: RefCell::new(None),
            strip: strip.clone(),
            title: title.clone(),
            location: location.clone(),
            icon,
        });

        let idx = self.notebook.append_page(&page, None::<&gtk::Widget>);
        self.notebook.set_tab_reorderable(&page, true);
        self.notebook.set_tab_detachable(&page, false);
        self.tab_strip.append(&strip);
        self.tabs.borrow_mut().push(tab.clone());

        {
            let weak = Rc::downgrade(self);
            let page = page.clone();
            tab_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(index) = ui.notebook.page_num(&page) {
                        ui.notebook.set_current_page(Some(index));
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let page = page.clone();
            close.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.close_page(&page);
                }
            });
        }

        if activate || self.notebook.n_pages() == 1 {
            self.notebook.set_current_page(Some(idx));
        }
        // Reveal the new tab when the strip is already scrolled to the end.
        {
            let scroller = self.tab_scroller.clone();
            glib::idle_add_local_once(move || {
                let adjustment = scroller.hadjustment();
                adjustment.set_value(adjustment.upper());
            });
        }
        self.refresh_chrome();
        tab
    }

    /// Show a library view (Trash, recents, network) in the active tab. The
    /// view is a state of that tab, not a new tab: the folder it came from is
    /// still there when the user browses again.
    fn open_library(self: &Rc<Self>, kind: LibraryKind) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if tab.library.get() == Some(kind) {
            if let Some(refresh) = tab.library_refresh.borrow().clone() {
                refresh();
            }
            self.refresh_chrome();
            return;
        }
        let library::LibraryView {
            root,
            refresh,
            focus,
        } = library::build(self, kind);
        if let Some(old) = tab.stack.child_by_name("library") {
            tab.stack.remove(&old);
        }
        tab.stack.add_named(&root, Some("library"));
        tab.stack.set_visible_child_name("library");
        tab.library.set(Some(kind));
        *tab.library_path.borrow_mut() = Some(tab.pane.path());
        *tab.library_refresh.borrow_mut() = Some(refresh);
        tab.title.set_text(kind.title());
        tab.location.set_text("Library");
        tab.icon.set_icon_name(Some(kind.icon()));
        tab.strip.set_tooltip_text(Some(kind.hint()));
        self.refresh_chrome();
        focus();
    }

    /// Take a tab back from a library view to the folder it holds.
    fn leave_library(self: &Rc<Self>, tab: &Rc<Tab>) {
        if tab.library.get().is_none() {
            return;
        }
        tab.library.set(None);
        *tab.library_path.borrow_mut() = None;
        *tab.library_refresh.borrow_mut() = None;
        tab.stack.set_visible_child_name("folder");
        tab.title.set_text(&tab.pane.current_dir_name());
        tab.location
            .set_text(&dialogs::pretty_path(&tab.pane.path()));
        tab.icon.set_icon_name(Some(icons::FOLDER_ICON));
        tab.strip
            .set_tooltip_text(Some(&tab.pane.path().display().to_string()));
        // The breadcrumbs and toolbar must be rebuilt for the folder again.
        self.chrome_path.borrow_mut().take();
    }

    fn close_page(self: &Rc<Self>, page: &gtk::Widget) {
        let Some(idx) = self.notebook.page_num(page) else {
            return;
        };
        let idx = idx as usize;
        if let Some(tab) = self.tabs.borrow().iter().find(|tab| tab.page == *page) {
            self.tab_strip.remove(&tab.strip);
        }
        self.tabs.borrow_mut().retain(|t| t.page != *page);
        self.notebook.remove_page(Some(idx as u32));
        if self.notebook.n_pages() == 0 {
            // Always keep at least one tab alive.
            let home = settings::home_dir();
            let view = self.config.borrow().view;
            self.add_tab(home, view, true);
        }
        self.refresh_chrome();
    }

    fn close_active_tab(self: &Rc<Self>) {
        if let Some(tab) = self.active_tab() {
            let page = tab.page.clone();
            self.close_page(&page);
        }
    }

    fn duplicate_tab(self: &Rc<Self>) {
        if let Some(pane) = self.active_pane() {
            let path = pane.path();
            let view = pane.view_mode.get();
            self.add_tab(path, view, true);
        }
    }

    fn wire_pane(self: &Rc<Self>, pane: &Rc<Pane>) {
        pane.wire_activation();

        let weak = Rc::downgrade(self);
        pane.on_changed(move || {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_chrome();
                ui.update_selection_preview();
            }
        });

        // Type-to-filter inside the pane mirrors into the search bar. Revealing
        // the bar must not steal focus, so the pane takes it back afterwards.
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(pane);
        pane.set_on_search(move |text| {
            if let Some(ui) = weak.upgrade() {
                if ui.search_entry.text() != text {
                    ui.search_entry.set_text(text);
                }
                ui.search_revealer.set_reveal_child(!text.is_empty());
                if let Some(pane) = pane_weak.upgrade() {
                    pane.focus_view();
                }
            }
        });

        let weak = Rc::downgrade(self);
        pane.on_open(move || {
            if let Some(ui) = weak.upgrade() {
                ui.open_selected();
            }
        });

        let weak = Rc::downgrade(self);
        pane.set_on_notice(move |message| {
            if let Some(ui) = weak.upgrade() {
                ui.toast(message);
            }
        });

        let weak = Rc::downgrade(self);
        pane.on_context(move |entry, x, y| {
            if let Some(ui) = weak.upgrade() {
                ui.show_context_menu(entry, x, y);
            }
        });

        // Drag and drop moves files and folders; a folder is never moved into
        // itself or one of its own children.
        let weak = Rc::downgrade(self);
        pane.set_on_drop(move |sources, target| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            // Never move a folder into itself or a descendant, and never into
            // the folder it already lives in.
            let sources: Vec<PathBuf> = sources
                .into_iter()
                .filter(|src| src != &target && !target.starts_with(src))
                .filter(|src| src.parent() != Some(target.as_path()))
                .collect();
            if sources.is_empty() {
                return;
            }
            let conflicts: Vec<PathBuf> = sources
                .iter()
                .filter_map(|src| src.file_name().map(|name| target.join(name)))
                .filter(|candidate| candidate.exists())
                .collect();
            if conflicts.is_empty() {
                ui.start_transfer(sources, target.clone(), true, ConflictChoice::Replace);
                return;
            }
            let first = conflicts[0].clone();
            let count = conflicts.len();
            let weak = Rc::downgrade(&ui);
            dialogs::conflict_prompt(&ui.window, &first, count, move |choice| {
                if let Some(ui) = weak.upgrade() {
                    ui.start_transfer(sources.clone(), target.clone(), true, choice);
                }
            });
        });

    }

    /* ------------------------------ actions ---------------------------- */

    pub fn open_selected(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        if selected.is_empty() {
            return;
        }
        if selected.len() == 1 && selected[0].is_dir {
            let path = selected[0].path.clone();
            pane.navigate(path, true);
            return;
        }
        let paths: Vec<PathBuf> = selected
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.path.clone())
            .collect();
        self.open_files(&paths);
    }

    /// Open files with their handlers and remember them for Recent Files.
    fn open_files(&self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let overrides = self.config.borrow().app_overrides.clone();
        let custom_openers = self.config.borrow().custom_openers.clone();
        let mime_of = |p: &std::path::Path| -> String {
            gio::content_type_guess(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .as_deref(),
                None::<&[u8]>,
            )
            .0
            .to_string()
        };
        if let Err(err) = apps::open_paths(paths, &mime_of, &overrides, &custom_openers) {
            dialogs::error(&self.window, "Cannot open file", &err);
            return;
        }
        {
            let mut config = self.config.borrow_mut();
            for path in paths {
                config.push_recent_file(path);
            }
        }
        self.config.borrow().save();
    }

    /// Recently opened files, with open and reveal actions, in their own tab.
    fn show_recent_files(self: &Rc<Self>) {
        self.open_library(LibraryKind::RecentFiles);
    }

    /// Folders visited recently; each opens in a new tab.
    fn show_recent_locations(self: &Rc<Self>) {
        self.open_library(LibraryKind::RecentLocations);
    }

    /// Mounted network shares and common remote addresses.
    fn show_network(self: &Rc<Self>) {
        self.open_library(LibraryKind::Network);
    }

    fn open_with_dialog(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        let Some(entry) = selected.first() else {
            return;
        };
        if entry.is_dir {
            return;
        }
        let mime = entry.mime.clone();
        let apps_list = apps::apps_for_mime(&mime);
        let current_default = apps::default_for_mime(&mime).and_then(|a| apps::desktop_id(&a));
        let path = entry.path.clone();
        let name = entry.name.clone();
        let mime_for_choice = mime.clone();
        let config = self.config.clone();
        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        dialogs::open_with(
            &self.window,
            &name,
            &mime,
            apps_list,
            current_default,
            move |app, always| {
                if let Err(err) = apps::launch_with(app, &[path.clone()]) {
                    dialogs::error(&window, "Cannot open file", &err);
                    return;
                }
                if always {
                    if let Some(id) = apps::desktop_id(app) {
                        let ext = path
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                            .unwrap_or_default();
                        {
                            let mut cfg = config.borrow_mut();
                            if !ext.is_empty() {
                                cfg.app_overrides.insert(ext, id.clone());
                            }
                            cfg.app_overrides.insert(mime_for_choice.clone(), id);
                            cfg.save();
                        }
                        if let Some(ui) = weak.upgrade() {
                            ui.toast(&format!("Default app for {mime_for_choice} saved"));
                            ui.refresh_chrome();
                        }
                    }
                    if let Err(err) = apps::set_as_default(app, &mime_for_choice) {
                        eprintln!("nook: could not set system default: {err}");
                    }
                }
            },
        );
    }

    fn new_folder(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let dir = pane.path();
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        dialogs::text_prompt(
            &self.window,
            "New folder",
            "Name for the new folder",
            "New Folder",
            "Create",
            move |name| {
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                let target = fsutil::unique_path(&dir, name);
                match std::fs::create_dir(&target) {
                    Ok(()) => {
                        if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                            pane.soft_refresh();
                            ui.toast(&format!(
                                "Created {}",
                                target.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                    }
                    Err(err) => {
                        if let Some(ui) = weak.upgrade() {
                            dialogs::error(
                                &ui.window,
                                "Could not create folder",
                                &format!("{err}\n\n{}", target.display()),
                            );
                        }
                    }
                }
            },
        );
    }

    fn rename_selected(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        if selected.is_empty() {
            return;
        }
        if selected.len() > 1 {
            self.bulk_rename();
            return;
        }
        let entry = selected[0].clone();
        let dir = entry
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        let original = entry.path.clone();
        dialogs::text_prompt(
            &self.window,
            "Rename",
            "New name",
            &entry.name,
            "Rename",
            move |name| {
                let name = name.trim();
                if name.is_empty() || name == original.file_name().unwrap_or_default() {
                    return;
                }
                let target = dir.join(name);
                if target.exists() {
                    if let Some(ui) = weak.upgrade() {
                        dialogs::error(
                            &ui.window,
                            "Cannot rename",
                            &format!("“{name}” already exists in this folder."),
                        );
                    }
                    return;
                }
                match std::fs::rename(&original, &target) {
                    Ok(()) => {
                        if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                            pane.soft_refresh();
                            ui.toast(&format!("Renamed to {name}"));
                        }
                    }
                    Err(err) => {
                        if let Some(ui) = weak.upgrade() {
                            dialogs::error(&ui.window, "Could not rename", &err.to_string());
                        }
                    }
                }
            },
        );
    }

    fn bulk_rename(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        if selected.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        dialogs::bulk_rename(&self.window, &selected, move |pairs| {
            let mut ok = 0;
            let mut errors = Vec::new();
            for (path, new_name) in pairs {
                let target = path
                    .parent()
                    .unwrap_or(std::path::Path::new("/"))
                    .join(&new_name);
                if target.exists() {
                    errors.push(format!("{}: already exists", new_name));
                    continue;
                }
                match std::fs::rename(&path, &target) {
                    Ok(()) => ok += 1,
                    Err(err) => errors.push(format!("{new_name}: {err}")),
                }
            }
            if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                pane.soft_refresh();
                if errors.is_empty() {
                    ui.toast(&format!("Renamed {ok} items"));
                } else {
                    dialogs::error(
                        &ui.window,
                        "Some items were not renamed",
                        &errors.join("\n"),
                    );
                }
            }
        });
    }

    fn copy_selection(self: &Rc<Self>, cut: bool) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let entries = pane.selected_entries();
        if entries.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        let count = paths.len();
        *self.clipboard.borrow_mut() = Some(ClipboardState { paths, cut });
        self.toast(&format!(
            "{} {count} item{}",
            if cut { "Cut" } else { "Copied" },
            if count == 1 { "" } else { "s" }
        ));
    }

    fn paste(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let clip = self.clipboard.borrow().clone();
        let Some(clip) = clip else {
            self.toast("Clipboard is empty");
            return;
        };
        let dest = pane.path();
        let conflicts: Vec<PathBuf> = clip
            .paths
            .iter()
            .filter_map(|s| s.file_name().map(|n| dest.join(n)))
            .filter(|p| p.exists() && p.parent() == Some(dest.as_path()))
            .collect();

        if conflicts.is_empty() {
            self.start_transfer(clip.paths, dest, clip.cut, ConflictChoice::Replace);
        } else {
            let weak = Rc::downgrade(self);
            let paths = clip.paths.clone();
            let dest2 = dest.clone();
            let cut = clip.cut;
            let first = conflicts[0].clone();
            dialogs::conflict_prompt(&self.window, &first, conflicts.len(), move |choice| {
                if let Some(ui) = weak.upgrade() {
                    ui.start_transfer(paths.clone(), dest2.clone(), cut, choice);
                }
            });
        }
    }

    fn start_transfer(
        self: &Rc<Self>,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        cut: bool,
        choice: ConflictChoice,
    ) {
        if sources.is_empty() {
            return;
        }
        let title = if cut { "Moving items" } else { "Copying items" };
        let dialog = ProgressDialog::new(&self.window, title);
        let tx = dialog.tx.clone();
        let cancel = dialog.cancel_flag();
        let total = sources.len().max(1);
        dialog.message(format!(
            "{title} — {total} item{}",
            if total == 1 { "" } else { "s" }
        ));

        let (done_tx, done_rx) = async_channel::bounded::<fsutil::OpReport>(1);
        let dest_clone = dest.clone();
        std::thread::Builder::new()
            .name("nook-transfer".into())
            .spawn(move || {
                let progress = |done: usize, _bytes: u64| {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let _ = tx.send_blocking(ProgressMsg::Fraction(done as f64 / total as f64));
                };
                let report = if cut {
                    fsutil::move_paths(&sources, &dest_clone, &|_| choice.to_fs(), &progress)
                } else {
                    fsutil::copy_paths(&sources, &dest_clone, &|_| choice.to_fs(), &progress)
                };
                let _ = done_tx.send_blocking(report);
            })
            .ok();

        let weak = Rc::downgrade(self);
        let dest_for_recent = dest.clone();
        let cut_copy = cut;
        glib::spawn_future_local(async move {
            if let Ok(report) = done_rx.recv().await {
                dialog.finish();
                if let Some(ui) = weak.upgrade() {
                    let verb = if cut_copy { "Moved" } else { "Copied" };
                    ui.toast(&report.summary(verb));
                    if report.is_ok() && cut_copy {
                        *ui.clipboard.borrow_mut() = None;
                    }
                    if let Some(first_error) = report.errors.first() {
                        dialogs::error(
                            &ui.window,
                            "Some items failed",
                            &format!(
                                "{}\n\n{} more error(s)",
                                first_error.1,
                                report.errors.len().saturating_sub(1)
                            ),
                        );
                    }
                    if let Some(cfg) = Some(ui.config.clone()) {
                        cfg.borrow_mut().push_recent_destination(&dest_for_recent);
                        cfg.borrow().save();
                    }
                    for pane in ui.all_panes() {
                        pane.soft_refresh();
                    }
                }
            }
        });
    }

    fn trash_selection(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let entries = pane.selected_entries();
        if entries.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        let count = paths.len();
        let confirm = self.config.borrow().confirm_delete;
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        let run = move || {
            let report = fsutil::trash_paths(&paths);
            if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                pane.soft_refresh();
                ui.toast(&report.summary("Moved to trash"));
                if let Some((path, err)) = report.errors.first() {
                    dialogs::error(
                        &ui.window,
                        "Could not move to trash",
                        &format!("{}\n\n{err}", path.display()),
                    );
                }
            }
        };
        if confirm {
            let window = self.window.clone();
            let run = Rc::new(run);
            dialogs::confirm(
                &window,
                "Move to trash?",
                &format!(
                    "{count} item{} will be moved to the trash. You can restore them later.",
                    if count == 1 { "" } else { "s" }
                ),
                "Move to trash",
                false,
                move || run(),
            );
        } else {
            run();
        }
    }

    fn delete_permanently(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let entries = pane.selected_entries();
        if entries.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        let count = paths.len();
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        dialogs::confirm(
            &self.window,
            "Delete permanently?",
            &format!(
                "{count} item{} will be deleted immediately. This cannot be undone.",
                if count == 1 { "" } else { "s" }
            ),
            "Delete",
            true,
            move || {
                let mut report = fsutil::OpReport::default();
                for path in &paths {
                    match fsutil::remove_path(path) {
                        Ok(()) => report.done += 1,
                        Err(err) => report.errors.push((path.clone(), err.to_string())),
                    }
                }
                if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                    pane.soft_refresh();
                    ui.toast(&report.summary("Deleted"));
                    if let Some((path, err)) = report.errors.first() {
                        dialogs::error(
                            &ui.window,
                            "Could not delete",
                            &format!("{}\n\n{err}", path.display()),
                        );
                    }
                }
            },
        );
    }

    fn show_properties(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        if let Some(entry) = selected.first() {
            dialogs::properties(&self.window, entry);
        }
    }

    fn show_checksums(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected: Vec<FileEntry> = pane
            .selected_entries()
            .into_iter()
            .filter(|e| !e.is_dir)
            .collect();
        if selected.is_empty() {
            self.toast("Select files to compute checksums");
            return;
        }
        dialogs::checksums(&self.window, &selected);
    }

    fn open_terminal(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let path = pane.path();
        let terminal = self.config.borrow().terminal.clone();
        if let Err(err) = fsutil::spawn_terminal(&terminal, &path, None) {
            dialogs::error(&self.window, "Could not open terminal", &err);
        }
    }

    fn run_in_terminal(self: &Rc<Self>, dir: PathBuf, command: &str) {
        let terminal = self.config.borrow().terminal.clone();
        match fsutil::spawn_terminal(&terminal, &dir, Some(command)) {
            Ok(_) => self.toast(&format!("Started in {}", dialogs::pretty_path(&dir))),
            Err(err) => dialogs::error(&self.window, "Could not start command", &err),
        }
    }

    /// Commands for `dir`: the nearest `fm_run_task.json`, falling back to the
    /// legacy commands that older versions stored in the config file.
    fn run_task_source(
        self: &Rc<Self>,
        dir: &Path,
    ) -> Result<crate::run_tasks::TaskSource, String> {
        let mut source = crate::run_tasks::load_for(dir)?;
        if !source.exists {
            let legacy = self.config.borrow().commands_for(dir);
            if !legacy.is_empty() {
                source.commands = legacy;
            }
        }
        Ok(source)
    }

    /// Pick one of the current folder's run commands and start it.
    fn show_run_commands(self: &Rc<Self>, target: Option<PathBuf>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let dir = target.unwrap_or_else(|| pane.path());
        let source = match self.run_task_source(&dir) {
            Ok(source) => source,
            Err(err) => {
                dialogs::error(&self.window, "Invalid run task file", &err);
                return;
            }
        };
        if source.commands.is_empty() {
            self.manage_run_commands(Some(dir));
            return;
        }
        let weak = Rc::downgrade(self);
        let target = dir.clone();
        let source_file = source.path.clone();
        let commands = source.commands.clone();
        let edit_ui = Rc::downgrade(self);
        let edit_dir = dir.clone();
        let note = if source.exists {
            format!("Commands defined in {}.", source.path.display())
        } else {
            format!(
                "Commands carried over from Nook's older config. Saving creates {}.",
                source.path.display()
            )
        };
        dialogs::run_command_picker(
            &self.window,
            &dir,
            &commands,
            &note,
            move |command| {
                if let Some(ui) = weak.upgrade() {
                    ui.confirm_and_run(target.clone(), command, source_file.clone());
                }
            },
            move || {
                if let Some(ui) = edit_ui.upgrade() {
                    ui.manage_run_commands(Some(edit_dir.clone()));
                }
            },
        );
    }

    /// Commands that can destroy data are confirmed with their exact text first.
    fn confirm_and_run(self: &Rc<Self>, dir: PathBuf, command: String, source: PathBuf) {
        if !crate::run_tasks::is_destructive(&command) {
            self.run_in_terminal(dir, &command);
            return;
        }
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        dialogs::confirm(
            &window,
            "Run a potentially destructive command?",
            &format!(
                "From {}\n\n{}\n\nThis command can delete or overwrite files. \
                 Run it anyway?",
                source.display(),
                command
            ),
            "Run command",
            true,
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.run_in_terminal(dir.clone(), &command);
                }
            },
        );
    }

    /// Add, edit or remove the run commands for one folder.
    fn manage_run_commands(self: &Rc<Self>, target: Option<PathBuf>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        // Explicit targets come from a folder's context menu; the settings
        // action configures the folder that is currently open.
        let dir = target.unwrap_or_else(|| pane.path());
        if !dir.is_dir() {
            self.toast("Run commands apply to folders");
            return;
        }
        let source = match self.run_task_source(&dir) {
            Ok(source) => source,
            Err(err) => {
                // Never write over a file Nook could not parse.
                dialogs::error(&self.window, "Invalid fm_run_task.json", &err);
                return;
            }
        };
        let weak = Rc::downgrade(self);
        let key = dir.to_string_lossy().to_string();
        let file = source.path.clone();
        let file_for_save = file.clone();
        dialogs::run_commands_editor(
            &self.window,
            &dir,
            &source.commands,
            &file,
            move |commands| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match crate::run_tasks::save(&file_for_save, &commands) {
                    Ok(()) => {
                        {
                            let mut cfg = ui.config.borrow_mut();
                            // Migrated to the file: drop the legacy copy.
                            cfg.folder_commands.remove(&key);
                            cfg.save();
                        }
                        ui.chrome_path.borrow_mut().take();
                        ui.refresh_chrome();
                        ui.toast(&format!(
                            "Run commands saved to {}",
                            file_for_save.display()
                        ));
                    }
                    Err(err) => {
                        dialogs::error(&ui.window, "Could not save run commands", &err)
                    }
                }
            },
        );
    }

    /// Configure per-type applications and shell commands used when opening.
    fn configure_default_apps(self: &Rc<Self>) {
        let config = self.config.borrow().clone();
        let weak = Rc::downgrade(self);
        dialogs::default_apps(&self.window, &config, move |overrides, openers| {
            if let Some(ui) = weak.upgrade() {
                {
                    let mut cfg = ui.config.borrow_mut();
                    cfg.app_overrides = overrides;
                    cfg.custom_openers = openers;
                    cfg.save();
                }
                ui.toast("Default applications updated");
            }
        });
    }

    fn open_in_zed(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let path = pane.path();
        let editor = self.config.borrow().editor.clone();
        match std::process::Command::new(&editor).arg(&path).spawn() {
            Ok(_) => {}
            Err(err) => dialogs::error(
                &self.window,
                "Could not open editor",
                &format!("{editor}: {err}\n\nSet a different editor in your config file."),
            ),
        }
    }

    /// Choose what the search field matches, keeping the drop-down in sync.
    fn set_search_mode_ui(self: &Rc<Self>, mode: SearchMode) {
        if let Some(dropdown) = self.search_mode_dropdown.borrow().as_ref() {
            dropdown.set_selected(match mode {
                SearchMode::Name => 0,
                SearchMode::Extension => 1,
            });
        }
        self.search_mode.set(mode);
        if let Some(pane) = self.active_pane() {
            pane.set_search_mode(mode);
        }
    }

    /// Switch the active pane (and the remembered default) between the icon
    /// grid and the detailed list.
    fn set_view(self: &Rc<Self>, mode: ViewMode) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        if pane.view_mode.get() == mode {
            return;
        }
        pane.set_view(mode);
        self.config.borrow_mut().view = mode;
        self.config.borrow().save();
        self.refresh_chrome();
    }

    /// Rebuild the shared folder style from config and rebind every icon.
    /// Toggle preview-based icons per category (images, videos, PDFs).
    fn choose_preview_icons(self: &Rc<Self>) {
        let snapshot = self.config.borrow().clone();
        let weak = Rc::downgrade(self);
        dialogs::preview_icons(&self.window, &snapshot, move |images, videos, pdfs| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            {
                let mut cfg = ui.config.borrow_mut();
                cfg.thumbs_images = images;
                cfg.thumbs_videos = videos;
                cfg.thumbs_pdfs = pdfs;
                cfg.save();
            }
            for pane in ui.all_panes() {
                pane.rebind_all();
            }
            let on = [images, videos, pdfs].iter().filter(|on| **on).count();
            ui.toast(&format!(
                "Preview icons on for {on} of 3 categories"
            ));
        });
    }

    fn refresh_folder_icons(self: &Rc<Self>) {
        let (default, overrides, inherit) = {
            let cfg = self.config.borrow();
            (
                cfg.folder_color.as_deref().and_then(icons::normalize_hex),
                cfg.folder_colors
                    .iter()
                    .filter_map(|(path, color)| {
                        icons::normalize_hex(color).map(|color| (PathBuf::from(path), color))
                    })
                    .collect::<HashMap<PathBuf, String>>(),
                cfg.folder_color_inherit,
            )
        };
        {
            let mut style = self.folder_style.borrow_mut();
            style.default = default;
            style.overrides = overrides;
            style.inherit = inherit;
        }
        // A full reload (rather than a soft refresh) guarantees every visible
        // cell is rebound with the new tint, including when a refresh happened
        // moments ago and the soft-refresh guard is still active.
        for pane in self.all_panes() {
            pane.reload();
        }
    }

    fn choose_folder_color(self: &Rc<Self>, target: Option<PathBuf>) {
        let path = target.or_else(|| {
            self.active_pane().and_then(|pane| {
                pane.selected_entries()
                    .iter()
                    .find(|entry| entry.is_dir)
                    .map(|entry| entry.path.clone())
            })
        });
        let path = path.or_else(|| self.active_pane().map(|pane| pane.path()));
        let Some(path) = path else { return };
        if !path.is_dir() {
            self.toast("Folder colours apply to folders");
            return;
        }
        let (current, has_own, inherited_from) = {
            let style = self.folder_style.borrow();
            (
                style.color_for(&path).map(|color| color.to_string()),
                style.has_override(&path),
                style
                    .inherits_from_parent(&path)
                    .map(|parent| dialogs::pretty_path(parent)),
            )
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let message = if has_own {
            format!(
                "Pick a colour for “{name}”, or go back to the default. Saved in Nook's \
                 config; the icon theme is not modified."
            )
        } else if let Some(parent) = inherited_from {
            format!("“{name}” currently inherits its colour from {parent}.")
        } else {
            format!(
                "Pick a colour for “{name}”. It currently follows the default folder \
                 colour and is saved in Nook's config only."
            )
        };
        let weak = Rc::downgrade(self);
        dialogs::folder_color(
            &self.window,
            "Folder Colour",
            &message,
            current.as_deref(),
            "Use default colour",
            move |choice| {
                if let Some(ui) = weak.upgrade() {
                    let key = path.to_string_lossy().to_string();
                    {
                        let mut cfg = ui.config.borrow_mut();
                        match choice {
                            Some(color) => {
                                cfg.folder_colors.insert(key, color);
                            }
                            None => {
                                cfg.folder_colors.remove(&key);
                            }
                        }
                        cfg.save();
                    }
                    ui.refresh_folder_icons();
                    ui.toast("Folder colour updated");
                }
            },
        );
    }

    /// Pick an installed icon theme for Nook only (or automatic).
    fn choose_icon_theme(self: &Rc<Self>) {
        let current = self.config.borrow().icon_theme.clone();
        let installed = icons::installed_themes();
        let resolved = icons::preferred_theme(current.as_deref(), self.palette.borrow().is_dark());
        let auto_hint = match (&current, resolved) {
            (None, Some(name)) => format!("Currently {name}"),
            _ => "Colloid when installed, otherwise the desktop theme".to_string(),
        };
        let weak = Rc::downgrade(self);
        dialogs::icon_theme_picker(
            &self.window,
            current.as_deref(),
            &installed,
            &auto_hint,
            move |choice| {
                if let Some(ui) = weak.upgrade() {
                    {
                        let mut cfg = ui.config.borrow_mut();
                        cfg.icon_theme = choice.clone();
                        cfg.save();
                    }
                    match icons::preferred_theme(choice.as_deref(), ui.palette.borrow().is_dark()) {
                        Some(name) => {
                            icons::apply_icon_theme(&name);
                            for pane in ui.all_panes() {
                                pane.soft_refresh();
                            }
                            ui.toast(&format!("Icon theme: {name}"));
                        }
                        None => ui.toast("No icon themes found; keeping the current one"),
                    }
                }
            },
        );
    }

    /// Start a background scan for folders that share the current folder's
    /// effective colour, rooted at the repository (or the folder itself).
    fn same_colour_scan(
        self: &Rc<Self>,
    ) -> Option<(async_channel::Receiver<Vec<PathBuf>>, PathBuf, String)> {
        let pane = self.active_pane()?;
        let start = pane.path();
        let style = self.folder_style.borrow().clone();
        let color = match style.color_for(&start) {
            Some(color) => color.to_string(),
            None => {
                self.toast("This folder has no colour yet — set one first");
                return None;
            }
        };
        let root = repository_root(&start);
        let show_hidden = self.config.borrow().show_hidden;
        let (tx, rx) = async_channel::bounded::<Vec<PathBuf>>(1);
        let target = color.clone();
        let scan_root = root.clone();
        std::thread::Builder::new()
            .name("nook-colour-scan".into())
            .spawn(move || {
                let mut list: Vec<PathBuf> =
                    fsutil::folders_recursive(&scan_root, show_hidden, 20000)
                        .into_iter()
                        .filter(|dir| {
                            style
                                .color_for(dir)
                                .map(|c| c.eq_ignore_ascii_case(&target))
                                .unwrap_or(false)
                        })
                        .take(5000)
                        .collect();
                list.sort();
                let _ = tx.send_blocking(list);
            })
            .ok();
        Some((rx, root, color))
    }

    fn show_same_colour(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let current = pane.path();
        let Some((rx, root, color)) = self.same_colour_scan() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(folders) = rx.recv().await else { return };
            let Some(ui) = weak.upgrade() else { return };
            if folders.is_empty() {
                ui.toast("No folders below this root share that colour");
                return;
            }
            let window = ui.window.clone();
            let weak = Rc::downgrade(&ui);
            dialogs::same_colour_folders(&window, &root, &color, &current, &folders, move |path| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        pane.navigate(path, true);
                    }
                }
            });
        });
    }

    /// Jump to the next/previous folder that shares the current colour, walking
    /// the repository recursively and wrapping around.
    fn jump_same_colour(self: &Rc<Self>, forward: bool) {
        let current = match self.active_pane() {
            Some(pane) => pane.path(),
            None => return,
        };
        let Some((rx, _root, _color)) = self.same_colour_scan() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(list) = rx.recv().await else { return };
            let Some(ui) = weak.upgrade() else { return };
            if list.is_empty() {
                ui.toast("No folders below this root share that colour");
                return;
            }
            let index = list.iter().position(|path| path == &current);
            let next = match (index, forward) {
                (Some(index), true) => (index + 1) % list.len(),
                (Some(index), false) => (index + list.len() - 1) % list.len(),
                (None, _) => 0,
            };
            let target = list[next].clone();
            if let Some(pane) = ui.active_pane() {
                pane.navigate(target.clone(), true);
            }
            ui.toast(&format!(
                "Same colour {}/{} · {}",
                next + 1,
                list.len(),
                dialogs::pretty_path(&target)
            ));
        });
    }

    fn choose_default_folder_color(self: &Rc<Self>) {
        let current = self.config.borrow().folder_color.clone();
        let weak = Rc::downgrade(self);
        dialogs::folder_color(
            &self.window,
            "Default Folder Colour",
            "Colour used for folders that do not have a colour of their own.",
            current.as_deref(),
            "Theme icon",
            move |choice| {
                if let Some(ui) = weak.upgrade() {
                    {
                        let mut cfg = ui.config.borrow_mut();
                        cfg.folder_color = choice;
                        cfg.save();
                    }
                    ui.refresh_folder_icons();
                    ui.toast("Default folder colour updated");
                }
            },
        );
    }

    /// Move every plain file in the current folder into an extension-named
    /// sub-folder. Runs in the background and leaves conflicts untouched.
    fn organize_by_extension(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let dir = pane.path();
        let groups = fsutil::extension_groups(&dir);
        let files: usize = groups.iter().map(|(_, files)| files.len()).sum();
        let folders = groups.iter().filter(|(_, files)| !files.is_empty()).count();
        if files == 0 {
            self.toast("No files to organise in this folder");
            return;
        }
        let window = self.window.clone();
        let weak = Rc::downgrade(self);
        dialogs::confirm(
            &window,
            "Organise by extension",
            &format!(
                "Move {files} file{} into {folders} sub-folder{} named after their \
                 extensions? Sub-folders and hidden files are left alone, and existing \
                 files are never overwritten.",
                if files == 1 { "" } else { "s" },
                if folders == 1 { "" } else { "s" },
            ),
            "Organise",
            false,
            move || {
                let Some(ui) = weak.upgrade() else { return };
                let work_dir = dir.clone();
                let (tx, rx) = async_channel::bounded::<fsutil::OrganizeReport>(1);
                std::thread::Builder::new()
                    .name("nook-organize".into())
                    .spawn(move || {
                        let _ = tx.send_blocking(fsutil::organize_by_extension(&work_dir));
                    })
                    .ok();
                let weak = Rc::downgrade(&ui);
                glib::spawn_future_local(async move {
                    let Ok(report) = rx.recv().await else { return };
                    let Some(ui) = weak.upgrade() else { return };
                    for pane in ui.all_panes() {
                        pane.soft_refresh();
                    }
                    ui.toast(&report.summary());
                    if let Some((path, err)) = report.errors.first() {
                        dialogs::error(
                            &ui.window,
                            "Some files could not be moved",
                            &format!("{}\n\n{err}", path.display()),
                        );
                    }
                });
            },
        );
    }

    fn compress(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected: Vec<PathBuf> = pane
            .selected_entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        if selected.is_empty() {
            return;
        }
        let dir = pane.path();
        let default_name = if selected.len() == 1 {
            format!(
                "{}.tar.gz",
                selected[0]
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "archive".into())
            )
        } else {
            format!(
                "{}.tar.gz",
                dir.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "archive".into())
            )
        };
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        dialogs::text_prompt(
            &self.window,
            "Compress",
            "Archive name (relative to this folder)",
            &default_name,
            "Create",
            move |name| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let target = dir.join(&name);
                let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) else {
                    return;
                };
                if target.exists() {
                    dialogs::error(
                        &ui.window,
                        "Cannot create archive",
                        &format!("“{name}” already exists in this folder."),
                    );
                    return;
                }
                ui.toast(&format!("Compressing {name}…"));
                // Compressing can take a while, so run it off the main thread
                // and report back through a channel. The archive format is
                // chosen from the name the user typed.
                let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
                let sources = selected.clone();
                let work_dir = dir.clone();
                let archive = target.clone();
                let lower = name.to_lowercase();
                std::thread::Builder::new()
                    .name("nook-archive".into())
                    .spawn(move || {
                        let names: Vec<String> = sources
                            .iter()
                            .filter_map(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().to_string())
                            })
                            .collect();
                        let mut command = if lower.ends_with(".zip") {
                            if !fsutil::which("zip") {
                                let _ = tx.send_blocking(Err("zip is not installed".into()));
                                return;
                            }
                            let mut command = std::process::Command::new("zip");
                            command.arg("-r").arg("-q").arg(&archive).args(&names);
                            command
                        } else if lower.ends_with(".7z") {
                            if !fsutil::which("7z") {
                                let _ = tx.send_blocking(Err("7z is not installed".into()));
                                return;
                            }
                            let mut command = std::process::Command::new("7z");
                            command.arg("a").arg("-y").arg(&archive).args(&names);
                            command
                        } else {
                            if !fsutil::which("tar") {
                                let _ = tx.send_blocking(Err("tar is not installed".into()));
                                return;
                            }
                            let mut command = std::process::Command::new("tar");
                            command.arg("-czf").arg(&archive).args(&names);
                            command
                        };
                        let tool = command
                            .get_program()
                            .to_string_lossy()
                            .to_string();
                        let result = command
                            .current_dir(&work_dir)
                            .status()
                            .map_err(|e| format!("{tool}: {e}"))
                            .and_then(|status| {
                                if status.success() {
                                    Ok(())
                                } else {
                                    Err(format!("{tool} exited with status {status}"))
                                }
                            });
                        let _ = tx.send_blocking(result);
                    })
                    .ok();
                let ui_weak = Rc::downgrade(&ui);
                let pane_weak = Rc::downgrade(&pane);
                glib::spawn_future_local(async move {
                    let Ok(result) = rx.recv().await else { return };
                    let (Some(ui), Some(pane)) = (ui_weak.upgrade(), pane_weak.upgrade()) else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            pane.reload();
                            ui.toast(&format!("Created {name}"));
                        }
                        Err(err) => dialogs::error(&ui.window, "Compression failed", &err),
                    }
                });
            },
        );
    }

    fn extract(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected: Vec<FileEntry> = pane
            .selected_entries()
            .into_iter()
            .filter(|e| fsutil::is_archive(&e.name))
            .collect();
        let Some(entry) = selected.first() else {
            self.toast("Select an archive to extract");
            return;
        };
        let path = entry.path.clone();
        let dir = pane.path();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "extracted".into());
        let dest = fsutil::unique_path(&dir, &stem);
        if let Err(err) = std::fs::create_dir_all(&dest) {
            dialogs::error(&self.window, "Extraction failed", &err.to_string());
            return;
        }
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        let ext = entry.extension();
        let source = path.clone();
        let dest_work = dest.clone();
        self.toast(&format!("Extracting {}", entry.name));
        std::thread::Builder::new()
            .name("nook-archive".into())
            .spawn(move || {
                let mut command = if ext == "zip" && fsutil::which("unzip") {
                    let mut c = std::process::Command::new("unzip");
                    c.arg("-q").arg(&source).arg("-d").arg(&dest_work);
                    c
                } else if fsutil::which("7z") {
                    let mut c = std::process::Command::new("7z");
                    c.arg("x")
                        .arg("-y")
                        .arg(format!("-o{}", dest_work.display()))
                        .arg(&source);
                    c
                } else {
                    let mut c = std::process::Command::new("tar");
                    c.arg("-xf").arg(&source).arg("-C").arg(&dest_work);
                    c
                };
                let tool = command.get_program().to_string_lossy().to_string();
                let result = command
                    .status()
                    .map_err(|e| format!("{tool}: {e}"))
                    .and_then(|status| {
                        if status.success() {
                            Ok(())
                        } else {
                            Err(format!("{tool} exited with status {status}"))
                        }
                    });
                let _ = tx.send_blocking(result);
            })
            .ok();

        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        glib::spawn_future_local(async move {
            let Ok(result) = rx.recv().await else { return };
            let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) else {
                return;
            };
            match result {
                Ok(()) => {
                    pane.soft_refresh();
                    ui.toast(&format!("Extracted to {}", dest.display()));
                }
                Err(err) => dialogs::error(&ui.window, "Extraction failed", &err),
            }
        });
    }

    fn create_symlink(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_entries();
        if selected.is_empty() {
            return;
        }
        let dir = pane.path();
        let target = selected[0].path.clone();
        let default = format!(
            "{} (link)",
            target.file_name().unwrap_or_default().to_string_lossy()
        );
        let weak = Rc::downgrade(self);
        let pane_weak = Rc::downgrade(&pane);
        dialogs::text_prompt(
            &self.window,
            "Create link",
            &format!("Link name for {}", target.display()),
            &default,
            "Create",
            move |name| {
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                let link = dir.join(name);
                match std::os::unix::fs::symlink(&target, &link) {
                    Ok(()) => {
                        if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) {
                            pane.soft_refresh();
                            ui.toast(&format!("Linked {name}"));
                        }
                    }
                    Err(err) => {
                        if let Some(ui) = weak.upgrade() {
                            dialogs::error(&ui.window, "Could not create link", &err.to_string());
                        }
                    }
                }
            },
        );
    }

    fn toggle_bookmark(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let path = pane.path();
        let path_string = path.to_string_lossy().to_string();
        let mut cfg = self.config.borrow_mut();
        let existing = cfg.bookmarks.iter().position(|b| b.path == path_string);
        if let Some(idx) = existing {
            cfg.bookmarks.remove(idx);
            cfg.save();
            drop(cfg);
            self.toast("Bookmark removed");
        } else {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".into());
            cfg.bookmarks.push(settings::Bookmark {
                name,
                path: path_string,
            });
            cfg.save();
            drop(cfg);
            self.toast("Bookmark added");
        }
        // Bookmarks are part of the sidebar, so force it to be rebuilt.
        self.chrome_path.borrow_mut().take();
        self.refresh_chrome();
    }

    fn toggle_sort(self: &Rc<Self>, key: SortKey) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let mut cfg = self.config.borrow_mut();
        let desc = if cfg.sort == key {
            !cfg.sort_desc
        } else {
            false
        };
        cfg.sort = key;
        cfg.sort_desc = desc;
        cfg.save();
        drop(cfg);
        pane.set_sort(key, desc);
    }

    fn show_trash(self: &Rc<Self>) {
        self.open_library(LibraryKind::Trash);
    }

    fn quick_shift(self: &Rc<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected: Vec<PathBuf> = pane
            .selected_entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        if selected.is_empty() {
            self.toast("Select files to move");
            return;
        }
        let weak = Rc::downgrade(self);
        let on_done: Rc<dyn Fn(String)> = Rc::new(move |summary: String| {
            if let Some(ui) = weak.upgrade() {
                ui.toast(&summary);
                for pane in ui.all_panes() {
                    pane.soft_refresh();
                }
            }
        });
        quickshift::show(&self.window, self.config.clone(), selected, on_done);
    }

    fn search_changed(self: &Rc<Self>, text: &str) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let mode = self.search_mode.get();
        pane.set_search_mode(mode);
        if self.search_recursive.is_active() && text.trim().len() >= 2 {
            // Cancel any in-flight recursive search.
            if let Some(flag) = self.search_cancel.borrow_mut().take() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            *self.search_cancel.borrow_mut() = Some(flag.clone());
            let root = pane.path();
            let query = text.trim().to_string();
            let query_for_thread = query.clone();
            let show_hidden = self.config.borrow().show_hidden;
            let (tx, rx) = async_channel::bounded::<Vec<FileEntry>>(1);
            std::thread::spawn(move || {
                let results = crate::search::search_recursive(
                    &root,
                    &query_for_thread,
                    mode,
                    show_hidden,
                    500,
                    &flag,
                );
                let _ = tx.send_blocking(results);
            });
            let weak = Rc::downgrade(self);
            let pane_weak = Rc::downgrade(&pane);
            let query_clone = query.clone();
            glib::spawn_future_local(async move {
                if let Ok(results) = rx.recv().await {
                    let (Some(ui), Some(pane)) = (weak.upgrade(), pane_weak.upgrade()) else {
                        return;
                    };
                    if pane.search_query() != query_clone {
                        return;
                    }
                    let count = results.len();
                    pane.set_entries(results);
                    ui.toast(&format!("{count} matches in subfolders"));
                    ui.refresh_chrome();
                }
            });
        } else {
            if let Some(flag) = self.search_cancel.borrow_mut().take() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            pane.set_search(text);
        }
    }

    fn show_context_menu(self: &Rc<Self>, entry: &FileEntry, x: f64, y: f64) {
        *self.context_target.borrow_mut() = Some(entry.clone());
        let menu = gio::Menu::new();
        if entry.is_dir {
            menu.append(Some("Open"), Some("win.open"));
            menu.append(Some("Open in new tab"), Some("win.open-new-tab"));
            menu.append(Some("Folder Colour…"), Some("win.folder-color"));
            menu.append(Some("Run Commands…"), Some("win.run-commands"));
            menu.append(
                Some("Configure Run Commands…"),
                Some("win.folder-commands"),
            );
            menu.append(Some("Git Repository…"), Some("win.git"));
            menu.append(Some("Compress Folder…"), Some("win.compress"));
        } else {
            menu.append(Some("Open"), Some("win.open"));
            menu.append(Some("Open With…"), Some("win.open-with"));
            menu.append(Some("Compress…"), Some("win.compress"));
            if fsutil::is_archive(&entry.name) {
                menu.append(Some("Extract Here"), Some("win.extract"));
            }
        }
        menu.append(Some("Open Terminal Here"), Some("win.open-terminal"));
        menu.append(Some("Open in Zed"), Some("win.open-zed"));
        let edit = gio::Menu::new();
        edit.append(Some("Rename…"), Some("win.rename"));
        edit.append(Some("Bulk Rename…"), Some("win.bulk-rename"));
        edit.append(Some("Copy"), Some("win.copy"));
        edit.append(Some("Cut"), Some("win.cut"));
        edit.append(Some("Create Link…"), Some("win.symlink"));
        menu.append_section(None, &edit);

        let select = gio::Menu::new();
        select.append(Some("Select All"), Some("win.select-all"));
        select.append(Some("Deselect All"), Some("win.select-none"));
        select.append(Some("Invert Selection"), Some("win.invert-selection"));
        menu.append_section(Some("Selection"), &select);
        let extras = gio::Menu::new();
        extras.append(Some("Copy Path"), Some("win.copy-path"));
        extras.append(Some("Checksums…"), Some("win.checksum"));
        extras.append(Some("Properties"), Some("win.properties"));
        menu.append_section(None, &extras);
        let danger = gio::Menu::new();
        danger.append(Some("Move to Trash"), Some("win.trash"));
        danger.append(Some("Delete Permanently"), Some("win.delete"));
        menu.append_section(None, &danger);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.add_css_class("nook-popover");
        popover.set_has_arrow(false);
        popover.set_parent(&self.window);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
        let popover_clone = popover.clone();
        popover.connect_closed(move |_| {
            popover_clone.unparent();
        });
    }

    fn build_header(self: &Rc<Self>) -> gtk::HeaderBar {
        let hb = gtk::HeaderBar::new();
        hb.add_css_class("nook-header");
        // The window buttons live at the end of the tab row instead of in the
        // header bar's own boxes: a header bar balances its title widget
        // against the button box, which would indent the whole top bar.
        hb.set_show_title_buttons(false);

        // The header is two rows: tabs on top, navigation and actions below.
        let chrome = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chrome.set_hexpand(true);
        chrome.add_css_class("nook-topbar");

        let tab_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab_row.add_css_class("nook-tabbar");
        let new_tab = icon_button("list-add-symbolic", "New tab (Ctrl+T)");
        new_tab.add_css_class("nook-tab-add");
        let window_controls = gtk::WindowControls::new(gtk::PackType::End);
        window_controls.add_css_class("nook-window-controls");
        tab_row.append(&self.tab_scroller);
        tab_row.append(&new_tab);
        tab_row.append(&window_controls);
        chrome.append(&tab_row);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_hexpand(true);
        toolbar.add_css_class("nook-toolbar");

        let nav_back = icon_button("go-previous-symbolic", "Back (Alt+Left)");
        let nav_fwd = icon_button("go-next-symbolic", "Forward (Alt+Right)");
        nav_back.set_sensitive(false);
        nav_fwd.set_sensitive(false);
        *self.nav_back.borrow_mut() = Some(nav_back.clone());
        *self.nav_fwd.borrow_mut() = Some(nav_fwd.clone());
        let nav_up = icon_button("go-up-symbolic", "Parent folder (Alt+Up)");

        self.crumb_box.set_hexpand(true);
        self.crumb_box.set_halign(gtk::Align::Start);
        self.crumb_box.add_css_class("nook-crumb-bar");
        let crumb_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&self.crumb_box)
            .build();
        // The breadcrumb width must not become the toolbar's minimum: with a
        // deep path that alone made the window impossible to shrink.

        // The breadcrumb display and the editable path field share one slot,
        // so switching between them keeps the toolbar layout stable.
        self.crumb_stack.set_hexpand(true);
        self.crumb_stack.set_transition_type(gtk::StackTransitionType::None);
        self.crumb_stack.add_named(&crumb_scroller, Some("crumbs"));
        self.crumb_stack.add_named(&self.path_entry, Some("edit"));
        self.crumb_stack.set_visible_child_name("crumbs");
        let edit_path = icon_button("document-edit-symbolic", "Edit location (Ctrl+L)");

        let search_button = icon_button("system-search-symbolic", "Search (Ctrl+F)");

        let view_button = gtk::Button::new();
        view_button.add_css_class("nook-btn");
        let view_icon = gtk::Image::from_icon_name("view-grid-symbolic");
        view_icon.set_pixel_size(20);
        view_button.set_child(Some(&view_icon));
        view_button.set_tooltip_text(Some("Switch to detailed view (Ctrl+1)"));

        let hidden_button = gtk::ToggleButton::new();
        hidden_button.add_css_class("nook-btn");
        let hidden_icon = gtk::Image::from_icon_name("view-conceal-symbolic");
        hidden_icon.set_pixel_size(20);
        hidden_button.set_child(Some(&hidden_icon));
        hidden_button.set_tooltip_text(Some("Show hidden files (Ctrl+H)"));

        let preview_button = gtk::ToggleButton::new();
        preview_button.add_css_class("nook-btn");
        let preview_icon = gtk::Image::from_icon_name("sidebar-show-symbolic");
        preview_icon.set_pixel_size(20);
        preview_button.set_child(Some(&preview_icon));
        preview_button.set_tooltip_text(Some("Preview pane (Ctrl+Shift+P)"));

        let terminal_button = icon_button("utilities-terminal-symbolic", "Open terminal here (Ctrl+Alt+T)");

        let send_button = icon_button(
            "send-to-symbolic",
            "Send to a device with LocalSend (Ctrl+Shift+S)",
        );

        let run_button = icon_button("media-playback-start-symbolic", "Run a folder command");
        run_button.set_visible(false);

        let git_button = icon_button("builder-vcs-git-symbolic", "Git repository dashboard");
        let settings_button = gtk::MenuButton::new();
        settings_button.set_icon_name("emblem-system-symbolic");
        settings_button.add_css_class("nook-btn");
        settings_button.set_tooltip_text(Some("Settings"));
        settings_button.set_menu_model(Some(&self.build_settings_menu()));

        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.add_css_class("nook-btn");
        menu_button.set_tooltip_text(Some("Main menu"));
        menu_button.set_menu_model(Some(&self.build_menu()));

        toolbar.append(&nav_back);
        toolbar.append(&nav_fwd);
        toolbar.append(&nav_up);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&self.crumb_stack);
        toolbar.append(&edit_path);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&search_button);
        toolbar.append(&view_button);
        toolbar.append(&hidden_button);
        toolbar.append(&preview_button);
        toolbar.append(&send_button);
        toolbar.append(&terminal_button);
        toolbar.append(&run_button);
        toolbar.append(&git_button);
        toolbar.append(&settings_button);
        toolbar.append(&menu_button);
        // As a last resort the toolbar scrolls sideways instead of being
        // clipped when the window is narrower than the buttons it holds.
        let toolbar_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .hexpand(true)
            .child(&toolbar)
            .build();
        toolbar_scroller.add_css_class("nook-toolbar-scroll");
        chrome.append(&toolbar_scroller);

        hb.set_title_widget(Some(&chrome));

        // Wire buttons.
        {
            let weak = Rc::downgrade(self);
            new_tab.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let path = ui
                        .active_pane()
                        .map(|pane| pane.path())
                        .unwrap_or_else(settings::home_dir);
                    let view = ui
                        .active_pane()
                        .map(|pane| pane.view_mode.get())
                        .unwrap_or(ui.config.borrow().view);
                    ui.add_tab(path, view, true);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            nav_back.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        pane.go_back();
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            nav_fwd.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        pane.go_forward();
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            nav_up.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        pane.go_up();
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            edit_path.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.start_path_edit();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            search_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let visible = !ui.search_revealer.reveals_child();
                    ui.search_revealer.set_reveal_child(visible);
                    if visible {
                        ui.search_entry.grab_focus();
                    } else {
                        ui.search_entry.set_text("");
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            view_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(pane) = ui.active_pane() {
                        ui.set_view(pane.view_mode.get().toggled());
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let icon = hidden_icon.clone();
            hidden_button.connect_toggled(move |b| {
                if let Some(ui) = weak.upgrade() {
                    let show = b.is_active();
                    if ui.active_pane().map(|p| p.show_hidden.get()) == Some(show) {
                        return;
                    }
                    ui.config.borrow_mut().show_hidden = show;
                    ui.config.borrow().save();
                    icon.set_icon_name(Some(if show {
                        "view-reveal-symbolic"
                    } else {
                        "view-conceal-symbolic"
                    }));
                    for pane in ui.all_panes() {
                        pane.set_show_hidden(show);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            preview_button.connect_toggled(move |b| {
                if let Some(ui) = weak.upgrade() {
                    let show = b.is_active();
                    if ui.preview_revealer.reveals_child() == show {
                        return;
                    }
                    ui.preview_revealer.set_reveal_child(show);
                    ui.config.borrow_mut().show_preview = show;
                    ui.config.borrow().save();
                    if show {
                        ui.update_selection_preview();
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            send_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send_selection();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            terminal_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_terminal();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            run_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.show_run_commands(None);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            git_button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let Some(pane) = ui.active_pane() else { return };
                    git_pane::show(&ui.window, &pane.path());
                }
            });
        }

        *self.folder_controls.borrow_mut() = vec![
            search_button.clone().upcast(),
            view_button.clone().upcast(),
            hidden_button.clone().upcast(),
            send_button.clone().upcast(),
            terminal_button.clone().upcast(),
            run_button.clone().upcast(),
            git_button.clone().upcast(),
        ];

        *self.view_button.borrow_mut() = Some(view_button);
        *self.view_icon.borrow_mut() = view_icon;
        *self.hidden_button.borrow_mut() = hidden_button;
        *self.hidden_icon.borrow_mut() = hidden_icon;
        *self.preview_button.borrow_mut() = preview_button;
        *self.run_button.borrow_mut() = Some(run_button);
        *self.menu_button.borrow_mut() = Some(menu_button);
        *self.settings_button.borrow_mut() = Some(settings_button);

        hb
    }

    fn build_settings_menu(self: &Rc<Self>) -> gio::Menu {
        let menu = gio::Menu::new();

        let appearance = gio::Menu::new();
        appearance.append(Some("Keyboard Shortcuts…"), Some("win.shortcuts"));
        appearance.append(Some("Icon Theme…"), Some("win.icon-theme"));
        appearance.append(Some("Preview Icons…"), Some("win.preview-icons"));
        appearance.append(
            Some("Default Folder Colour…"),
            Some("win.default-folder-color"),
        );
        appearance.append(
            Some("Subfolders Inherit Colour"),
            Some("win.inherit-folder-colors"),
        );
        menu.append_submenu(Some("Appearance"), &appearance);

        let behaviour = gio::Menu::new();
        behaviour.append(
            Some("Default Applications & Commands…"),
            Some("win.default-apps"),
        );
        behaviour.append(Some("Folder Run Commands…"), Some("win.folder-commands"));
        behaviour.append(Some("LocalSend Transfers…"), Some("win.transfer-settings"));
        menu.append_submenu(Some("Behaviour"), &behaviour);

        let privacy = gio::Menu::new();
        privacy.append(Some("Git Repository…"), Some("win.git"));
        privacy.append(Some("Trash"), Some("win.show-trash"));
        menu.append_submenu(Some("Repository"), &privacy);

        let about = gio::Menu::new();
        about.append(Some("About Nook"), Some("win.about"));
        menu.append_submenu(Some("About"), &about);
        menu
    }

    /// The hamburger menu. Every section is a submenu: the popover stays a
    /// compact list of eight headings instead of one long scroll, and each
    /// heading drops down its own items (arrow right, or hover to open).
    fn build_menu(self: &Rc<Self>) -> gio::Menu {
        let menu = gio::Menu::new();

        let tabs = gio::Menu::new();
        tabs.append(Some("New Tab"), Some("win.new-tab"));
        tabs.append(Some("Duplicate Tab"), Some("win.duplicate-tab"));
        tabs.append(Some("Next Tab"), Some("win.next-tab"));
        tabs.append(Some("Previous Tab"), Some("win.prev-tab"));
        tabs.append(Some("Close Tab"), Some("win.close-tab"));
        menu.append_submenu(Some("Tabs"), &tabs);

        let files = gio::Menu::new();
        files.append(Some("New Folder…"), Some("win.new-folder"));
        files.append(Some("Compress Selection…"), Some("win.compress"));
        files.append(Some("Extract Here"), Some("win.extract"));
        files.append(Some("Create Link…"), Some("win.symlink"));
        files.append(Some("Quick Shift…"), Some("win.quick-shift"));
        menu.append_submenu(Some("Files"), &files);

        let edit = gio::Menu::new();
        edit.append(Some("Copy"), Some("win.copy"));
        edit.append(Some("Cut"), Some("win.cut"));
        edit.append(Some("Paste"), Some("win.paste"));
        edit.append(Some("Rename…"), Some("win.rename"));
        edit.append(Some("Bulk Rename…"), Some("win.bulk-rename"));
        edit.append(Some("Move to Trash"), Some("win.trash"));
        edit.append(Some("Delete Permanently"), Some("win.delete"));
        menu.append_submenu(Some("Edit"), &edit);

        let selection = gio::Menu::new();
        selection.append(Some("Select All"), Some("win.select-all"));
        selection.append(Some("Deselect All"), Some("win.select-none"));
        selection.append(Some("Invert Selection"), Some("win.invert-selection"));
        menu.append_submenu(Some("Selection"), &selection);

        let view = gio::Menu::new();
        view.append(Some("Icon View"), Some("win.view-icons"));
        view.append(Some("Detailed View"), Some("win.view-details"));
        view.append(
            Some("Switch view: Icons → Details → Recursive"),
            Some("win.toggle-view"),
        );
        view.append(Some("Focus File List"), Some("win.focus-files"));
        view.append(Some("Focus Sidebar"), Some("win.focus-sidebar"));
        view.append(Some("Show Hidden Files"), Some("win.toggle-hidden"));
        view.append(Some("Toggle Preview Pane"), Some("win.toggle-preview"));
        view.append(Some("Minimise Sidebar"), Some("win.toggle-sidebar"));
        let sort = gio::Menu::new();
        sort.append(Some("Name"), Some("win.sort-name"));
        sort.append(Some("Size"), Some("win.sort-size"));
        sort.append(Some("Modified"), Some("win.sort-modified"));
        sort.append(Some("Kind"), Some("win.sort-kind"));
        view.append_submenu(Some("Sort by"), &sort);
        let search = gio::Menu::new();
        search.append(Some("By Name"), Some("win.search-by-name"));
        search.append(Some("By Extension"), Some("win.search-by-extension"));
        view.append_submenu(Some("Search Mode"), &search);
        menu.append_submenu(Some("View"), &view);

        let go = gio::Menu::new();
        go.append(Some("Home Folder"), Some("win.home"));
        go.append(Some("Parent Folder"), Some("win.up"));
        go.append(Some("Back"), Some("win.back"));
        go.append(Some("Forward"), Some("win.forward"));
        go.append(Some("Edit Location…"), Some("win.go-to"));
        go.append(Some("Open Terminal Here"), Some("win.open-terminal"));
        go.append(Some("Open Folder in Zed"), Some("win.open-zed"));
        go.append(Some("Bookmark This Folder"), Some("win.bookmark"));
        go.append(Some("Copy Path"), Some("win.copy-path"));
        menu.append_submenu(Some("Go"), &go);

        let places = gio::Menu::new();
        places.append(Some("Trash"), Some("win.show-trash"));
        places.append(Some("Recent Files"), Some("win.show-recent-files"));
        places.append(Some("Recent Locations"), Some("win.show-recent-locations"));
        places.append(Some("Network"), Some("win.show-network"));
        places.append(Some("Git Repository…"), Some("win.git"));
        menu.append_submenu(Some("Places"), &places);

        let share = gio::Menu::new();
        share.append(Some("Send to Device…"), Some("win.send"));
        share.append(Some("LocalSend Transfers…"), Some("win.transfer-settings"));
        share.append(Some("Open Received Folder"), Some("win.open-received"));
        menu.append_submenu(Some("Share"), &share);

        let tools = gio::Menu::new();
        tools.append(Some("Run Command Here…"), Some("win.run-commands"));
        tools.append(Some("Organise by Extension…"), Some("win.organize"));
        let colours = gio::Menu::new();
        colours.append(
            Some("Default Folder Colour…"),
            Some("win.default-folder-color"),
        );
        colours.append(
            Some("Subfolders Inherit Colour"),
            Some("win.inherit-folder-colors"),
        );
        colours.append(Some("Folders With This Colour…"), Some("win.same-colour"));
        colours.append(
            Some("Next Same-colour Folder"),
            Some("win.same-colour-next"),
        );
        colours.append(
            Some("Previous Same-colour Folder"),
            Some("win.same-colour-prev"),
        );
        tools.append_submenu(Some("Folder Colours"), &colours);
        tools.append(Some("Icon Theme…"), Some("win.icon-theme"));
        tools.append(Some("Preview Icons…"), Some("win.preview-icons"));
        menu.append_submenu(Some("Tools"), &tools);

        let help = gio::Menu::new();
        help.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        help.append(Some("Keyboard-only Guide"), Some("win.keyboard-guide"));
        help.append(Some("About Nook"), Some("win.about"));
        menu.append_submenu(Some("Help"), &help);
        menu
    }

    fn show_shortcuts(self: &Rc<Self>) {
        let actions = shortcut_actions();
        let config = self.config.borrow().clone();
        let weak = Rc::downgrade(self);
        let app = self.window.application();
        dialogs::shortcuts(&self.window, &actions, &config, move |pairs| {
            if let Some(ui) = weak.upgrade() {
                {
                    let mut cfg = ui.config.borrow_mut();
                    cfg.shortcuts.clear();
                    for (action, accel) in pairs {
                        let accel = accel.trim().to_string();
                        if !accel.is_empty() {
                            cfg.shortcuts.insert(action, accel);
                        }
                    }
                    cfg.save();
                }
                if let Some(app) = app.as_ref() {
                    apply_shortcuts(app, &ui.config.borrow());
                }
                ui.toast("Shortcuts updated");
            }
        });
    }

    /// A short keyboard-only walkthrough, reachable from Help.
    fn show_keyboard_guide(self: &Rc<Self>) {
        let win = dialogs::base_window(&self.window, "Keyboard-only guide");
        win.set_default_size(620, 640);
        win.set_resizable(true);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(18);
        root.set_margin_bottom(14);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.append(&dialogs::title_label("Using Nook without a mouse"));
        root.append(&dialogs::body_label(
            "Every window, menu and list is reachable from the keyboard. F10 opens the \
             menu bar; Tab and the arrow keys move; Enter activates; Escape backs out.",
        ));
        let text = gtk::Label::new(Some(KEYBOARD_GUIDE));
        text.set_xalign(0.0);
        text.set_wrap(true);
        text.set_selectable(true);
        text.set_can_focus(false);
        text.add_css_class("nook-meta");
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&text)
            .build();
        root.append(&scroller);
        let close = gtk::Button::with_label("Close");
        close.add_css_class("nook-suggested");
        close.set_halign(gtk::Align::End);
        root.append(&close);
        win.set_child(Some(&root));
        {
            let win = win.clone();
            close.connect_clicked(move |_| win.close());
        }
        win.set_default_widget(Some(&close));
        win.present();
    }

    fn show_about(self: &Rc<Self>) {
        let about = gtk::AboutDialog::builder()
            .program_name("Nook")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("A native GTK4 file manager for Hyprland and DankMaterialShell.")
            .website("https://github.com/")
            .license_type(gtk::License::MitX11)
            .modal(true)
            .transient_for(&self.window)
            .build();
        about.present();
    }
}

/// Nearest ancestor (including `start`) that looks like a repository root, so
/// recursive colour browsing stays inside the project.
fn repository_root(start: &Path) -> PathBuf {
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// Text of the keyboard-only walkthrough (Help ▸ Keyboard-only Guide).
const KEYBOARD_GUIDE: &str = "\
Windows and tabs
  F10                    open the main menu (arrows walk it, Right opens a submenu)
  Ctrl+,                 open the settings menu
  Ctrl+T / Ctrl+W        new tab / close tab
  Ctrl+Tab, Ctrl+Shift+Tab  next / previous tab

Moving around
  Alt+Left / Alt+Right   back / forward
  Alt+Up                 parent folder
  Alt+Home               home folder
  Ctrl+L                 type a path, Enter to go, Esc to cancel
  Ctrl+Shift+B           focus the sidebar (Up/Down move, Enter opens, Esc returns)
  Ctrl+Shift+F           focus the file list
  Ctrl+F                 search; type to filter, Down jumps into the results
  Ctrl+H                 show hidden files
  Ctrl+1                 cycle icons → details → recursive

Working with files
  Arrow keys, Home/End, Page Up/Down   move in the list
  Ctrl+Space             add to / remove from the selection
  Shift+F10 or Menu key  context menu for the selection (or the folder)
  Enter                  open, or enter a folder
  F2                     rename
  Ctrl+C / Ctrl+X / Ctrl+V  copy / cut / paste
  Delete / Shift+Delete  move to trash / delete permanently
  Ctrl+Shift+M           Quick Shift (type a destination, Enter to move)
  Ctrl+Shift+N           new folder
  Ctrl+Shift+R           bulk rename
  Alt+Return             properties
  Ctrl+Shift+C           copy path
  Ctrl+Shift+O           open with…
  Ctrl+D                 bookmark this folder

Views that are states of a tab
  Trash, Recent Files, Recent Locations and Network open inside the current
  tab. Arrow keys select rows, Enter runs the main action (Restore or Open),
  Delete runs the destructive one, Ctrl+A selects every row and Esc returns
  to the folder. The action bar at the bottom mirrors those keys.

Sharing
  Ctrl+Shift+S           send the selection with LocalSend
  In the device picker: type to filter, Up/Down to choose, Enter to send,
  Ctrl+R rescans, Esc cancels. Incoming transfers ask Accept / Decline and
  default to Accept on Enter.

Every one of these can be rebound in Settings ▸ Appearance ▸ Keyboard Shortcuts.
";

/// Focusable rows of the sidebar, in display order.
fn sidebar_rows(container: &gtk::Box) -> Vec<gtk::Widget> {
    let mut rows = Vec::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        if widget.can_focus() {
            rows.push(widget.clone());
        }
        child = widget.next_sibling();
    }
    rows
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::from_icon_name(icon);
    btn.add_css_class("nook-btn");
    btn.set_tooltip_text(Some(tooltip));
    btn
}

/// All rebindable actions, with the labels shown in the shortcuts dialog.
pub fn shortcut_actions() -> Vec<(&'static str, &'static str)> {
    vec![
        ("win.new-tab", "New tab"),
        ("win.close-tab", "Close tab"),
        ("win.duplicate-tab", "Duplicate tab"),
        ("win.next-tab", "Next tab"),
        ("win.prev-tab", "Previous tab"),
        ("win.open-terminal", "Open terminal here"),
        ("win.open-zed", "Open folder in Zed"),
        ("win.go-to", "Go to folder"),
        ("win.copy-path", "Copy path"),
        ("win.icon-theme", "Icon theme"),
        ("win.same-colour", "Folders with this colour"),
        ("win.same-colour-next", "Next same-colour folder"),
        ("win.same-colour-prev", "Previous same-colour folder"),
        ("win.toggle-preview", "Toggle preview pane"),
        ("win.toggle-sidebar", "Minimise or expand the sidebar"),
        ("win.toggle-hidden", "Show hidden files"),
        ("win.toggle-view", "Cycle icons / details / recursive"),
        ("win.focus-search", "Focus search"),
        ("win.quick-shift", "Quick Shift"),
        ("win.new-folder", "New folder"),
        ("win.rename", "Rename"),
        ("win.bulk-rename", "Bulk rename"),
        ("win.copy", "Copy"),
        ("win.cut", "Cut"),
        ("win.paste", "Paste"),
        ("win.trash", "Move to trash"),
        ("win.delete", "Delete permanently"),
        ("win.properties", "Properties"),
        ("win.checksum", "Checksums"),
        ("win.select-all", "Select all"),
        ("win.select-none", "Deselect all"),
        ("win.invert-selection", "Invert selection"),
        ("win.refresh", "Refresh"),
        ("win.up", "Parent folder"),
        ("win.back", "Back"),
        ("win.forward", "Forward"),
        ("win.home", "Home folder"),
        ("win.open-with", "Open with…"),
        ("win.bookmark", "Bookmark current folder"),
        ("win.compress", "Compress selection"),
        ("win.extract", "Extract archive"),
        ("win.folder-color", "Folder colour"),
        ("win.folder-commands", "Configure folder run commands"),
        ("win.run-commands", "Run folder command"),
        ("win.git", "Git repository dashboard"),
        ("win.default-apps", "Default applications"),
        ("win.shortcuts", "Keyboard shortcuts"),
        ("win.send", "Send files to a device"),
        ("win.transfer-settings", "LocalSend settings"),
        ("win.main-menu", "Open the main menu"),
        ("win.settings-menu", "Open the settings menu"),
        ("win.context-menu", "Open the context menu"),
        ("win.focus-sidebar", "Focus the sidebar"),
        ("win.show-trash", "Trash view"),
        ("win.show-recent-files", "Recent files view"),
        ("win.show-recent-locations", "Recent locations view"),
        ("win.show-network", "Network view"),
    ]
}

fn default_accels() -> Vec<(&'static str, &'static str)> {
    vec![
        ("win.new-tab", "<Control>t"),
        ("win.close-tab", "<Control>w"),
        ("win.open-terminal", "<Control><Alt>t"),
        ("win.toggle-preview", "<Control><Shift>p"),
        ("win.toggle-sidebar", "<Control>b"),
        ("win.toggle-hidden", "<Control>h"),
        ("win.toggle-view", "<Control>1"),
        ("win.focus-search", "<Control>f"),
        ("win.quick-shift", "<Control><Shift>m"),
        ("win.new-folder", "<Control><Shift>n"),
        ("win.rename", "F2"),
        ("win.bulk-rename", "<Control><Shift>r"),
        ("win.copy", "<Control>c"),
        ("win.cut", "<Control>x"),
        ("win.paste", "<Control>v"),
        ("win.trash", "Delete"),
        ("win.delete", "<Shift>Delete"),
        ("win.properties", "<Alt>Return"),
        ("win.select-all", "<Control>a"),
        ("win.select-none", "<Control><Shift>a"),
        ("win.invert-selection", "<Control><Shift>i"),
        ("win.refresh", "<Control>r"),
        ("win.up", "<Alt>Up"),
        ("win.back", "<Alt>Left"),
        ("win.forward", "<Alt>Right"),
        ("win.home", "<Alt>Home"),
        ("win.open-with", "<Control><Shift>o"),
        ("win.bookmark", "<Control>d"),
        ("win.shortcuts", "F1"),
        ("win.next-tab", "<Control>Tab"),
        ("win.prev-tab", "<Control><Shift>Tab"),
        ("win.go-to", "<Control>l"),
        ("win.copy-path", "<Control><Shift>c"),
        ("win.same-colour", "<Control><Shift>k"),
        ("win.same-colour-next", "<Alt>Page_Down"),
        ("win.same-colour-prev", "<Alt>Page_Up"),
        ("win.send", "<Control><Shift>s"),
        ("win.main-menu", "F10"),
        ("win.settings-menu", "<Control>comma"),
        ("win.context-menu", "<Shift>F10"),
        ("win.focus-sidebar", "<Control><Shift>b"),
        ("win.focus-files", "<Control><Shift>f"),
    ]
}

pub fn apply_shortcuts(app: &gtk::Application, config: &Config) {
    for (action, _) in shortcut_actions() {
        app.set_accels_for_action(action, &[]);
    }
    for (action, default) in default_accels() {
        let accel = config.shortcut(action, default);
        if accel.is_empty() {
            continue;
        }
        app.set_accels_for_action(action, &[accel.as_str()]);
    }
}

fn add_action<F>(ui: &Rc<AppUi>, name: &str, f: F)
where
    F: Fn(&Rc<AppUi>) + 'static,
{
    let action = gio::SimpleAction::new(name, None);
    let weak = Rc::downgrade(ui);
    action.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            f(&ui);
        }
    });
    ui.window.add_action(&action);
}

fn register_actions(ui: &Rc<AppUi>) {
    add_action(ui, "new-tab", |ui| {
        let home = settings::home_dir();
        let view = ui.config.borrow().view;
        ui.add_tab(home, view, true);
    });
    add_action(ui, "close-tab", |ui| ui.close_active_tab());
    add_action(ui, "duplicate-tab", |ui| ui.duplicate_tab());
    add_action(ui, "next-tab", |ui| {
        let n = ui.notebook.n_pages();
        if n > 1 {
            let cur = ui.notebook.current_page().unwrap_or(0);
            ui.notebook.set_current_page(Some((cur + 1) % n));
        }
    });
    add_action(ui, "prev-tab", |ui| {
        let n = ui.notebook.n_pages();
        if n > 1 {
            let cur = ui.notebook.current_page().unwrap_or(0);
            ui.notebook
                .set_current_page(Some(if cur == 0 { n - 1 } else { cur - 1 }));
        }
    });
    add_action(ui, "open-terminal", |ui| ui.open_terminal());
    add_action(ui, "open-zed", |ui| ui.open_in_zed());
    add_action(ui, "toggle-preview", |ui| {
        let show = !ui.preview_revealer.reveals_child();
        ui.preview_revealer.set_reveal_child(show);
        ui.preview_button.borrow().set_active(show);
        ui.config.borrow_mut().show_preview = show;
        ui.config.borrow().save();
        if show {
            ui.update_selection_preview();
        }
    });
    add_action(ui, "toggle-sidebar", |ui| {
        ui.set_sidebar_mini(!ui.sidebar_mini.get());
    });
    add_action(ui, "refresh", |ui| {
        // Recompute the run-button state from the run task file.
        ui.chrome_path.borrow_mut().take();
        for pane in ui.all_panes() {
            pane.reload();
        }
        if let Some(tab) = ui.active_tab() {
            if let Some(refresh) = tab.library_refresh.borrow().clone() {
                refresh();
            }
        }
        ui.refresh_chrome();
        ui.toast("Refreshed");
    });
    add_action(ui, "toggle-hidden", |ui| {
        let show = !ui
            .active_pane()
            .map(|p| p.show_hidden.get())
            .unwrap_or(false);
        ui.hidden_button.borrow().set_active(show);
    });
    add_action(ui, "toggle-view", |ui| {
        if let Some(pane) = ui.active_pane() {
            ui.set_view(pane.view_mode.get().toggled());
        }
    });
    add_action(ui, "focus-files", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.focus_view();
        }
    });
    add_action(ui, "view-icons", |ui| ui.set_view(ViewMode::Grid));
    add_action(ui, "view-details", |ui| ui.set_view(ViewMode::List));
    add_action(ui, "search-by-name", |ui| {
        ui.set_search_mode_ui(SearchMode::Name)
    });
    add_action(ui, "search-by-extension", |ui| {
        ui.set_search_mode_ui(SearchMode::Extension)
    });
    add_action(ui, "focus-search", |ui| {
        ui.search_revealer.set_reveal_child(true);
        ui.search_entry.grab_focus();
    });
    add_action(ui, "quick-shift", |ui| ui.quick_shift());
    add_action(ui, "new-folder", |ui| ui.new_folder());
    add_action(ui, "rename", |ui| ui.rename_selected());
    add_action(ui, "bulk-rename", |ui| ui.bulk_rename());
    add_action(ui, "copy", |ui| ui.copy_selection(false));
    add_action(ui, "cut", |ui| ui.copy_selection(true));
    add_action(ui, "paste", |ui| ui.paste());
    add_action(ui, "trash", |ui| ui.trash_selection());
    add_action(ui, "delete", |ui| ui.delete_permanently());
    add_action(ui, "properties", |ui| ui.show_properties());
    add_action(ui, "checksum", |ui| ui.show_checksums());
    add_action(ui, "select-none", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.select_none();
            ui.refresh_chrome();
        }
    });
    add_action(ui, "invert-selection", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.invert_selection();
            ui.refresh_chrome();
        }
    });
    add_action(ui, "select-all", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.select_all();
            ui.refresh_chrome();
        }
    });
    add_action(ui, "up", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.go_up();
        }
    });
    add_action(ui, "back", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.go_back();
        }
    });
    add_action(ui, "forward", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.go_forward();
        }
    });
    add_action(ui, "home", |ui| {
        if let Some(pane) = ui.active_pane() {
            pane.navigate(settings::home_dir(), true);
        }
    });
    add_action(ui, "open-with", |ui| ui.open_with_dialog());
    add_action(ui, "open", |ui| ui.open_selected());
    add_action(ui, "bookmark", |ui| ui.toggle_bookmark());
    add_action(ui, "compress", |ui| ui.compress());
    add_action(ui, "extract", |ui| ui.extract());
    add_action(ui, "default-apps", |ui| ui.configure_default_apps());
    add_action(ui, "folder-commands", |ui| {
        let target = ui
            .context_target
            .borrow()
            .as_ref()
            .filter(|entry| entry.is_dir)
            .map(|entry| entry.path.clone());
        ui.manage_run_commands(target);
    });
    add_action(ui, "run-commands", |ui| {
        let target = ui
            .context_target
            .borrow()
            .as_ref()
            .filter(|entry| entry.is_dir)
            .map(|entry| entry.path.clone());
        ui.show_run_commands(target);
    });
    add_action(ui, "git", |ui| {
        if let Some(pane) = ui.active_pane() {
            git_pane::show(&ui.window, &pane.path());
        }
    });
    add_action(ui, "symlink", |ui| ui.create_symlink());
    add_action(ui, "show-trash", |ui| ui.show_trash());
    add_action(ui, "show-recent-files", |ui| ui.show_recent_files());
    add_action(ui, "show-recent-locations", |ui| ui.show_recent_locations());
    add_action(ui, "show-network", |ui| ui.show_network());
    add_action(ui, "send", |ui| ui.send_selection());
    add_action(ui, "transfer-settings", |ui| send::settings_dialog(ui));
    add_action(ui, "open-received", |ui| {
        let dir = ui.receive_dir();
        let view = ui.config.borrow().view;
        ui.add_tab(dir, view, true);
    });
    add_action(ui, "main-menu", |ui| {
        if let Some(button) = ui.menu_button.borrow().as_ref() {
            button.popup();
        }
    });
    add_action(ui, "settings-menu", |ui| {
        if let Some(button) = ui.settings_button.borrow().as_ref() {
            button.popup();
        }
    });
    add_action(ui, "context-menu", |ui| ui.context_menu_keyboard());
    add_action(ui, "focus-sidebar", |ui| ui.focus_sidebar());
    add_action(ui, "keyboard-guide", |ui| ui.show_keyboard_guide());
    add_action(ui, "shortcuts", |ui| ui.show_shortcuts());
    add_action(ui, "about", |ui| ui.show_about());
    add_action(ui, "sort-name", |ui| ui.toggle_sort(SortKey::Name));
    add_action(ui, "sort-size", |ui| ui.toggle_sort(SortKey::Size));
    add_action(ui, "sort-modified", |ui| ui.toggle_sort(SortKey::Modified));
    add_action(ui, "sort-kind", |ui| ui.toggle_sort(SortKey::Kind));
    add_action(ui, "folder-color", |ui| {
        let target = ui
            .context_target
            .borrow()
            .as_ref()
            .filter(|entry| entry.is_dir)
            .map(|entry| entry.path.clone());
        ui.choose_folder_color(target);
    });
    add_action(ui, "default-folder-color", |ui| {
        ui.choose_default_folder_color();
    });
    add_action(ui, "organize", |ui| ui.organize_by_extension());
    add_action(ui, "icon-theme", |ui| ui.choose_icon_theme());
    add_action(ui, "preview-icons", |ui| ui.choose_preview_icons());
    add_action(ui, "same-colour", |ui| ui.show_same_colour());
    add_action(ui, "same-colour-next", |ui| ui.jump_same_colour(true));
    add_action(ui, "same-colour-prev", |ui| ui.jump_same_colour(false));
    add_action(ui, "go-to", |ui| ui.start_path_edit());
    {
        // Stateful so the menu item shows a check mark.
        let inherit = ui.config.borrow().folder_color_inherit;
        let action =
            gio::SimpleAction::new_stateful("inherit-folder-colors", None, &inherit.to_variant());
        let weak = Rc::downgrade(ui);
        action.connect_activate(move |action, _| {
            let next = action
                .state()
                .and_then(|state| state.get::<bool>())
                .map(|current| !current)
                .unwrap_or(true);
            action.set_state(&next.to_variant());
            if let Some(ui) = weak.upgrade() {
                {
                    let mut cfg = ui.config.borrow_mut();
                    cfg.folder_color_inherit = next;
                    cfg.save();
                }
                ui.refresh_folder_icons();
                ui.toast(if next {
                    "Subfolders inherit their parent colour"
                } else {
                    "Subfolders use the default colour"
                });
            }
        });
        ui.window.add_action(&action);
    }
    add_action(ui, "copy-path", |ui| {
        if let Some(pane) = ui.active_pane() {
            let paths: Vec<String> = pane
                .selected_entries()
                .iter()
                .map(|e| e.path.display().to_string())
                .collect();
            if !paths.is_empty() {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&paths.join("\n"));
                }
                ui.toast("Path copied to clipboard");
            }
        }
    });
    add_action(ui, "open-new-tab", |ui| {
        if let Some(pane) = ui.active_pane() {
            let path = pane.selected_entries().first().map(|e| e.path.clone());
            if let Some(path) = path {
                let view = pane.view_mode.get();
                ui.add_tab(path, view, true);
            }
        }
    });
}

/// Build the whole application window.
pub fn build(app: &gtk::Application) -> Rc<AppUi> {
    let config = Config::load();
    let palette = Palette::load(None);
    let provider = theme::apply(&palette);

    // Nook's own icon theme: Colloid when installed, unless configured.
    if let Some(theme_name) =
        icons::preferred_theme(config.icon_theme.as_deref(), palette.is_dark())
    {
        icons::apply_icon_theme(&theme_name);
    }

    // Folder colours are shared by every pane and every tab.
    let folder_style = Rc::new(RefCell::new(FolderStyle {
        default: config
            .folder_color
            .as_deref()
            .and_then(icons::normalize_hex),
        overrides: config
            .folder_colors
            .iter()
            .filter_map(|(path, color)| {
                icons::normalize_hex(color).map(|color| (PathBuf::from(path), color))
            })
            .collect(),
        inherit: config.folder_color_inherit,
    }));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Nook")
        .default_width(config.window.width)
        .default_height(config.window.height)
        .build();
    window.add_css_class("nook");
    // Keep the taskbar icon out of the custom two-row header; the application
    // id still supplies it to the desktop shell.
    window.set_icon_name(None::<&str>);

    let preview_service = PreviewService::new(2);
    let preview = PreviewPane::new(preview_service.clone());
    preview.set_max_bytes(config.preview_max_bytes);

    let notebook = gtk::Notebook::new();
    notebook.add_css_class("nook-tabs");
    notebook.set_scrollable(true);
    notebook.set_show_border(false);
    notebook.set_show_tabs(false);
    notebook.set_vexpand(true);
    notebook.set_hexpand(true);

    // Native tab strip for the top bar: each tab carries an icon, the folder
    // name and its location. The notebook itself only owns the pages.
    let tab_strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    tab_strip.add_css_class("nook-tab-strip");
    tab_strip.set_hexpand(true);
    let tab_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .child(&tab_strip)
        .build();
    tab_scroller.add_css_class("nook-tab-scroll");
    tab_scroller.set_propagate_natural_height(true);

    let crumb_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let crumb_stack = gtk::Stack::new();
    let path_entry = gtk::Entry::new();
    path_entry.add_css_class("nook-entry");
    path_entry.add_css_class("nook-path-entry");
    path_entry.set_placeholder_text(Some("Type or paste a path, then press Enter"));
    path_entry.set_hexpand(true);
    // The entry may grow with the toolbar, but it can also shrink to almost
    // nothing so a narrow window never forces the whole toolbar out of view.
    path_entry.set_width_chars(16);
    path_entry.set_max_width_chars(64);
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 3);

    let status_left = gtk::Label::new(Some(""));
    status_left.set_xalign(0.0);
    let status_right = gtk::Label::new(Some(""));
    status_right.set_xalign(1.0);
    status_right.add_css_class("nook-dim");
    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status_box.add_css_class("nook-status");
    status_left.set_hexpand(true);
    status_box.append(&status_left);
    status_box.append(&status_right);

    let preview_revealer = gtk::Revealer::new();
    preview_revealer.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
    preview_revealer.set_transition_duration(180);
    preview_revealer.set_child(Some(&preview.root));
    preview_revealer.set_reveal_child(config.show_preview);

    let content_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    content_paned.set_start_child(Some(&notebook));
    content_paned.set_end_child(Some(&preview_revealer));
    content_paned.set_resize_start_child(true);
    content_paned.set_shrink_start_child(true);
    content_paned.set_resize_end_child(false);
    content_paned.set_shrink_end_child(true);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&content_paned);
    content_box.append(&status_box);

    // The collapse toggle lives in a fixed header at the top of the sidebar so
    // it is always reachable while the location list scrolls beneath it.
    let sidebar_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    sidebar_header.add_css_class("nook-sidebar-header");
    let sidebar_toggle = gtk::Button::new();
    sidebar_toggle.add_css_class("nook-sidebar-toggle");
    sidebar_toggle.set_focus_on_click(false);
    sidebar_header.append(&sidebar_toggle);

    let sidebar_scroller = gtk::ScrolledWindow::new();
    sidebar_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    sidebar_scroller.set_child(Some(&sidebar_box));
    sidebar_scroller.set_vexpand(true);

    let sidebar_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_container.add_css_class("nook-sidebar");
    sidebar_container.append(&sidebar_header);
    sidebar_container.append(&sidebar_scroller);
    sidebar_container.set_width_request(if config.sidebar_mini {
        SIDEBAR_MINI_WIDTH
    } else {
        SIDEBAR_WIDTH
    });

    let sidebar_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    sidebar_paned.set_start_child(Some(&sidebar_container));
    sidebar_paned.set_end_child(Some(&content_box));
    sidebar_paned.set_position(if config.sidebar_mini {
        SIDEBAR_MINI_WIDTH
    } else {
        SIDEBAR_WIDTH
    });
    sidebar_paned.set_resize_start_child(false);
    // The sidebar may shrink with the window instead of pushing the file view
    // out of the window when space runs out.
    sidebar_paned.set_shrink_start_child(true);
    sidebar_paned.set_resize_end_child(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("nook-root");
    root.append(&sidebar_paned);

    let toast_label = gtk::Label::new(None);
    toast_label.add_css_class("nook-toast");
    toast_label.set_wrap(true);
    toast_label.set_max_width_chars(64);
    let toast_revealer = gtk::Revealer::new();
    toast_revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    toast_revealer.set_transition_duration(180);
    toast_revealer.set_child(Some(&toast_label));
    toast_revealer.set_halign(gtk::Align::Center);
    toast_revealer.set_valign(gtk::Align::End);
    toast_revealer.set_margin_bottom(46);
    toast_revealer.set_can_focus(false);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&root));
    overlay.add_overlay(&toast_revealer);

    window.set_child(Some(&overlay));

    // Search bar sits directly under the header.
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Filter this folder…"));
    search_entry.add_css_class("nook-search");
    search_entry.set_hexpand(true);
    let search_mode = gtk::DropDown::from_strings(&["By name", "By extension"]);
    search_mode.add_css_class("nook-search-mode");
    search_mode.set_tooltip_text(Some("Search by file name or by file extension"));
    let search_recursive = gtk::ToggleButton::new();
    search_recursive.set_icon_name("folder-saved-search-symbolic");
    search_recursive.add_css_class("nook-btn");
    search_recursive.set_tooltip_text(Some("Search subfolders too"));
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_box.set_margin_top(2);
    search_box.set_margin_bottom(4);
    search_box.set_margin_start(10);
    search_box.set_margin_end(10);
    search_box.append(&search_entry);
    search_box.append(&search_mode);
    search_box.append(&search_recursive);
    let search_revealer = gtk::Revealer::new();
    search_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    search_revealer.set_transition_duration(150);
    search_revealer.set_child(Some(&search_box));
    search_revealer.set_reveal_child(false);

    let ui = Rc::new(AppUi {
        window: window.clone(),
        config: Rc::new(RefCell::new(config.clone())),
        palette: RefCell::new(palette),
        css_provider: RefCell::new(provider),
        notebook: notebook.clone(),
        tabs: RefCell::new(Vec::new()),
        tab_strip,
        tab_scroller,
        sidebar_box,
        sidebar_container: sidebar_container.clone(),
        sidebar_toggle: sidebar_toggle.clone(),
        sidebar_paned: sidebar_paned.clone(),
        crumb_box,
        crumb_stack,
        path_entry,
        path_editing: Cell::new(false),
        status_left,
        status_right,
        preview,
        preview_revealer,
        search_revealer: search_revealer.clone(),
        search_entry: search_entry.clone(),
        search_recursive: search_recursive.clone(),
        search_cancel: RefCell::new(None),
        toast_revealer,
        toast_label,
        toast_gen: Cell::new(0),
        clipboard: RefCell::new(None),
        folder_style: folder_style.clone(),
        context_target: RefCell::new(None),
        search_mode: Cell::new(SearchMode::Name),
        search_mode_dropdown: RefCell::new(None),
        sidebar_mini: Cell::new(config.sidebar_mini),
        width_bucket: Cell::new(-1),
        view_button: RefCell::new(None),
        view_icon: RefCell::new(gtk::Image::from_icon_name("view-grid-symbolic")),
        hidden_button: RefCell::new(gtk::ToggleButton::new()),
        hidden_icon: RefCell::new(gtk::Image::from_icon_name("view-conceal-symbolic")),
        preview_button: RefCell::new(gtk::ToggleButton::new()),
        run_button: RefCell::new(None),
        transfer: RefCell::new(None),
        menu_button: RefCell::new(None),
        settings_button: RefCell::new(None),
        folder_controls: RefCell::new(Vec::new()),
        nav_back: RefCell::new(None),
        nav_fwd: RefCell::new(None),
        chrome_path: RefCell::new(None),
        run_available: Cell::new(false),
        theme_watcher: RefCell::new(None),
    });

    // Sidebar: the fixed header toggle switches between full and icon rail.
    {
        let weak = Rc::downgrade(&ui);
        ui.sidebar_toggle.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.set_sidebar_mini(!ui.sidebar_mini.get());
            }
        });
    }
    ui.sync_sidebar_chrome();
    ui.sidebar_container.set_visible(true);

    // The header must be built after the struct exists because it wires actions.
    let header = ui.build_header();
    window.set_titlebar(Some(&header));

    // Keep the chrome in step with the window size. GtkWindow's default-width
    // is updated on every resize, which is GTK 4's documented way to observe
    // a toplevel's size, so no polling is needed.
    {
        let weak = Rc::downgrade(&ui);
        window.connect_notify_local(Some("default-width"), move |window, _| {
            if let Some(ui) = weak.upgrade() {
                ui.apply_adaptive_layout(window.default_width());
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.apply_adaptive_layout(ui.window.default_width());
            }
        });
    }

    // Sidebar keyboard navigation: Up/Down walk the places, Escape hands
    // focus back to the file list. The rows are rebuilt often, so the
    // controller reads them live instead of caching widgets.
    {
        let sidebar_box = ui.sidebar_box.clone();
        let weak = Rc::downgrade(&ui);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            let rows = sidebar_rows(&sidebar_box);
            if rows.is_empty() {
                return glib::Propagation::Proceed;
            }
            let current = rows.iter().position(|row| row.has_focus());
            let target = match key {
                gtk::gdk::Key::Down => Some(match current {
                    Some(index) => (index + 1).min(rows.len() - 1),
                    None => 0,
                }),
                gtk::gdk::Key::Up => Some(match current {
                    Some(index) => index.saturating_sub(1),
                    None => rows.len() - 1,
                }),
                gtk::gdk::Key::Home => Some(0),
                gtk::gdk::Key::End => Some(rows.len() - 1),
                gtk::gdk::Key::Escape => {
                    if let Some(ui) = weak.upgrade() {
                        if let Some(pane) = ui.active_pane() {
                            pane.focus_view();
                        }
                    }
                    return glib::Propagation::Stop;
                }
                _ => None,
            };
            match target {
                Some(index) => {
                    rows[index].grab_focus();
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        ui.sidebar_box.add_controller(keys);
    }

    // The Menu key opens the context menu, like Shift+F10 does through its
    // accelerator; some keyboards send only one of the two.
    {
        let weak = Rc::downgrade(&ui);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Menu {
                return glib::Propagation::Proceed;
            }
            if let Some(ui) = weak.upgrade() {
                ui.context_menu_keyboard();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);
    }

    // Search bar belongs inside the content area above the notebook.
    if let Some(end) = sidebar_paned.end_child() {
        if let Some(content_box) = end.downcast::<gtk::Box>().ok() {
            content_box.prepend(&search_revealer);
        }
    }

    // Inline path editing: Enter navigates, Escape restores the breadcrumbs and
    // clicking away cancels without changing the current folder.
    {
        let weak = Rc::downgrade(&ui);
        ui.path_entry.connect_activate(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.submit_path_edit();
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(ui) = weak.upgrade() {
                ui.cancel_path_edit();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        ui.path_entry.add_controller(keys);
    }
    {
        let weak = Rc::downgrade(&ui);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.path_editing.get() {
                    ui.cancel_path_edit();
                }
            }
        });
        ui.path_entry.add_controller(focus);
    }

    // Wire search.
    {
        let weak = Rc::downgrade(&ui);
        search_entry.connect_search_changed(move |e| {
            if let Some(ui) = weak.upgrade() {
                ui.search_changed(&e.text());
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        search_mode.connect_selected_notify(move |dropdown| {
            if let Some(ui) = weak.upgrade() {
                let mode = if dropdown.selected() == 1 {
                    SearchMode::Extension
                } else {
                    SearchMode::Name
                };
                ui.search_mode.set(mode);
                ui.search_entry.set_placeholder_text(Some(match mode {
                    SearchMode::Name => "Filter by name…",
                    SearchMode::Extension => "Filter by extension (png, jpg…)…",
                }));
                if let Some(pane) = ui.active_pane() {
                    pane.set_search_mode(mode);
                }
                let text = ui.search_entry.text().to_string();
                ui.search_changed(&text);
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        search_recursive.connect_toggled(move |b| {
            if let Some(ui) = weak.upgrade() {
                if b.is_active() {
                    let text = ui.search_entry.text().to_string();
                    ui.search_changed(&text);
                } else if let Some(pane) = ui.active_pane() {
                    pane.set_entries(Vec::new());
                    pane.reload();
                }
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        search_entry.connect_stop_search(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_revealer.set_reveal_child(false);
                if let Some(pane) = ui.active_pane() {
                    pane.set_search("");
                    pane.focus_view();
                }
            }
        });
    }

    *ui.search_mode_dropdown.borrow_mut() = Some(search_mode.clone());

    register_actions(&ui);
    apply_shortcuts(app, &config);
    ui.start_transfer_service();

    // Theme watcher keeps Nook in sync with wallpaper changes.
    {
        let weak = Rc::downgrade(&ui);
        let watcher = theme::ThemeWatcher::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.reload_theme();
            }
        });
        *ui.theme_watcher.borrow_mut() = Some(watcher);
    }

    // Restore the session, or start in the home directory.
    let restored: Vec<settings::TabConfig> = config.tabs.clone();
    if restored.is_empty() {
        let start = settings::home_dir();
        ui.add_tab(start, config.view, true);
    } else {
        for (i, tab_cfg) in restored.iter().enumerate() {
            let path = settings::expand_path(&tab_cfg.path);
            let path = if path.is_dir() {
                path
            } else {
                settings::home_dir()
            };
            ui.add_tab(path.clone(), tab_cfg.view, i == config.active_tab);
        }
        ui.notebook
            .set_current_page(Some(config.active_tab.min(restored.len() - 1) as u32));
    }

    if config.window.maximized {
        window.maximize();
    }

    // Tab switching refreshes all chrome.
    {
        let weak = Rc::downgrade(&ui);
        notebook.connect_switch_page(move |_, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_chrome();
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        notebook.connect_page_reordered(move |nb, page, _| {
            if let Some(ui) = weak.upgrade() {
                let mut tabs = ui.tabs.borrow_mut();
                if let Some(pos) = tabs.iter().position(|t| &t.page == page) {
                    let tab = tabs.remove(pos);
                    let idx = nb.page_num(page).unwrap_or(0) as usize;
                    let len = tabs.len();
                    tabs.insert(idx.min(len), tab);
                }
            }
        });
    }

    // Persist session state on close.
    {
        let ui_weak = Rc::downgrade(&ui);
        window.connect_close_request(move |_| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut cfg = ui.config.borrow_mut();
                // A tab showing a library view is saved as the folder it holds:
                // library views are transient states, not part of the session.
                cfg.tabs = ui
                    .tabs
                    .borrow()
                    .iter()
                    .map(|tab| settings::TabConfig {
                        path: tab.pane.path().to_string_lossy().to_string(),
                        view: tab.pane.view_mode.get(),
                    })
                    .collect();
                cfg.active_tab = ui.notebook.current_page().unwrap_or(0) as usize;
                let (w, h) = (ui.window.width(), ui.window.height());
                if w > 200 && h > 200 {
                    cfg.window.width = w;
                    cfg.window.height = h;
                }
                cfg.window.maximized = ui.window.is_maximized();
                cfg.show_preview = ui.preview_revealer.reveals_child();
                cfg.show_sidebar = true;
                cfg.save();
            }
            glib::Propagation::Proceed
        });
    }

    // Every signal handler above holds only a `Weak` reference, and the caller
    // drops its `Rc` as soon as this function returns. Keep one strong reference
    // alive for as long as the window exists, otherwise the whole UI state (and
    // the theme watcher) would be dropped while the window is still on screen.
    LIVE_STATE.with(|live| live.borrow_mut().push(ui.clone()));
    {
        let weak = Rc::downgrade(&ui);
        window.connect_destroy(move |_| {
            if let Some(ui) = weak.upgrade() {
                LIVE_STATE.with(|live| {
                    live.borrow_mut().retain(|other| !Rc::ptr_eq(other, &ui));
                });
            }
        });
    }

    ui.refresh_chrome();

    ui
}
