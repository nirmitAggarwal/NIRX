//! In-window library views: Trash, Recent files, Recent locations and Network.
//!
//! A library view is a *state* of a tab, not a separate window or tab: it takes
//! the place of the folder view inside the tab, sharing the tab strip,
//! breadcrumbs, status bar and preview chrome with ordinary folders. Actions on
//! the listed items live in a bar along the bottom edge, so restore and delete
//! stay reachable for any number of selected rows and in a narrow window.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::fsutil;
use crate::settings;

use super::{dialogs, AppUi};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryKind {
    Trash,
    RecentFiles,
    RecentLocations,
    Network,
}

impl LibraryKind {
    pub fn title(self) -> &'static str {
        match self {
            LibraryKind::Trash => "Trash",
            LibraryKind::RecentFiles => "Recent Files",
            LibraryKind::RecentLocations => "Recent Locations",
            LibraryKind::Network => "Network",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            LibraryKind::Trash => "user-trash-symbolic",
            LibraryKind::RecentFiles => "document-open-recent-symbolic",
            LibraryKind::RecentLocations => "go-jump-symbolic",
            LibraryKind::Network => "network-server-symbolic",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            LibraryKind::Trash => "Items moved to trash, with restore and permanent delete.",
            LibraryKind::RecentFiles => "Files opened in Nook, most recent first.",
            LibraryKind::RecentLocations => "Folders visited recently in this session and before.",
            LibraryKind::Network => {
                "Mounted network shares and addresses the desktop can browse. \
                 Sharing requires gvfs on the system."
            }
        }
    }

    /// Singular and plural noun for the status bar and the action bar.
    fn noun(self) -> (&'static str, &'static str) {
        match self {
            LibraryKind::RecentLocations => ("folder", "folders"),
            _ => ("item", "items"),
        }
    }
}

/// The value behind one row, kept next to its widget so the action bar can act
/// on the current selection without re-reading the disk.
#[derive(Clone)]
enum RowItem {
    Trashed(fsutil::TrashedItem),
    File(PathBuf),
    Folder(PathBuf),
    Uri(String),
}

type RowMap = Rc<RefCell<Vec<(gtk::ListBoxRow, RowItem)>>>;

/// A library view ready to be shown in a tab.
pub struct LibraryView {
    pub root: gtk::Widget,
    /// Rebuilds the rows and the status line from the current state.
    pub refresh: Rc<dyn Fn()>,
    /// Puts the keyboard on the list when the view is opened.
    pub focus: Rc<dyn Fn()>,
}

pub fn build(ui: &Rc<AppUi>, kind: LibraryKind) -> LibraryView {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("nook-surface");
    root.set_hexpand(true);
    root.set_vexpand(true);

    let list = gtk::ListBox::new();
    // Multiple selection so a whole run of rows can be restored or purged at
    // once from the action bar instead of one button press per row.
    list.set_selection_mode(gtk::SelectionMode::Multiple);
    list.add_css_class("nook-library");
    list.set_hexpand(true);
    // Rows keep their natural height instead of stretching over the window.
    list.set_valign(gtk::Align::Start);

    // Rows may be wider than a very narrow window; scroll instead of clipping.
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&list)
        .build();
    scroller.add_css_class("nook-surface-bare");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("nook-library-actions");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let rows: RowMap = Rc::new(RefCell::new(Vec::new()));

    // The page lives inside the window, so never hold a strong AppUi here:
    // that would keep the window alive after it is closed.
    let fill: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(ui);
        let list = list.clone();
        let rows = rows.clone();
        Rc::new(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            rows.borrow_mut().clear();
            match kind {
                LibraryKind::Trash => fill_trash(&list, &rows),
                LibraryKind::RecentFiles => fill_recent_files(&ui, &list, &rows),
                LibraryKind::RecentLocations => fill_recent_locations(&ui, &list, &rows),
                LibraryKind::Network => fill_network(&list, &rows),
            }
            update_status(&ui, kind, &list, &rows);
        })
    };

    let refresh = gtk::Button::with_label("Refresh");
    refresh.add_css_class("nook-action");
    refresh.set_valign(gtk::Align::Center);
    {
        let fill = fill.clone();
        refresh.connect_clicked(move |_| fill());
    }
    bar.append(&refresh);

    let actions: Vec<(&str, bool, Rc<dyn Fn(Vec<RowItem>)>)> = match kind {
        LibraryKind::Trash => vec![
            ("Restore", false, restore_action(ui, fill.clone())),
            ("Delete Permanently", true, purge_action(ui, fill.clone())),
        ],
        LibraryKind::RecentFiles => vec![
            ("Open", false, open_files_action(ui)),
            ("Show Folder", false, reveal_action(ui)),
            ("Copy Path", false, copy_path_action(ui)),
        ],
        LibraryKind::RecentLocations => vec![
            ("Open", false, open_folder_action(ui)),
            ("Copy Path", false, copy_path_action(ui)),
        ],
        LibraryKind::Network => vec![("Open", false, open_uri_action(ui))],
    };

    let mut buttons: Vec<gtk::Button> = Vec::new();
    let mut primary: Option<gtk::Button> = None;
    let mut dangerous: Option<gtk::Button> = None;
    for (index, (label, danger, run)) in actions.into_iter().enumerate() {
        let button = gtk::Button::with_label(label);
        button.add_css_class("nook-action");
        if danger {
            button.add_css_class("nook-destructive");
        }
        button.set_valign(gtk::Align::Center);
        // Nothing is selected yet; the selection handler enables the actions.
        button.set_sensitive(false);
        {
            let rows = rows.clone();
            button.connect_clicked(move |_| {
                let items: Vec<RowItem> = rows
                    .borrow()
                    .iter()
                    .filter(|(row, _)| row.is_selected())
                    .map(|(_, item)| item.clone())
                    .collect();
                if !items.is_empty() {
                    run(items);
                }
            });
        }
        if index == 0 {
            primary = Some(button.clone());
        }
        if danger {
            dangerous = Some(button.clone());
        }
        bar.append(&button);
        buttons.push(button);
    }

    {
        let weak = Rc::downgrade(ui);
        let list = list.clone();
        let rows = rows.clone();
        let buttons = buttons.clone();
        list.clone().connect_selected_rows_changed(move |_| {
            let selected = list.selected_rows().len();
            for button in &buttons {
                button.set_sensitive(selected > 0);
            }
            if let Some(ui) = weak.upgrade() {
                update_status(&ui, kind, &list, &rows);
            }
        });
    }

    // Keyboard model: the list follows the arrow keys, Enter runs the first
    // action (Restore / Open), Delete runs the destructive one, Ctrl+A selects
    // everything and Escape takes the tab back to its folder.
    {
        let weak = Rc::downgrade(ui);
        let list = list.clone();
        let rows = rows.clone();
        let primary = primary.clone();
        let dangerous = dangerous.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| match key {
            gtk::gdk::Key::Escape => {
                if let Some(ui) = weak.upgrade() {
                    ui.close_library();
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                if let Some(button) = primary.as_ref().filter(|button| button.is_sensitive()) {
                    button.emit_clicked();
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Delete => {
                if let Some(button) = dangerous.as_ref().filter(|button| button.is_sensitive()) {
                    button.emit_clicked();
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::a if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                list.select_all();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::space => {
                let focused = rows
                    .borrow()
                    .iter()
                    .find(|(row, _)| row.has_focus())
                    .map(|(row, _)| row.clone());
                match focused {
                    Some(row) => {
                        if row.is_selected() {
                            list.unselect_row(&row);
                        } else {
                            list.select_row(Some(&row));
                        }
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            }
            _ => glib::Propagation::Proceed,
        });
        root.add_controller(keys);
    }

    root.append(&scroller);
    root.append(&bar);
    fill();

    let focus: Rc<dyn Fn()> = {
        let list = list.clone();
        Rc::new(move || {
            list.grab_focus();
        })
    };

    LibraryView {
        root: root.upcast(),
        refresh: fill,
        focus,
    }
}

/// `12 items` or `12 items · 2 selected` in the window's status bar.
fn update_status(ui: &Rc<AppUi>, kind: LibraryKind, list: &gtk::ListBox, rows: &RowMap) {
    let total = rows.borrow().len();
    let selected = list.selected_rows().len();
    let (one, many) = kind.noun();
    let noun = if total == 1 { one } else { many };
    let text = if selected > 0 {
        format!("{total} {noun} · {selected} selected")
    } else {
        format!("{total} {noun}")
    };
    ui.set_status_text(&text);
}

/* ------------------------------- filling ------------------------------- */

fn fill_trash(list: &gtk::ListBox, rows: &RowMap) {
    let items = fsutil::list_trash();
    if items.is_empty() {
        empty_row(list, "Trash is empty");
        return;
    }
    for item in items {
        let label = item
            .original
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| item.name.clone());
        let icon = if item.is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        };
        let sub = if item.trashed_at.is_empty() {
            dialogs::pretty_path(&item.original)
        } else {
            format!(
                "{} · deleted {}",
                dialogs::pretty_path(&item.original),
                item.trashed_at
            )
        };
        append_row(list, rows, icon, &label, &sub, RowItem::Trashed(item));
    }
}

fn fill_recent_files(ui: &Rc<AppUi>, list: &gtk::ListBox, rows: &RowMap) {
    let files: Vec<PathBuf> = ui
        .config
        .borrow()
        .recent_files
        .iter()
        .map(|p| settings::expand_path(p))
        .filter(|p| p.is_file())
        .collect();
    if files.is_empty() {
        empty_row(
            list,
            "No files opened yet — open one from any folder and it will appear here.",
        );
        return;
    }
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        append_row(
            list,
            rows,
            "text-x-generic-symbolic",
            &name,
            &dialogs::pretty_path(&path),
            RowItem::File(path),
        );
    }
}

fn fill_recent_locations(ui: &Rc<AppUi>, list: &gtk::ListBox, rows: &RowMap) {
    let mut dirs: Vec<PathBuf> = ui
        .config
        .borrow()
        .recent_destinations
        .iter()
        .map(|p| settings::expand_path(p))
        .filter(|p| p.is_dir())
        .collect();
    for tab in ui.tabs.borrow().iter() {
        let path = tab.pane.path();
        if path.is_dir() {
            dirs.retain(|p| p != &path);
            dirs.push(path);
        }
    }
    dirs.truncate(30);
    if dirs.is_empty() {
        empty_row(list, "No recent locations yet.");
        return;
    }
    for path in dirs {
        append_row(
            list,
            rows,
            "folder-symbolic",
            &dialogs::pretty_path(&path),
            &path.display().to_string(),
            RowItem::Folder(path),
        );
    }
}

fn fill_network(list: &gtk::ListBox, rows: &RowMap) {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let monitor = gio::VolumeMonitor::get();
    for mount in monitor.mounts() {
        let uri = mount.root().uri().to_string();
        if uri.starts_with("file://") {
            continue;
        }
        let name = mount.name().to_string();
        let label = if name.trim().is_empty() {
            uri.clone()
        } else {
            name
        };
        entries.push(("network-server-symbolic".into(), label, uri));
    }
    for (label, uri) in [
        ("Windows shares", "smb:///"),
        ("Network servers", "network:///"),
        ("iPhone / iPad (afc)", "afc:///"),
        ("MTP devices", "mtp://[sidecar]/"),
    ] {
        entries.push((
            "network-workgroup-symbolic".into(),
            label.into(),
            uri.into(),
        ));
    }
    if entries.is_empty() {
        empty_row(list, "No network locations found.");
        return;
    }
    for (icon, label, uri) in entries {
        append_row(list, rows, &icon, &label, &uri, RowItem::Uri(uri.clone()));
    }
}

/* ------------------------------- actions ------------------------------- */

fn restore_action(ui: &Rc<AppUi>, fill: Rc<dyn Fn()>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut report = fsutil::OpReport::default();
        for item in items {
            let RowItem::Trashed(item) = item else {
                continue;
            };
            match fsutil::restore_trashed(&item) {
                Ok(_) => report.done += 1,
                Err(err) => report.errors.push((item.original, err.to_string())),
            }
        }
        if report.done > 0 {
            for pane in ui.all_panes() {
                pane.soft_refresh();
            }
        }
        ui.toast(&report.summary("Restored"));
        if let Some((path, err)) = report.errors.first() {
            dialogs::error(
                &ui.window,
                "Could not restore",
                &format!("{}\n\n{err}", path.display()),
            );
        }
        fill();
    })
}

fn purge_action(ui: &Rc<AppUi>, fill: Rc<dyn Fn()>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let trashed: Vec<fsutil::TrashedItem> = items
            .into_iter()
            .filter_map(|item| match item {
                RowItem::Trashed(item) => Some(item),
                _ => None,
            })
            .collect();
        let count = trashed.len();
        if count == 0 {
            return;
        }
        let weak = Rc::downgrade(&ui);
        let fill = fill.clone();
        dialogs::confirm(
            &ui.window,
            "Delete permanently?",
            &format!(
                "{count} item{} will be deleted from the trash. This cannot be undone.",
                if count == 1 { "" } else { "s" }
            ),
            "Delete",
            true,
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let mut report = fsutil::OpReport::default();
                for item in &trashed {
                    match fsutil::purge_trashed(item) {
                        Ok(()) => report.done += 1,
                        Err(err) => report.errors.push((item.original.clone(), err.to_string())),
                    }
                }
                ui.toast(&report.summary("Deleted"));
                if let Some((path, err)) = report.errors.first() {
                    dialogs::error(
                        &ui.window,
                        "Could not delete",
                        &format!("{}\n\n{err}", path.display()),
                    );
                }
                fill();
            },
        );
    })
}

fn open_files_action(ui: &Rc<AppUi>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let paths: Vec<PathBuf> = items
            .into_iter()
            .filter_map(|item| match item {
                RowItem::File(path) => Some(path),
                _ => None,
            })
            .collect();
        ui.open_files(&paths);
    })
}

/// Show the folder a listed file lives in, in the same tab.
fn reveal_action(ui: &Rc<AppUi>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let first = items.into_iter().find_map(|item| match item {
            RowItem::File(path) => path.parent().map(|p| p.to_path_buf()),
            RowItem::Folder(path) => Some(path),
            _ => None,
        });
        if let Some(dir) = first {
            ui.open_folder_here(dir);
        }
    })
}

fn open_folder_action(ui: &Rc<AppUi>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let first = items.into_iter().find_map(|item| match item {
            RowItem::Folder(path) => Some(path),
            _ => None,
        });
        if let Some(dir) = first {
            ui.open_folder_here(dir);
        }
    })
}

fn copy_path_action(ui: &Rc<AppUi>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let paths: Vec<String> = items
            .into_iter()
            .filter_map(|item| match item {
                RowItem::File(path) | RowItem::Folder(path) => {
                    Some(path.to_string_lossy().to_string())
                }
                RowItem::Uri(uri) => Some(uri),
                RowItem::Trashed(_) => None,
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        copy_to_clipboard(&paths.join("\n"));
        ui.toast(if paths.len() == 1 {
            "Copied path"
        } else {
            "Copied paths"
        });
    })
}

fn open_uri_action(ui: &Rc<AppUi>) -> Rc<dyn Fn(Vec<RowItem>)> {
    let weak = Rc::downgrade(ui);
    Rc::new(move |items| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let first = items.into_iter().find_map(|item| match item {
            RowItem::Uri(uri) => Some(uri),
            _ => None,
        });
        if let Some(uri) = first {
            open_uri_location(&ui.window, &uri);
        }
    })
}

/* --------------------------------- rows -------------------------------- */

fn append_row(
    list: &gtk::ListBox,
    rows: &RowMap,
    icon: &str,
    title: &str,
    sub: &str,
    item: RowItem,
) {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("nook-library-row");

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(10);
    hbox.set_margin_end(10);

    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(20);
    image.set_valign(gtk::Align::Center);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let name = gtk::Label::new(Some(title));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("nook-file-name");
    let sub = gtk::Label::new(Some(sub));
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    sub.add_css_class("nook-meta");
    text.append(&name);
    text.append(&sub);

    hbox.append(&image);
    hbox.append(&text);
    row.set_child(Some(&hbox));
    list.append(&row);
    rows.borrow_mut().push((row, item));
}

fn empty_row(list: &gtk::ListBox, text: &str) {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("nook-meta");
    label.set_margin_top(20);
    label.set_margin_bottom(20);
    label.set_margin_start(14);
    label.set_margin_end(14);
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_child(Some(&label));
    list.append(&row);
}

fn copy_to_clipboard(text: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

/// Hand a non-local URI to the desktop's own handler; Nook's panes browse
/// local folders only.
fn open_uri_location(parent: &impl IsA<gtk::Window>, uri: &str) {
    if let Err(err) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        dialogs::error(
            parent,
            "Cannot open this location",
            &format!("{uri}\n\n{err}"),
        );
    }
}
