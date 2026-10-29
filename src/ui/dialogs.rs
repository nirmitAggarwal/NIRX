//! Modal dialogs: confirmations, text prompts, Open With, properties, bulk
//! rename, checksums, conflict resolution, progress and shortcut editing.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::fsutil::{self, FileEntry};
use crate::settings::{Config, RunCommand};

/// A colour button showing `color` as its background. The colour itself comes
/// from a generated `.nook-swatch-<hex>` rule in the theme stylesheet.
fn color_swatch(color: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("nook-swatch");
    button.add_css_class(&format!("nook-swatch-{}", color.trim_start_matches('#')));
    button.set_size_request(30, 30);
    button.set_tooltip_text(Some(color));
    button
}

/// Pick a folder colour from the presets or type a hex value.
///
/// `None` means "no tint": the global dialog labels that button "Theme icon",
/// the per-folder dialog labels it "Use default colour".
pub fn folder_color<F>(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    message: &str,
    current: Option<&str>,
    none_label: &str,
    on_choice: F,
) where
    F: Fn(Option<String>) + 'static,
{
    let win = base_window(parent, title);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(title));
    let body = body_label(message);
    body.set_max_width_chars(52);
    root.append(&body);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    let chosen: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let finish: Rc<dyn Fn()> = {
        let win = win.clone();
        let chosen = chosen.clone();
        Rc::new(move || {
            let value = chosen.borrow().clone();
            on_choice(value);
            win.close();
        })
    };

    for (index, (_name, color)) in crate::icons::FOLDER_COLOR_PRESETS.iter().enumerate() {
        let swatch = color_swatch(color);
        if current
            .map(|c| c.eq_ignore_ascii_case(color))
            .unwrap_or(false)
        {
            swatch.add_css_class("selected");
        }
        let chosen = chosen.clone();
        let finish = finish.clone();
        let color = color.to_string();
        swatch.connect_clicked(move |_| {
            *chosen.borrow_mut() = Some(color.clone());
            finish();
        });
        grid.attach(&swatch, (index % 6) as i32, (index / 6) as i32, 1, 1);
    }
    root.append(&grid);

    let custom_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Custom colour, e.g. #c678dd"));
    entry.add_css_class("nook-entry");
    entry.set_hexpand(true);
    entry.set_text(current.unwrap_or(""));
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("nook-action");
    custom_row.append(&entry);
    custom_row.append(&apply);
    root.append(&custom_row);

    let hint = gtk::Label::new(None);
    hint.add_css_class("nook-meta");
    hint.set_xalign(0.0);
    root.append(&hint);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let none = gtk::Button::with_label(none_label);
    none.add_css_class("nook-action");
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    buttons.append(&none);
    buttons.append(&cancel);
    root.append(&buttons);

    {
        let chosen = chosen.clone();
        let finish = finish.clone();
        let hint = hint.clone();
        let entry_for_apply = entry.clone();
        apply.connect_clicked(move |_| {
            match crate::icons::normalize_hex(&entry_for_apply.text()) {
                Some(color) => {
                    *chosen.borrow_mut() = Some(color);
                    finish();
                }
                None => hint.set_text("Enter a colour like #5294e2 or #59e."),
            }
        });
    }
    {
        let chosen = chosen.clone();
        let finish = finish.clone();
        entry.connect_activate(move |entry| {
            if let Some(color) = crate::icons::normalize_hex(&entry.text()) {
                *chosen.borrow_mut() = Some(color);
                finish();
            }
        });
    }
    {
        let chosen = chosen.clone();
        let finish = finish.clone();
        none.connect_clicked(move |_| {
            *chosen.borrow_mut() = None;
            finish();
        });
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }

    win.set_child(Some(&root));
    win.set_default_widget(Some(&apply));
    win.present();
}

/// Choose the icon theme used by Nook. `None` means automatic (Colloid when
/// installed, otherwise the desktop's theme).
pub fn icon_theme_picker<F>(
    parent: &impl IsA<gtk::Window>,
    current: Option<&str>,
    installed: &[String],
    auto_hint: &str,
    on_choice: F,
) where
    F: Fn(Option<String>) + 'static,
{
    let win = base_window(parent, "Icon Theme");
    win.set_default_size(460, 560);
    win.set_resizable(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("Icon Theme"));
    root.append(&body_label(
        "Only Nook changes; your desktop keeps its own icon theme.",
    ));

    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("Filter themes…"));
    filter.add_css_class("nook-search");
    root.append(&filter);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("nook-theme-list");

    let mut choices: Vec<Option<String>> = Vec::new();
    let mut selected_row: Option<gtk::ListBoxRow> = None;

    let auto_row = gtk::ListBoxRow::new();
    {
        let row_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
        row_box.set_margin_top(7);
        row_box.set_margin_bottom(7);
        row_box.set_margin_start(10);
        row_box.set_margin_end(10);
        let name = gtk::Label::new(Some("Automatic"));
        name.set_xalign(0.0);
        name.add_css_class("nook-file-name");
        let hint = gtk::Label::new(Some(auto_hint));
        hint.set_xalign(0.0);
        hint.add_css_class("nook-meta");
        row_box.append(&name);
        row_box.append(&hint);
        auto_row.set_child(Some(&row_box));
    }
    list.append(&auto_row);
    choices.push(None);
    if current.is_none() {
        selected_row = Some(auto_row);
    }

    for theme in installed {
        let row = gtk::ListBoxRow::new();
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row_box.set_margin_top(7);
        row_box.set_margin_bottom(7);
        row_box.set_margin_start(10);
        row_box.set_margin_end(10);
        let icon = gtk::Image::from_icon_name("folder");
        icon.set_pixel_size(20);
        let label = gtk::Label::new(Some(theme));
        label.set_xalign(0.0);
        label.add_css_class("nook-file-name");
        row_box.append(&icon);
        row_box.append(&label);
        row.set_child(Some(&row_box));
        list.append(&row);
        choices.push(Some(theme.clone()));
        if current == Some(theme.as_str()) {
            selected_row = Some(row);
        }
    }

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    scroller.add_css_class("nook-theme-scroller");
    root.append(&scroller);

    let choices = Rc::new(choices);

    if let Some(row) = selected_row.clone() {
        list.select_row(Some(&row));
        row.grab_focus();
    }

    // Filter rows by name; keep the first match selected for Enter to activate.
    {
        let list = list.clone();
        filter.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            let mut first_visible: Option<gtk::ListBoxRow> = None;
            let mut child = list.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                    let text = list_row_text(row).to_lowercase();
                    let visible = needle.is_empty() || text.contains(&needle);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row.clone());
                    }
                }
                child = widget.next_sibling();
            }
            if let Some(row) = first_visible {
                list.select_row(Some(&row));
            }
        });
    }

    let on_choice: Rc<dyn Fn(Option<String>)> = Rc::new(on_choice);
    {
        let win = win.clone();
        let choices = choices.clone();
        let on_choice = on_choice.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(choice) = choices.get(row.index() as usize).cloned() {
                on_choice(choice);
                win.close();
            }
        });
    }
    {
        // Enter in the filter applies the highlighted theme, so the picker is
        // fully keyboard driven: type, Down/Up, Enter.
        let list = list.clone();
        let win = win.clone();
        let choices = choices.clone();
        filter.connect_activate(move |_| {
            let Some(row) = list.selected_row() else {
                return;
            };
            if let Some(choice) = choices.get(row.index() as usize).cloned() {
                on_choice(choice);
                win.close();
            }
        });
    }
    let close = gtk::Button::with_label("Close");
    close.add_css_class("nook-action");
    close.set_halign(gtk::Align::End);
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    root.append(&close);

    win.set_child(Some(&root));
    win.present();
    filter.grab_focus();
}

/// Label text of a theme row, used by the filter.
fn list_row_text(row: &gtk::ListBoxRow) -> String {
    let mut out = String::new();
    fn collect(widget: &gtk::Widget, out: &mut String) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            out.push_str(label.text().as_str());
            out.push(' ');
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            collect(&widget, out);
            child = widget.next_sibling();
        }
    }
    if let Some(child) = row.child() {
        collect(&child, &mut out);
    }
    out
}

/// A searchable list of folders that share one colour, for quick jumping.
pub fn same_colour_folders<F>(
    parent: &impl IsA<gtk::Window>,
    root: &Path,
    color: &str,
    current: &Path,
    folders: &[PathBuf],
    on_pick: F,
) where
    F: Fn(PathBuf) + 'static,
{
    let win = base_window(parent, "Same-colour folders");
    win.set_default_size(560, 580);
    win.set_resizable(true);

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root_box.set_margin_top(18);
    root_box.set_margin_bottom(14);
    root_box.set_margin_start(18);
    root_box.set_margin_end(18);
    root_box.append(&title_label("Same-colour folders"));
    root_box.append(&body_label(&format!(
        "{} folder{} below {} share the colour {}. Type to filter, then press Enter to jump.",
        folders.len(),
        if folders.len() == 1 { "" } else { "s" },
        root.display(),
        color,
    )));

    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("Filter folders…"));
    filter.add_css_class("nook-search");
    root_box.append(&filter);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);

    let paths = Rc::new(folders.to_vec());
    for path in folders {
        let row = gtk::ListBoxRow::new();
        let row_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
        row_box.set_margin_top(6);
        row_box.set_margin_bottom(6);
        row_box.set_margin_start(10);
        row_box.set_margin_end(10);
        let relative = path.strip_prefix(root).unwrap_or(path);
        let name = gtk::Label::new(Some(&relative.display().to_string()));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.add_css_class("nook-file-name");
        let hint = gtk::Label::new(Some(&format!("Colour {color}")));
        hint.set_xalign(0.0);
        hint.add_css_class("nook-meta");
        row_box.append(&name);
        row_box.append(&hint);
        row.set_child(Some(&row_box));
        row.set_tooltip_text(Some(&path.display().to_string()));
        list.append(&row);
        if path == current {
            row.add_css_class("nook-current-folder");
        }
    }

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    root_box.append(&scroller);

    // Select the current folder when it is part of the list, else the first.
    let initial = folders
        .iter()
        .position(|path| path == current)
        .or(if folders.is_empty() { None } else { Some(0) });
    if let Some(index) = initial {
        let mut child = list.first_child();
        let mut i = 0u32;
        while let Some(widget) = child {
            if i as usize == index {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                    list.select_row(Some(row));
                    row.grab_focus();
                }
                break;
            }
            i += 1;
            child = widget.next_sibling();
        }
    }

    {
        let list = list.clone();
        filter.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            let mut first_visible: Option<gtk::ListBoxRow> = None;
            let mut child = list.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                    let visible =
                        needle.is_empty() || list_row_text(row).to_lowercase().contains(&needle);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row.clone());
                    }
                }
                child = widget.next_sibling();
            }
            if let Some(row) = first_visible {
                list.select_row(Some(&row));
            }
        });
    }

    let on_pick: Rc<dyn Fn(PathBuf)> = Rc::new(on_pick);
    {
        let win = win.clone();
        let paths = paths.clone();
        let on_pick = on_pick.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(path) = paths.get(row.index() as usize) {
                on_pick(path.clone());
                win.close();
            }
        });
    }
    {
        let list = list.clone();
        let win = win.clone();
        let paths = paths.clone();
        filter.connect_activate(move |_| {
            let Some(row) = list.selected_row() else {
                return;
            };
            if let Some(path) = paths.get(row.index() as usize) {
                on_pick(path.clone());
                win.close();
            }
        });
    }

    let close = gtk::Button::with_label("Close");
    close.add_css_class("nook-action");
    close.set_halign(gtk::Align::End);
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    root_box.append(&close);

    win.set_child(Some(&root_box));
    win.present();
    filter.grab_focus();
}

pub(crate) fn base_window(parent: &impl IsA<gtk::Window>, title: &str) -> gtk::Window {
    let win = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .resizable(false)
        .build();
    win.add_css_class("nook-dialog");
    win.set_default_size(430, -1);
    win
}

pub(crate) fn title_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("nook-empty-title");
    label
}

pub(crate) fn body_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("nook-empty-sub");
    label.set_selectable(true);
    // Selectable labels grab focus and select their whole text when a dialog
    // opens, which paints a bright selection wash behind the first paragraph.
    // Keep mouse selection available without making the label the initial
    // keyboard focus target.
    label.set_can_focus(false);
    label
}

pub fn error(parent: &impl IsA<gtk::Window>, title: &str, message: &str) {
    let win = base_window(parent, title);
    let ok = gtk::Button::with_label("Close");
    ok.add_css_class("nook-suggested");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(title));
    let body = body_label(message);
    body.set_max_width_chars(60);
    root.append(&body);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&ok);
    root.append(&buttons);
    win.set_child(Some(&root));

    let w = win.clone();
    ok.connect_clicked(move |_| w.close());
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape || k == gtk::gdk::Key::Return {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();
}

pub fn confirm<F>(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    message: &str,
    accept: &str,
    danger: bool,
    on_accept: F,
) where
    F: Fn() + 'static,
{
    let win = base_window(parent, title);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let ok = gtk::Button::with_label(accept);
    ok.add_css_class(if danger {
        "nook-destructive"
    } else {
        "nook-suggested"
    });

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(title));
    let body = body_label(message);
    body.set_max_width_chars(58);
    root.append(&body);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&ok);
    root.append(&buttons);
    win.set_child(Some(&root));

    let on_accept = Rc::new(on_accept);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let w = win.clone();
        ok.connect_clicked(move |_| {
            w.close();
            on_accept();
        });
    }
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();
}

pub fn text_prompt<F>(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    message: &str,
    initial: &str,
    accept: &str,
    on_accept: F,
) where
    F: Fn(String) + 'static,
{
    let win = base_window(parent, title);
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    entry.add_css_class("nook-entry");
    entry.set_activates_default(true);

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let ok = gtk::Button::with_label(accept);
    ok.add_css_class("nook-suggested");
    win.set_default_widget(Some(&ok));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(title));
    let body = body_label(message);
    body.set_max_width_chars(56);
    root.append(&body);
    root.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&ok);
    root.append(&buttons);
    win.set_child(Some(&root));

    let on_accept = Rc::new(on_accept);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    let submit = {
        let entry = entry.clone();
        let w = win.clone();
        let on_accept = on_accept.clone();
        move || {
            let text = entry.text().to_string();
            w.close();
            on_accept(text);
        }
    };
    {
        let submit = submit.clone();
        ok.connect_clicked(move |_| submit());
    }
    {
        let submit = submit.clone();
        entry.connect_activate(move |_| submit());
    }
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    entry.grab_focus();
    if initial.is_empty() {
        entry.select_region(0, -1);
    } else {
        // Select just the stem, keeping the extension, like most file managers.
        let stem_len = Path::new(initial)
            .file_stem()
            .map(|s| s.to_string_lossy().chars().count() as i32)
            .unwrap_or(-1);
        entry.select_region(0, stem_len);
    }
    win.present();
}

/* ------------------------------ Open With ------------------------------ */

pub fn open_with<F, P: IsA<gtk::Window>>(
    parent: &P,
    entry_name: &str,
    mime: &str,
    apps: Vec<gio::AppInfo>,
    current_default: Option<String>,
    on_pick: F,
) where
    F: Fn(&gio::AppInfo, bool) + 'static,
{
    let win = base_window(parent, "Open With");
    win.set_resizable(true);
    win.set_default_size(460, 520);

    let title = title_label(&format!("Open “{entry_name}” with"));
    let subtitle = body_label(&format!("File type: {mime}"));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search applications…"));
    search.add_css_class("nook-search");

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("nook-boxed-list");

    let set_default =
        gtk::CheckButton::with_label("Always use this application for this file type");
    set_default.set_active(true);
    let set_default_for = set_default.clone();

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(240)
        .child(&list)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let open = gtk::Button::with_label("Open");
    open.add_css_class("nook-suggested");
    open.set_sensitive(false);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&open);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title);
    root.append(&subtitle);
    root.append(&search);
    root.append(&scroller);
    root.append(&set_default);
    root.append(&buttons);
    win.set_child(Some(&root));

    let all_apps = apps;
    let chosen: Rc<RefCell<Option<gio::AppInfo>>> = Rc::new(RefCell::new(None));

    let apps_for_populate = all_apps.clone();
    let populate = {
        let list = list.clone();
        let chosen = chosen.clone();
        let open = open.clone();
        let current_default = current_default.clone();
        move |filter: &str| {
            let all_apps = apps_for_populate.clone();
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            *chosen.borrow_mut() = None;
            open.set_sensitive(false);
            let needle = filter.to_lowercase();
            let mut first_row: Option<gtk::ListBoxRow> = None;
            for app in all_apps.iter() {
                if !needle.is_empty() && !app.name().to_lowercase().contains(&needle) {
                    continue;
                }
                let row = gtk::ListBoxRow::new();
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                hbox.set_margin_top(7);
                hbox.set_margin_bottom(7);
                hbox.set_margin_start(8);
                hbox.set_margin_end(8);
                let image =
                    gtk::Image::from_gicon(&app.icon().unwrap_or_else(|| {
                        gio::ThemedIcon::new("application-x-executable").upcast()
                    }));
                image.set_pixel_size(22);
                let label = gtk::Label::new(Some(&app.name()));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                hbox.append(&image);
                hbox.append(&label);
                let is_default = current_default
                    .as_ref()
                    .map(|d| app.id().map(|i| i.as_str() == d).unwrap_or(false))
                    .unwrap_or(false);
                if is_default {
                    let badge = gtk::Label::new(Some("default"));
                    badge.add_css_class("nook-meta");
                    hbox.append(&badge);
                }
                row.set_child(Some(&hbox));
                list.append(&row);
                if first_row.is_none() {
                    first_row = Some(row);
                }
            }
            if let Some(row) = first_row {
                list.select_row(Some(&row));
            }
        }
    };
    populate("");

    {
        let list = list.clone();
        let chosen = chosen.clone();
        let open = open.clone();
        let all_apps = all_apps.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let idx = row.index();
                // Rows are filtered, so resolve by matching the label text.
                if idx >= 0 {
                    if let Some(child) = row.child() {
                        if let Some(hbox) = child.downcast::<gtk::Box>().ok() {
                            let mut c = hbox.first_child();
                            // Skip the image widget.
                            c = c.and_then(|w| w.next_sibling());
                            if let Some(label) = c.and_downcast::<gtk::Label>() {
                                let name = label.text().to_string();
                                if let Some(app) =
                                    all_apps.iter().find(|a| a.name().to_string() == name)
                                {
                                    *chosen.borrow_mut() = Some(app.clone());
                                    open.set_sensitive(true);
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            *chosen.borrow_mut() = None;
            open.set_sensitive(false);
        });
    }

    {
        let populate = populate.clone();
        search.connect_search_changed(move |s| populate(&s.text()));
    }

    let fire = {
        let win = win.clone();
        let chosen = chosen.clone();
        let on_pick = Rc::new(on_pick);
        let set_default_for = set_default_for.clone();
        move || {
            let app = chosen.borrow().clone();
            if let Some(app) = app {
                let always = set_default_for.is_active();
                win.close();
                on_pick(&app, always);
            }
        }
    };
    {
        let fire = fire.clone();
        open.connect_clicked(move |_| fire());
    }
    {
        let fire = fire.clone();
        list.connect_row_activated(move |_, _| fire());
    }
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();
    search.grab_focus();
}

/* ------------------------------ Properties ------------------------------ */

pub fn properties(parent: &impl IsA<gtk::Window>, entry: &FileEntry) {
    let win = base_window(parent, "Properties");
    win.set_resizable(true);
    win.set_default_size(480, -1);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(7);
    grid.set_column_spacing(16);
    grid.add_css_class("nook-props");

    let mut row = 0;
    let mut add = |key: &str, value: &str| {
        let k = gtk::Label::new(Some(key));
        k.add_css_class("nook-prop-key");
        k.set_xalign(0.0);
        k.set_valign(gtk::Align::Start);
        let v = gtk::Label::new(Some(value));
        v.add_css_class("nook-prop-value");
        v.set_xalign(0.0);
        v.set_wrap(true);
        v.set_selectable(true);
        v.set_hexpand(true);
        v.set_max_width_chars(48);
        grid.attach(&k, 0, row, 1, 1);
        grid.attach(&v, 1, row, 1, 1);
        row += 1;
    };

    add("Name", &entry.name);
    add(
        "Location",
        &entry
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    );
    add("Type", &fsutil::kind_label(entry));
    if !entry.mime.is_empty() {
        add("MIME type", &entry.mime);
    }
    add(
        "Size",
        &if entry.is_dir {
            "calculating…".to_string()
        } else {
            format!("{} ({} bytes)", fsutil::human_size(entry.size), entry.size)
        },
    );
    if entry.is_dir {
        add(
            "Contents",
            &format!("{} items", fsutil::count_items(&entry.path)),
        );
    }
    add("Modified", &fsutil::format_time(entry.modified));
    add(
        "Permissions",
        &format!(
            "{} ({:04o})",
            fsutil::mode_string(entry.mode, entry.is_dir),
            entry.mode & 0o7777
        ),
    );
    add("Owner (uid)", &format!("{}", entry.mode & 0o777));
    if entry.is_symlink {
        add(
            "Link target",
            &std::fs::read_link(&entry.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unreadable".into()),
        );
    }
    add("Path", &entry.path.display().to_string());

    let close = gtk::Button::with_label("Close");
    close.add_css_class("nook-suggested");
    close.set_halign(gtk::Align::End);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("Properties"));
    root.append(&grid);
    root.append(&close);
    win.set_child(Some(&root));

    let w = win.clone();
    close.connect_clicked(move |_| w.close());
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();

    // Directory sizes are computed in the background and patched in.
    if entry.is_dir {
        let path = entry.path.clone();
        let grid_weak = grid.downgrade();
        let (tx, rx) = async_channel::bounded::<u64>(1);
        std::thread::spawn(move || {
            let size = fsutil::total_size(&path);
            let _ = tx.send_blocking(size);
        });
        glib::spawn_future_local(async move {
            if let Ok(size) = rx.recv().await {
                if let Some(grid) = grid_weak.upgrade() {
                    if let Some(value) = grid.child_at(1, 4).and_downcast::<gtk::Label>() {
                        value.set_text(&format!(
                            "{} ({} bytes, recursively)",
                            fsutil::human_size(size),
                            size
                        ));
                    }
                }
            }
        });
    }
}

/* ------------------------------ Bulk rename ------------------------------ */

fn expand_pattern(pattern: &str, entry: &FileEntry, index: usize) -> String {
    let stem = Path::new(&entry.name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| entry.name.clone());
    let ext = Path::new(&entry.name)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = pattern
        .replace("{name}", &stem)
        .replace("{ext}", &ext)
        .replace("{n}", &index.to_string())
        .replace("{nn}", &format!("{index:02}"))
        .replace("{nnn}", &format!("{index:03}"));
    if out.is_empty() {
        out = entry.name.clone();
    }
    out
}

pub fn bulk_rename<F>(parent: &impl IsA<gtk::Window>, entries: &[FileEntry], on_apply: F)
where
    F: Fn(Vec<(PathBuf, String)>) + 'static,
{
    let win = base_window(parent, "Bulk Rename");
    win.set_resizable(true);
    win.set_default_size(520, 480);

    let entry = gtk::Entry::new();
    entry.set_text("{name}");
    entry.add_css_class("nook-entry");

    let hint = body_label("Tokens: {name}  {ext}  {n}  {nn}  {nnn}");
    hint.set_max_width_chars(60);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("nook-meta");

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(240)
        .child(&list)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let apply = gtk::Button::with_label("Rename");
    apply.add_css_class("nook-suggested");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&apply);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(&format!("Rename {} items", entries.len())));
    root.append(&gtk::Label::new(Some("Pattern")));
    root.append(&entry);
    root.append(&hint);
    root.append(&scroller);
    root.append(&status);
    root.append(&buttons);
    win.set_child(Some(&root));

    let entries: Rc<Vec<FileEntry>> = Rc::new(entries.to_vec());
    let preview = {
        let list = list.clone();
        let entries = entries.clone();
        let status = status.clone();
        let apply = apply.clone();
        move |pattern: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let mut names: Vec<String> = Vec::new();
            let mut invalid = 0usize;
            for (i, e) in entries.iter().enumerate() {
                let new_name = expand_pattern(pattern, e, i + 1);
                if new_name.contains('/') || new_name == "." || new_name == ".." {
                    invalid += 1;
                }
                if names.contains(&new_name) {
                    invalid += 1;
                }
                names.push(new_name.clone());
                let row = gtk::ListBoxRow::new();
                row.set_activatable(false);
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                hbox.set_margin_top(4);
                hbox.set_margin_bottom(4);
                hbox.set_margin_start(6);
                hbox.set_margin_end(6);
                let old = gtk::Label::new(Some(&e.name));
                old.add_css_class("nook-meta");
                old.set_xalign(0.0);
                old.set_hexpand(true);
                old.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                let arrow = gtk::Label::new(Some("→"));
                arrow.add_css_class("nook-meta");
                let new = gtk::Label::new(Some(&new_name));
                new.set_xalign(0.0);
                new.set_hexpand(true);
                new.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                if new_name == e.name {
                    new.add_css_class("nook-meta");
                }
                hbox.append(&old);
                hbox.append(&arrow);
                hbox.append(&new);
                row.set_child(Some(&hbox));
                list.append(&row);
            }
            if invalid > 0 {
                status.set_text(&format!(
                    "{invalid} name(s) are duplicated or invalid — fix the pattern to continue"
                ));
                apply.set_sensitive(false);
            } else {
                status.set_text(&format!("{} files will be renamed", entries.len()));
                apply.set_sensitive(true);
            }
        }
    };
    preview("{name}");
    {
        let preview = preview.clone();
        entry.connect_changed(move |e| preview(&e.text()));
    }

    let on_apply = Rc::new(on_apply);
    {
        let w = win.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    {
        let entry = entry.clone();
        let entries = entries.clone();
        let w = win.clone();
        let on_apply = on_apply.clone();
        apply.connect_clicked(move |_| {
            let pattern = entry.text().to_string();
            let pairs: Vec<(PathBuf, String)> = entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let new_name = expand_pattern(&pattern, e, i + 1);
                    if new_name == e.name || new_name.is_empty() {
                        None
                    } else {
                        Some((e.path.clone(), new_name))
                    }
                })
                .collect();
            w.close();
            if !pairs.is_empty() {
                on_apply(pairs);
            }
        });
    }
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();
    entry.grab_focus();
}

/* ------------------------------ Checksums ------------------------------ */

pub fn checksums(parent: &impl IsA<gtk::Window>, entries: &[FileEntry]) {
    let win = base_window(parent, "Checksums");
    win.set_resizable(true);
    win.set_default_size(560, 420);

    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_monospace(true);
    text.set_top_margin(8);
    text.set_bottom_margin(8);
    text.set_left_margin(8);
    text.set_right_margin(8);
    text.buffer().set_text("Computing SHA-256…");

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&text)
        .build();
    scroller.add_css_class("nook-surface-bare");

    let copy = gtk::Button::with_label("Copy");
    copy.add_css_class("nook-action");
    let close = gtk::Button::with_label("Close");
    close.add_css_class("nook-suggested");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&copy);
    buttons.append(&close);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("SHA-256 checksums"));
    root.append(&scroller);
    root.append(&buttons);
    win.set_child(Some(&root));

    let paths: Vec<(String, PathBuf)> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| (e.name.clone(), e.path.clone()))
        .collect();
    let (tx, rx) = async_channel::bounded::<String>(1);
    std::thread::spawn(move || {
        use sha2::{Digest, Sha256};
        let mut out = String::new();
        for (name, path) in paths {
            match std::fs::File::open(&path) {
                Ok(file) => {
                    let mut hasher = Sha256::new();
                    let mut reader = std::io::BufReader::with_capacity(128 * 1024, file);
                    if std::io::copy(&mut reader, &mut hasher).is_ok() {
                        out.push_str(&format!("{}  {}\n", hex(&hasher.finalize()), name));
                    } else {
                        out.push_str(&format!("<read error>  {name}\n"));
                    }
                }
                Err(err) => out.push_str(&format!("<{err}>  {name}\n")),
            }
        }
        let _ = tx.send_blocking(out);
    });
    let buffer = text.buffer();
    glib::spawn_future_local(async move {
        if let Ok(result) = rx.recv().await {
            buffer.set_text(&result);
        }
    });

    {
        let buffer = text.buffer();
        copy.connect_clicked(move |_| {
            let (start, end) = buffer.bounds();
            if let Some(display) = gtk::gdk::Display::default() {
                display
                    .clipboard()
                    .set_text(&buffer.text(&start, &end, false));
            }
        });
    }
    let w = win.clone();
    close.connect_clicked(move |_| w.close());
    let w = win.clone();
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            w.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
    win.present();
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/* ------------------------------ Conflicts ------------------------------ */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConflictChoice {
    Replace,
    Skip,
    KeepBoth,
}

impl ConflictChoice {
    pub fn to_fs(self) -> fsutil::Conflict {
        match self {
            ConflictChoice::Replace => fsutil::Conflict::Replace,
            ConflictChoice::Skip => fsutil::Conflict::Skip,
            ConflictChoice::KeepBoth => fsutil::Conflict::KeepBoth,
        }
    }
}

/// Ask how to resolve one or more name collisions before an operation starts.
/// The decision applies to every conflict in the batch, which keeps bulk moves
/// predictable and avoids a dialog storm.
pub fn conflict_prompt(
    parent: &impl IsA<gtk::Window>,
    dest: &Path,
    count: usize,
    on_choice: impl Fn(ConflictChoice) + 'static,
) {
    let win = base_window(parent, "Items already exist");
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    if count <= 1 {
        root.append(&title_label(&format!("“{name}” already exists")));
    } else {
        root.append(&title_label(&format!("{count} items already exist")));
    }
    let body = body_label(&format!(
        "Choose what to do with the existing items in\n{}",
        dest.parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    ));
    body.set_max_width_chars(56);
    root.append(&body);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let skip = gtk::Button::with_label("Skip");
    skip.add_css_class("nook-action");
    let keep = gtk::Button::with_label("Keep both");
    keep.add_css_class("nook-action");
    let replace = gtk::Button::with_label("Replace");
    replace.add_css_class("nook-destructive");
    buttons.append(&skip);
    buttons.append(&keep);
    buttons.append(&replace);
    root.append(&buttons);
    win.set_child(Some(&root));

    let on_choice = Rc::new(RefCell::new(Some(on_choice)));
    let respond = {
        let win = win.clone();
        let on_choice = on_choice.clone();
        move |choice: ConflictChoice| {
            if let Some(cb) = on_choice.borrow_mut().take() {
                cb(choice);
            }
            win.close();
        }
    };
    {
        let respond = respond.clone();
        skip.connect_clicked(move |_| respond(ConflictChoice::Skip));
    }
    {
        let respond = respond.clone();
        keep.connect_clicked(move |_| respond(ConflictChoice::KeepBoth));
    }
    {
        let respond = respond.clone();
        replace.connect_clicked(move |_| respond(ConflictChoice::Replace));
    }
    let resp = respond.clone();
    win.connect_close_request(move |_| {
        resp(ConflictChoice::Skip);
        glib::Propagation::Proceed
    });
    win.present();
}

/* ------------------------------ Progress ------------------------------ */

pub enum ProgressMsg {
    Message(String),
    Fraction(f64),
    Done,
}

pub struct ProgressDialog {
    win: gtk::Window,
    pub tx: async_channel::Sender<ProgressMsg>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ProgressDialog {
    pub fn new(parent: &impl IsA<gtk::Window>, title: &str) -> Rc<Self> {
        let win = base_window(parent, title);
        win.set_resizable(false);
        win.set_default_size(400, -1);
        win.set_deletable(false);

        let label = gtk::Label::new(Some("Working…"));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.add_css_class("nook-meta");
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("nook-progress");
        bar.set_show_text(false);

        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("nook-action");
        cancel.set_halign(gtk::Align::End);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(18);
        root.set_margin_bottom(14);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.append(&title_label(title));
        root.append(&label);
        root.append(&bar);
        root.append(&cancel);
        win.set_child(Some(&root));

        let (tx, rx) = async_channel::unbounded::<ProgressMsg>();
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let cancelled = cancelled.clone();
            let bar = bar.clone();
            let label = label.clone();
            cancel.connect_clicked(move |_| {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                bar.set_text(Some("Cancelling…"));
                label.set_text("Cancelling after the current item…");
            });
        }

        let win_for_loop = win.clone();
        let label_loop = label.clone();
        let bar_loop = bar.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    ProgressMsg::Message(text) => label_loop.set_text(&text),
                    ProgressMsg::Fraction(f) => bar_loop.set_fraction(f.clamp(0.0, 1.0)),
                    ProgressMsg::Done => {
                        win_for_loop.close();
                        break;
                    }
                }
            }
        });

        let dialog = Rc::new(Self { win, tx, cancelled });
        dialog.win.present();
        dialog
    }

    pub fn message(&self, text: impl Into<String>) {
        let _ = self.tx.try_send(ProgressMsg::Message(text.into()));
    }

    pub fn finish(&self) {
        let _ = self.tx.try_send(ProgressMsg::Done);
    }

    /// A `Send` flag the worker can poll to stop early.
    pub fn cancel_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.cancelled.clone()
    }
}

/* ------------------------------ Shortcuts ------------------------------ */

pub fn shortcuts<F>(
    parent: &impl IsA<gtk::Window>,
    actions: &[(&str, &str)],
    config: &Config,
    on_save: F,
) where
    F: Fn(Vec<(String, String)>) + 'static,
{
    let win = base_window(parent, "Keyboard Shortcuts");
    win.set_resizable(true);
    win.set_default_size(520, 560);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    grid.set_margin_top(4);
    grid.set_margin_bottom(4);
    grid.set_margin_start(4);
    grid.set_margin_end(4);

    let entries: Rc<RefCell<Vec<(String, gtk::Entry)>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, (action, label)) in actions.iter().enumerate() {
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        let e = gtk::Entry::new();
        e.set_text(&config.shortcut(action, ""));
        e.set_placeholder_text(Some("unset"));
        e.set_width_chars(14);
        e.add_css_class("nook-entry");
        grid.attach(&l, 0, i as i32, 1, 1);
        grid.attach(&e, 1, i as i32, 1, 1);
        entries.borrow_mut().push((action.to_string(), e));
    }

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&grid)
        .build();

    let hint = body_label(
        "Use GTK accelerator syntax, e.g. <Control>h or <Alt>F2. Leave empty to disable.",
    );
    hint.set_max_width_chars(60);

    let reset = gtk::Button::with_label("Reset to defaults");
    reset.add_css_class("nook-action");
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("nook-suggested");

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&reset);
    buttons.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
    buttons.set_hexpand(true);
    buttons.append(&cancel);
    buttons.append(&save);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("Keyboard shortcuts"));
    root.append(&hint);
    root.append(&scroller);
    root.append(&buttons);
    win.set_child(Some(&root));

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let entries = entries.clone();
        reset.connect_clicked(move |_| {
            for (_, e) in entries.borrow().iter() {
                e.set_text("");
            }
        });
    }
    {
        let entries = entries.clone();
        let win = win.clone();
        let on_save = Rc::new(on_save);
        save.connect_clicked(move |_| {
            let pairs: Vec<(String, String)> = entries
                .borrow()
                .iter()
                .map(|(a, e)| (a.clone(), e.text().to_string()))
                .collect();
            win.close();
            on_save(pairs);
        });
    }
    win.present();
}

/* ------------------------ Default applications ------------------------ */

/// Configure how Nook opens file types and individual paths.
///
/// Two rule kinds are supported and stored in `config.json`:
/// * an application rule (`app_overrides`) keyed by extension, MIME type or
///   absolute path;
/// * a shell command rule (`custom_openers`) with the same keys, where `%f`
///   expands to the selected file.
pub fn default_apps<F>(parent: &impl IsA<gtk::Window>, config: &Config, on_save: F)
where
    F: Fn(std::collections::HashMap<String, String>, std::collections::HashMap<String, String>)
        + 'static,
{
    let win = base_window(parent, "Default Applications");
    win.set_resizable(true);
    win.set_default_size(660, 620);

    let overrides = Rc::new(RefCell::new(config.app_overrides.clone()));
    let openers = Rc::new(RefCell::new(config.custom_openers.clone()));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("Default applications"));
    root.append(&body_label(
        "Rules apply to Nook only. Keys accept an extension (.md), a MIME type \
         (text/markdown) or an absolute file path.",
    ));

    let rules = gtk::ListBox::new();
    rules.set_selection_mode(gtk::SelectionMode::None);
    rules.add_css_class("nook-theme-list");
    let rules_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&rules)
        .build();
    root.append(&rules_scroller);

    // Applications are resolved once; the drop-down index maps back into this
    // list, so duplicate display names never pick the wrong desktop file.
    let mut app_list: Vec<(String, String)> = gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show())
        .filter_map(|app| app.id().map(|id| (app.name().to_string(), id.to_string())))
        .collect();
    app_list.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    app_list.dedup_by(|a, b| a.1 == b.1);
    let app_list = Rc::new(app_list);
    let mut app_names: Vec<String> = vec!["Custom command…".to_string()];
    app_names.extend(app_list.iter().map(|(name, _)| name.clone()));
    let app_names_refs: Vec<&str> = app_names.iter().map(String::as_str).collect();
    let app_dropdown = gtk::DropDown::from_strings(&app_names_refs);
    app_dropdown.add_css_class("nook-search-mode");
    app_dropdown.set_hexpand(true);

    let add_grid = gtk::Grid::new();
    add_grid.set_row_spacing(6);
    add_grid.set_column_spacing(6);
    let key_entry = gtk::Entry::new();
    key_entry.set_placeholder_text(Some(".md, text/markdown or /full/path"));
    key_entry.add_css_class("nook-entry");
    key_entry.set_hexpand(true);
    let command_entry = gtk::Entry::new();
    command_entry.set_placeholder_text(Some("Command, e.g. gimp %f"));
    command_entry.add_css_class("nook-entry");
    command_entry.set_hexpand(true);
    let add = gtk::Button::with_label("Add rule");
    add.add_css_class("nook-suggested");
    let key_label = gtk::Label::new(Some("Match"));
    key_label.set_xalign(0.0);
    key_label.add_css_class("nook-meta");
    let app_label = gtk::Label::new(Some("Application"));
    app_label.set_xalign(0.0);
    app_label.add_css_class("nook-meta");
    let command_label = gtk::Label::new(Some("or command"));
    command_label.set_xalign(0.0);
    command_label.add_css_class("nook-meta");
    add_grid.attach(&key_label, 0, 0, 1, 1);
    add_grid.attach(&key_entry, 1, 0, 1, 1);
    add_grid.attach(&app_label, 0, 1, 1, 1);
    add_grid.attach(&app_dropdown, 1, 1, 1, 1);
    add_grid.attach(&command_label, 0, 2, 1, 1);
    add_grid.attach(&command_entry, 1, 2, 1, 1);
    add_grid.attach(&add, 2, 2, 1, 1);
    root.append(&add_grid);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("nook-meta");
    root.append(&status);

    let holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let rebuild: Rc<dyn Fn()> = {
        let rules = rules.clone();
        let overrides = overrides.clone();
        let openers = openers.clone();
        let holder = holder.clone();
        Rc::new(move || {
            while let Some(child) = rules.first_child() {
                rules.remove(&child);
            }
            let mut keys: Vec<String> = overrides
                .borrow()
                .keys()
                .chain(openers.borrow().keys())
                .cloned()
                .collect();
            keys.sort();
            keys.dedup();
            if keys.is_empty() {
                let empty_row = gtk::ListBoxRow::new();
                empty_row.set_activatable(false);
                let empty = gtk::Label::new(Some("No rules yet — every file uses the system default."));
                empty.add_css_class("nook-meta");
                empty.set_xalign(0.0);
                empty.set_margin_top(12);
                empty.set_margin_bottom(12);
                empty.set_margin_start(8);
                empty_row.set_child(Some(&empty));
                rules.append(&empty_row);
                return;
            }
            for key in keys {
                let row = gtk::ListBoxRow::new();
                row.set_activatable(false);
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                hbox.set_margin_top(6);
                hbox.set_margin_bottom(6);
                hbox.set_margin_start(8);
                hbox.set_margin_end(8);
                let key_label = gtk::Label::new(Some(&key));
                key_label.set_xalign(0.0);
                key_label.add_css_class("nook-file-name");
                key_label.set_width_chars(18);
                key_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                let handler = if let Some(command) = openers.borrow().get(&key) {
                    format!("Command · {command}")
                } else {
                    overrides
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| "—".into())
                };
                let handler_label = gtk::Label::new(Some(&handler));
                handler_label.set_xalign(0.0);
                handler_label.set_hexpand(true);
                handler_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                handler_label.add_css_class("nook-meta");
                let remove = gtk::Button::from_icon_name("user-trash-symbolic");
                remove.add_css_class("nook-btn");
                remove.set_tooltip_text(Some("Remove rule"));
                let overrides = overrides.clone();
                let openers = openers.clone();
                let holder = holder.clone();
                let key_for_remove = key.clone();
                remove.connect_clicked(move |_| {
                    overrides.borrow_mut().remove(&key_for_remove);
                    openers.borrow_mut().remove(&key_for_remove);
                    if let Some(rebuild) = holder.borrow().as_ref() {
                        rebuild();
                    }
                });
                hbox.append(&key_label);
                hbox.append(&handler_label);
                hbox.append(&remove);
                row.set_child(Some(&hbox));
                rules.append(&row);
            }
        })
    };
    *holder.borrow_mut() = Some(rebuild.clone());
    rebuild();

    {
        let key_entry = key_entry.clone();
        let command_entry = command_entry.clone();
        let app_dropdown = app_dropdown.clone();
        let app_list = app_list.clone();
        let overrides = overrides.clone();
        let openers = openers.clone();
        let status = status.clone();
        let rebuild = rebuild.clone();
        add.connect_clicked(move |_| {
            let raw = key_entry.text().trim().to_lowercase();
            if raw.is_empty() {
                status.set_text("Enter an extension, MIME type or absolute path first.");
                return;
            }
            let key = if raw.starts_with('/') || raw.contains('/') {
                raw
            } else if raw.starts_with('.') {
                raw
            } else {
                format!(".{raw}")
            };
            let command = command_entry.text().trim().to_string();
            if !command.is_empty() {
                openers.borrow_mut().insert(key.clone(), command);
                overrides.borrow_mut().remove(&key);
                status.set_text(&format!("Command rule saved for {key}"));
            } else {
                let selected = app_dropdown.selected() as usize;
                if selected == 0 {
                    status.set_text("Choose an application or enter a command.");
                    return;
                }
                if let Some((name, id)) = app_list.get(selected - 1) {
                    overrides.borrow_mut().insert(key.clone(), id.clone());
                    openers.borrow_mut().remove(&key);
                    status.set_text(&format!("{name} will open {key} files"));
                }
            }
            key_entry.set_text("");
            command_entry.set_text("");
            rebuild();
        });
    }
    {
        let command_entry = command_entry.clone();
        let app_dropdown = app_dropdown.clone();
        command_entry.connect_changed(move |entry| {
            if !entry.text().trim().is_empty() {
                app_dropdown.set_selected(0);
            }
        });
    }

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let save = gtk::Button::with_label("Save rules");
    save.add_css_class("nook-suggested");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&save);
    root.append(&buttons);

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        let overrides = overrides.clone();
        let openers = openers.clone();
        let on_save = Rc::new(on_save);
        save.connect_clicked(move |_| {
            let overrides = overrides.borrow().clone();
            let openers = openers.borrow().clone();
            win.close();
            on_save(overrides, openers);
        });
    }

    win.set_child(Some(&root));
    win.present();
    key_entry.grab_focus();
}

/* --------------------------- Preview icons --------------------------- */

/// Toggle preview-based icons (real thumbnails) per file category.
pub fn preview_icons<F>(parent: &impl IsA<gtk::Window>, config: &Config, on_save: F)
where
    F: Fn(bool, bool, bool) + 'static,
{
    let win = base_window(parent, "Preview Icons");
    win.set_resizable(false);
    win.set_default_size(520, -1);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label("Preview-based icons"));
    root.append(&body_label(
        "Show real thumbnails instead of generic file icons. Videos and PDFs \
         render their first frame through ffmpeg and poppler; the first open \
         generates a thumbnail that is then cached.",
    ));

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 10);
    rows.set_margin_top(6);

    let build_row = |title: &str, hint: &str, on: bool| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("nook-file");
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(6);
        row.set_margin_end(6);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let name = gtk::Label::new(Some(title));
        name.set_xalign(0.0);
        name.add_css_class("nook-file-name");
        let sub = gtk::Label::new(Some(hint));
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        sub.add_css_class("nook-meta");
        text.append(&name);
        text.append(&sub);
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        toggle.set_active(on);
        row.append(&text);
        row.append(&toggle);
        (row, toggle)
    };

    let (images_row, images) = build_row(
        "Images",
        "JPG, PNG, GIF, WebP, SVG and other pictures",
        config.thumbs_images,
    );
    let (videos_row, videos) = build_row(
        "Videos",
        "MP4, MKV, WebM, MOV… rendered with ffmpeg",
        config.thumbs_videos,
    );
    let (pdfs_row, pdfs) = build_row("PDF", "First page rendered with poppler", config.thumbs_pdfs);
    rows.append(&images_row);
    rows.append(&videos_row);
    rows.append(&pdfs_row);
    root.append(&rows);

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("nook-suggested");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&save);
    root.append(&buttons);

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        let on_save = Rc::new(on_save);
        save.connect_clicked(move |_| {
            let flags = (images.is_active(), videos.is_active(), pdfs.is_active());
            win.close();
            on_save(flags.0, flags.1, flags.2);
        });
    }

    win.set_child(Some(&root));
    win.present();
}

/* --------------------------- Folder commands --------------------------- */

fn add_run_row(
    list: &gtk::Box,
    rows: &Rc<RefCell<Vec<(gtk::Box, gtk::Entry, gtk::Entry)>>>,
    name: &str,
    command: &str,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_top(3);
    row.set_margin_bottom(3);
    let name_entry = gtk::Entry::new();
    name_entry.set_text(name);
    name_entry.set_placeholder_text(Some("Name"));
    name_entry.add_css_class("nook-entry");
    name_entry.set_width_chars(14);
    let command_entry = gtk::Entry::new();
    command_entry.set_text(command);
    command_entry.set_placeholder_text(Some("Command, e.g. npm run dev"));
    command_entry.add_css_class("nook-entry");
    command_entry.set_hexpand(true);
    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("nook-btn");
    remove.set_tooltip_text(Some("Remove command"));
    row.append(&name_entry);
    row.append(&command_entry);
    row.append(&remove);
    list.append(&row);

    rows.borrow_mut().push((row.clone(), name_entry, command_entry));
    let list = list.clone();
    let rows = rows.clone();
    remove.connect_clicked(move |_| {
        list.remove(&row);
        rows.borrow_mut().retain(|(widget, _, _)| widget != &row);
    });
}

/// Edit the run commands configured for one folder.
pub fn run_commands_editor<F>(
    parent: &impl IsA<gtk::Window>,
    folder: &Path,
    commands: &[RunCommand],
    file: &Path,
    on_save: F,
) where
    F: Fn(Vec<RunCommand>) + 'static,
{
    let win = base_window(parent, "Run Commands");
    win.set_resizable(true);
    win.set_default_size(640, 460);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(&format!(
        "Run commands · {}",
        folder
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| folder.display().to_string())
    )));
    root.append(&body_label(
        "Commands start in a terminal in this folder. Useful for dev servers, tests \
         and build scripts.",
    ));
    root.append(&body_label(&format!(
        "Saved to {}.\n\nNook only runs a command when you pick it from the Run menu, \
         and asks before starting commands that can delete or modify data.",
        file.display()
    )));

    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let rows: Rc<RefCell<Vec<(gtk::Box, gtk::Entry, gtk::Entry)>>> =
        Rc::new(RefCell::new(Vec::new()));
    for command in commands {
        add_run_row(&list, &rows, &command.name, &command.command);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(220)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    root.append(&scroller);

    let add = gtk::Button::with_label("Add command");
    add.add_css_class("nook-action");
    add.set_halign(gtk::Align::Start);
    {
        let list = list.clone();
        let rows = rows.clone();
        add.connect_clicked(move |_| {
            add_run_row(&list, &rows, "", "");
        });
    }
    root.append(&add);

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("nook-suggested");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&save);
    root.append(&buttons);

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let win = win.clone();
        let rows = rows.clone();
        let on_save = Rc::new(on_save);
        save.connect_clicked(move |_| {
            let commands: Vec<RunCommand> = rows
                .borrow()
                .iter()
                .filter_map(|(_, name, command)| {
                    let command = command.text().trim().to_string();
                    if command.is_empty() {
                        return None;
                    }
                    let name = name.text().trim().to_string();
                    Some(RunCommand {
                        name: if name.is_empty() {
                            command.clone()
                        } else {
                            name
                        },
                        command,
                    })
                })
                .collect();
            win.close();
            on_save(commands);
        });
    }

    win.set_child(Some(&root));
    win.present();
}

/// Pick one of a folder's configured commands to run.
pub fn run_command_picker<Pick, Edit>(
    parent: &impl IsA<gtk::Window>,
    folder: &Path,
    commands: &[RunCommand],
    note: &str,
    on_pick: Pick,
    on_edit: Edit,
) where
    Pick: Fn(String) + 'static,
    Edit: Fn() + 'static,
{
    let win = base_window(parent, "Run Commands");
    win.set_resizable(true);
    win.set_default_size(540, 420);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&title_label(&format!(
        "Run in {}",
        folder
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| folder.display().to_string())
    )));
    root.append(&body_label(&format!(
        "{note}\n\nReview the command before running it; Nook asks for confirmation \
         when it can delete or overwrite data."
    )));

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("nook-theme-list");
    for command in commands {
        let row = gtk::ListBoxRow::new();
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
        vbox.set_margin_top(7);
        vbox.set_margin_bottom(7);
        vbox.set_margin_start(10);
        vbox.set_margin_end(10);
        let name = gtk::Label::new(Some(&command.name));
        name.set_xalign(0.0);
        name.add_css_class("nook-file-name");
        let cmd = gtk::Label::new(Some(&command.command));
        cmd.set_xalign(0.0);
        cmd.add_css_class("nook-meta");
        cmd.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        vbox.append(&name);
        vbox.append(&cmd);
        row.set_child(Some(&vbox));
        list.append(&row);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroller);

    let edit = gtk::Button::with_label("Edit commands…");
    edit.add_css_class("nook-action");
    let close = gtk::Button::with_label("Close");
    close.add_css_class("nook-action");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&edit);
    buttons.append(&close);
    root.append(&buttons);

    let commands = Rc::new(commands.to_vec());
    let on_pick: Rc<dyn Fn(String)> = Rc::new(on_pick);
    {
        let win = win.clone();
        let on_edit: Rc<dyn Fn()> = Rc::new(on_edit);
        edit.connect_clicked(move |_| {
            win.close();
            on_edit();
        });
    }
    {
        let win = win.clone();
        let commands = commands.clone();
        let on_pick = on_pick.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(command) = commands.get(row.index() as usize) {
                on_pick(command.command.clone());
                win.close();
            }
        });
    }
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }

    win.set_child(Some(&root));
    win.present();
    if let Some(first) = list.first_child() {
        if let Some(row) = first.downcast_ref::<gtk::ListBoxRow>() {
            list.select_row(Some(row));
            row.grab_focus();
        }
    }
}

/// Small helper used by the sidebar and Quick Shift for elided paths.
pub fn pretty_path(path: &Path) -> String {
    let home = crate::settings::home_dir();
    if let Ok(rest) = path.strip_prefix(&home) {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}
