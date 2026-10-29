//! Quick Shift: move the current selection to a destination chosen by typing.
//!
//! Destinations are ranked from the most recent Quick Shift targets, the user's
//! bookmarks, the usual XDG folders, and whatever path is typed. Conflicts are
//! resolved with one decision for the whole batch, and the move itself runs on a
//! worker thread with a progress dialog.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::fsutil;
use crate::settings::{self, Config};
use crate::ui::dialogs::{self, ConflictChoice, ProgressDialog};

pub fn show(
    parent: &impl IsA<gtk::Window>,
    config: Rc<RefCell<Config>>,
    sources: Vec<PathBuf>,
    on_done: Rc<dyn Fn(String)>,
) {
    if sources.is_empty() {
        return;
    }
    let win = gtk::Window::builder()
        .title("Quick Shift")
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(560)
        .default_height(470)
        .build();
    win.add_css_class("nook-quickshift");
    win.set_decorated(true);

    let count = sources.len();
    let heading = gtk::Label::new(Some(&format!(
        "Move {count} item{} where?",
        if count == 1 { "" } else { "s" }
    )));
    heading.set_xalign(0.0);
    heading.add_css_class("nook-empty-title");

    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search destinations or type a path…"));
    entry.add_css_class("nook-search");

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();

    let hint = gtk::Label::new(Some("↑↓ to choose  ·  Enter to move  ·  Esc to cancel"));
    hint.add_css_class("nook-meta");
    hint.set_xalign(0.0);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(16);
    root.set_margin_bottom(12);
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.append(&heading);
    root.append(&entry);
    root.append(&scroller);
    root.append(&hint);
    win.set_child(Some(&root));

    let destinations: Rc<RefCell<Vec<(String, String, PathBuf)>>> =
        Rc::new(RefCell::new(Vec::new()));

    let build_destinations = {
        let config = config.borrow();
        let mut out: Vec<(String, String, PathBuf)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for recent in &config.recent_destinations {
            let p = settings::expand_path(recent);
            if p.is_dir() && seen.insert(p.clone()) {
                out.push((
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "/".into()),
                    format!("Recent · {}", dialogs::pretty_path(&p)),
                    p,
                ));
            }
        }
        for bm in &config.bookmarks {
            let p = settings::expand_path(&bm.path);
            if p.is_dir() && seen.insert(p.clone()) {
                out.push((
                    bm.name.clone(),
                    format!("Bookmark · {}", dialogs::pretty_path(&p)),
                    p,
                ));
            }
        }
        let home = settings::home_dir();
        for dir in [
            home.clone(),
            home.join("Desktop"),
            home.join("Documents"),
            home.join("Downloads"),
            home.join("Pictures"),
            home.join("Videos"),
            home.join("Music"),
            home.join("Projects"),
            home.join("code"),
        ] {
            if dir.is_dir() && seen.insert(dir.clone()) {
                out.push((
                    dir.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Home".into()),
                    dialogs::pretty_path(&dir),
                    dir,
                ));
            }
        }
        out
    };

    let rebuild = {
        let list = list.clone();
        let destinations = destinations.clone();
        move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let mut shown: Vec<(String, String, PathBuf)> = Vec::new();

            let trimmed = query.trim();
            if !trimmed.is_empty() {
                let path = settings::expand_path(trimmed);
                if path.is_dir() {
                    shown.push(("Move here".to_string(), dialogs::pretty_path(&path), path));
                }
            }

            let mut scored: Vec<(i32, (String, String, PathBuf))> = build_destinations
                .iter()
                .filter_map(|d| {
                    if trimmed.is_empty() {
                        Some((0, d.clone()))
                    } else {
                        let hay = format!("{} {}", d.0, d.2.display());
                        crate::search::fuzzy_score(trimmed, &hay).map(|s| (s, d.clone()))
                    }
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            shown.extend(scored.into_iter().map(|(_, d)| d));

            for (label, sub, path) in &shown {
                let row = gtk::ListBoxRow::new();
                row.set_activatable(true);
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                hbox.add_css_class("nook-qs-dest");
                hbox.set_margin_top(2);
                hbox.set_margin_bottom(2);
                let icon = gtk::Image::from_icon_name("folder-symbolic");
                icon.set_pixel_size(18);
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 1);
                vbox.set_hexpand(true);
                let l = gtk::Label::new(Some(label));
                l.set_xalign(0.0);
                let s = gtk::Label::new(Some(sub));
                s.set_xalign(0.0);
                s.add_css_class("nook-meta");
                s.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                vbox.append(&l);
                vbox.append(&s);
                hbox.append(&icon);
                hbox.append(&vbox);
                row.set_child(Some(&hbox));
                row.set_tooltip_text(Some(&path.display().to_string()));
                list.append(&row);
            }
            *destinations.borrow_mut() = shown;
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    rebuild("");

    {
        let rebuild = rebuild.clone();
        entry.connect_search_changed(move |e| rebuild(&e.text()));
    }

    // Perform the move once a destination is picked.
    let move_to = {
        let win = win.clone();
        let config = config.clone();
        let sources: Vec<PathBuf> = sources;
        let on_done = on_done.clone();
        let destinations = destinations.clone();
        let list = list.clone();
        move || {
            let Some(row) = list.selected_row() else {
                return;
            };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let target = destinations.borrow().get(idx as usize).map(|d| d.2.clone());
            let Some(target) = target else { return };

            let conflicts: Vec<PathBuf> = sources
                .iter()
                .filter_map(|s| s.file_name().map(|n| target.join(n)))
                .filter(|p| p.exists())
                .collect();
            let sources_for_start = sources.clone();
            let config_for_start = config.clone();
            let on_done_for_start = on_done.clone();
            let win_for_start = win.clone();
            let target_for_start = target.clone();
            let start = move |choice: ConflictChoice| {
                let dialog = ProgressDialog::new(&win_for_start, "Moving items");
                let tx = dialog.tx.clone();
                let cancel = dialog.cancel_flag();
                let sources = sources_for_start.clone();
                let target_thread = target_for_start.clone();
                let target_recent = target_for_start.clone();
                let total = sources.len().max(1);
                let (done_tx, done_rx) = async_channel::bounded::<fsutil::OpReport>(1);
                dialog.message(format!(
                    "Moving {total} item{}…",
                    if total == 1 { "" } else { "s" }
                ));
                std::thread::Builder::new()
                    .name("nook-move".into())
                    .spawn(move || {
                        let report = fsutil::move_paths(
                            &sources,
                            &target_thread,
                            &|_| choice.to_fs(),
                            &|done, _bytes| {
                                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                    return;
                                }
                                let _ =
                                    tx.send_blocking(crate::ui::dialogs::ProgressMsg::Fraction(
                                        done as f64 / total as f64,
                                    ));
                            },
                        );
                        let _ = done_tx.send_blocking(report);
                    })
                    .ok();
                let win_done = win_for_start.clone();
                let config = config_for_start.clone();
                let on_done = on_done_for_start.clone();
                glib::spawn_future_local(async move {
                    if let Ok(report) = done_rx.recv().await {
                        dialog.finish();
                        if report.is_ok() {
                            config.borrow_mut().push_recent_destination(&target_recent);
                            config.borrow().save();
                        }
                        let mut summary = report.summary("Moved");
                        if let Some(first_error) = report.errors.first() {
                            summary.push_str(&format!(
                                " — {}: {}",
                                first_error.0.display(),
                                first_error.1
                            ));
                        }
                        win_done.close();
                        on_done(summary);
                    }
                });
            };

            if conflicts.is_empty() {
                start(ConflictChoice::Replace);
            } else {
                let start = Rc::new(start);
                let first = conflicts[0].clone();
                dialogs::conflict_prompt(&win, &first, conflicts.len(), move |choice| {
                    start(choice);
                });
            }
        }
    };

    {
        let move_to = move_to.clone();
        list.connect_row_activated(move |_, _| move_to());
    }
    {
        let move_to = move_to.clone();
        entry.connect_activate(move |_| move_to());
    }
    {
        let list = list.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| match keyval {
            gtk::gdk::Key::Down | gtk::gdk::Key::Tab => {
                let count = list.observe_children().n_items() as i32;
                if count == 0 {
                    return glib::Propagation::Stop;
                }
                let idx = list.selected_row().map(|r| r.index()).unwrap_or(-1);
                let next = (idx + 1).clamp(0, count - 1);
                if let Some(row) = list.row_at_index(next) {
                    list.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                let idx = list.selected_row().map(|r| r.index()).unwrap_or(1);
                let prev = (idx - 1).max(0);
                if let Some(row) = list.row_at_index(prev) {
                    list.select_row(Some(&row));
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        win.add_controller(key);
    }
    {
        let win_key = win.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                win_key.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(key);
    }

    win.present();
    entry.grab_focus();
}
