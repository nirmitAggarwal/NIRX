//! NIRX — a developer-focused GTK4 file manager for Linux.

mod apps;
mod fsutil;
mod git;
mod icons;
mod localsend;
mod preview;
mod run_tasks;
mod search;
mod settings;
mod theme;
mod thumb;
mod ui;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

/// Directory passed on the command line, if any.
fn cli_directory() -> Option<std::path::PathBuf> {
    let arg = std::env::args().skip(1).find(|a| !a.starts_with('-'))?;
    let path = settings::expand_path(&arg);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    if path.is_dir() {
        Some(path)
    } else {
        path.parent()
            .filter(|p| p.is_dir())
            .map(|p| p.to_path_buf())
    }
}

fn main() -> glib::ExitCode {
    glib::set_prgname(Some("nirx"));

    let app = gtk::Application::builder()
        .application_id("dev.nirx.Files")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        let quit = gio::SimpleAction::new("quit", None);
        let app_weak = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = app_weak.upgrade() {
                if let Some(window) = app.active_window() {
                    window.close();
                }
            }
        });
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
    });

    app.connect_activate(|app| {
        let ui = ui::build(app);
        if let Some(dir) = cli_directory() {
            if let Some(pane) = ui.active_pane() {
                pane.navigate(dir, true);
            }
        }
        ui.window.present();
    });

    app.connect_open(|app, files, _hint| {
        let ui = ui::build(app);
        if let Some(file) = files.first() {
            if let Some(path) = file.path() {
                let target = if path.is_dir() {
                    path
                } else {
                    path.parent().map(|p| p.to_path_buf()).unwrap_or(path)
                };
                if let Some(pane) = ui.active_pane() {
                    pane.navigate(target, true);
                }
            }
        }
        ui.window.present();
    });

    app.run()
}
