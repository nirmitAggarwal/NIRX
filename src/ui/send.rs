//! LocalSend in Nook: pick a device, watch the progress, accept what arrives,
//! and configure the receiver — all in Nook's own windows.
//!
//! The protocol work lives in [`crate::localsend`]; this module is only the
//! interface. Every dialog is keyboard-first: a search box for devices, arrow
//! keys to move, Enter to accept and Escape to back out.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::fsutil;
use crate::localsend::{self, Device, Incoming, Progress};
use crate::ui::dialogs::{self, ProgressDialog, ProgressMsg};
use crate::ui::AppUi;

/* --------------------------------- send ---------------------------------- */

/// Send the current selection: opens the device picker.
pub fn send_selection(ui: &Rc<AppUi>) {
    if let Some(tab) = ui.active_tab() {
        if tab.library.get().is_some() {
            ui.toast("Leave this view with Esc to send files from the folder");
            return;
        }
    }
    let Some(pane) = ui.active_pane() else {
        return;
    };
    let paths: Vec<PathBuf> = pane
        .selected_entries()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    if paths.is_empty() {
        ui.toast("Select the files or folders to send first");
        return;
    }
    let Some(service) = ui.transfer_service() else {
        dialogs::error(
            &ui.window,
            "LocalSend is unavailable",
            "The transfer service could not start. Another program may already use the \
             LocalSend port; close it and restart Nook, or change the port in \
             LocalSend settings.",
        );
        return;
    };
    show_picker(ui, service, paths);
}

fn show_picker(ui: &Rc<AppUi>, service: localsend::Service, paths: Vec<PathBuf>) {
    let total: u64 = paths.iter().map(|path| fsutil::total_size(path)).sum();
    let win = dialogs::base_window(&ui.window, "Send with LocalSend");
    win.set_default_size(560, 560);
    win.set_resizable(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(16);
    root.set_margin_bottom(12);
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.append(&dialogs::title_label(&format!(
        "Send {} item{} · {}",
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        fsutil::human_size(total)
    )));
    root.append(&dialogs::body_label(
        "Choose a device. Both devices must be on the same network, with LocalSend, \
         LocalSend-compatible software, or Nook running.",
    ));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filter devices…"));
    search.add_css_class("nook-search");
    root.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.add_css_class("nook-library");
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&list)
        .build();
    scroller.add_css_class("nook-surface-bare");
    root.append(&scroller);

    let manual = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let address = gtk::Entry::new();
    address.set_placeholder_text(Some("Add by address (192.168.1.20 or host:port)"));
    address.add_css_class("nook-entry");
    address.set_hexpand(true);
    let add = gtk::Button::with_label("Add");
    add.add_css_class("nook-action");
    manual.append(&address);
    manual.append(&add);
    root.append(&manual);

    let pin_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let pin_label = gtk::Label::new(Some("PIN"));
    pin_label.add_css_class("nook-meta");
    let pin = gtk::Entry::new();
    pin.set_placeholder_text(Some("Only if the device asks for one"));
    pin.add_css_class("nook-entry");
    pin.set_hexpand(true);
    pin.set_input_purpose(gtk::InputPurpose::Digits);
    pin_row.append(&pin_label);
    pin_row.append(&pin);
    root.append(&pin_row);

    let hint = gtk::Label::new(Some(
        "↑↓ to choose  ·  Enter to send  ·  Ctrl+R rescans  ·  Esc to cancel",
    ));
    hint.add_css_class("nook-meta");
    hint.set_xalign(0.0);
    root.append(&hint);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let scan = gtk::Button::with_label("Scan");
    scan.add_css_class("nook-action");
    actions.append(&scan);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    actions.append(&cancel);
    let send = gtk::Button::with_label("Send");
    send.add_css_class("nook-suggested");
    send.set_sensitive(false);
    actions.append(&send);
    root.append(&actions);

    win.set_child(Some(&root));

    // Devices found so far, in list order, so the send action can look up the
    // chosen one without re-parsing row labels.
    let rows: Rc<RefCell<Vec<(gtk::ListBoxRow, Device)>>> = Rc::new(RefCell::new(Vec::new()));
    let chosen: Rc<RefCell<Option<Device>>> = Rc::new(RefCell::new(None));

    let reference = Rc::new(RefCell::new(None::<String>));
    let rebuild: Rc<dyn Fn()> = {
        let list = list.clone();
        let search = search.clone();
        let rows = rows.clone();
        let chosen = chosen.clone();
        let reference = reference.clone();
        let service = service.clone();
        Rc::new(move || {
            let needle = search.text().to_lowercase();
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            rows.borrow_mut().clear();
            for device in service.peers() {
                if !needle.is_empty() && !device.label().to_lowercase().contains(&needle) {
                    continue;
                }
                let row = gtk::ListBoxRow::new();
                let line = gtk::Box::new(gtk::Orientation::Vertical, 2);
                line.set_margin_top(8);
                line.set_margin_bottom(8);
                line.set_margin_start(10);
                line.set_margin_end(10);
                let name = gtk::Label::new(Some(&device.label()));
                name.set_xalign(0.0);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name.add_css_class("nook-file-name");
                let meta = gtk::Label::new(Some(&format!(
                    "{} · {} · {} · v{}",
                    device.kind(),
                    device.address(),
                    device.protocol.to_uppercase(),
                    device.version
                )));
                meta.set_xalign(0.0);
                meta.add_css_class("nook-meta");
                line.append(&name);
                line.append(&meta);
                row.set_child(Some(&line));
                row.set_tooltip_text(Some(&format!(
                    "{}\nFingerprint {}",
                    device.label(),
                    &device.fingerprint[..device.fingerprint.len().min(32)]
                )));
                list.append(&row);
                rows.borrow_mut().push((row, device));
            }
            // Restore the previous selection when that device is still around.
            let want = reference.borrow().clone();
            let mut restored = false;
            for (row, device) in rows.borrow().iter() {
                if Some(device.fingerprint.clone()) == want && !device.fingerprint.is_empty() {
                    list.select_row(Some(row));
                    restored = true;
                    break;
                }
            }
            if !restored {
                *chosen.borrow_mut() = None;
            }
        })
    };

    // Refresh while the dialog is open, so a device that appears a second later
    // still shows up without pressing anything.
    {
        let rebuild = rebuild.clone();
        let win = win.clone();
        glib::timeout_add_local(Duration::from_millis(900), move || {
            if !win.is_visible() {
                return glib::ControlFlow::Break;
            }
            rebuild();
            glib::ControlFlow::Continue
        });
    }
    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |_| rebuild());
    }
    {
        let rows = rows.clone();
        let chosen = chosen.clone();
        let reference = reference.clone();
        let send = send.clone();
        list.connect_row_selected(move |_, row| {
            let device = row.and_then(|row| {
                rows.borrow()
                    .iter()
                    .find(|(candidate, _)| candidate == row)
                    .map(|(_, device)| device.clone())
            });
            match device {
                Some(device) => {
                    if !device.fingerprint.is_empty() {
                        *reference.borrow_mut() = Some(device.fingerprint.clone());
                    }
                    *chosen.borrow_mut() = Some(device);
                    send.set_sensitive(true);
                }
                None => {
                    *chosen.borrow_mut() = None;
                    send.set_sensitive(false);
                }
            }
        });
    }

    let start: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let service = service.clone();
        let chosen = chosen.clone();
        let pin = pin.clone();
        let win = win.clone();
        let paths = paths.clone();
        Rc::new(move || {
            let Some(device) = chosen.borrow().clone() else {
                return;
            };
            let pin = pin.text().to_string();
            let pin = (!pin.trim().is_empty()).then(|| pin.trim().to_string());
            win.close();
            send_to(&ui, &service, device, paths.clone(), pin);
        })
    };
    {
        let start = start.clone();
        send.connect_clicked(move |_| start());
    }
    {
        let start = start.clone();
        list.connect_row_activated(move |_, _| start());
    }
    {
        let rebuild = rebuild.clone();
        let service = service.clone();
        scan.connect_clicked(move |_| {
            service.announce();
            rebuild();
        });
    }
    {
        let address = address.clone();
        let service = service.clone();
        let rebuild = rebuild.clone();
        let win = win.clone();
        add.connect_clicked(move |_| {
            let text = address.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            match service.add_by_address(&text) {
                Ok(device) => {
                    address.set_text("");
                    *reference.borrow_mut() = Some(device.fingerprint.clone());
                    rebuild();
                }
                Err(err) => dialogs::error(&win, "Cannot reach that device", &err),
            }
        });
    }
    {
        let start = start.clone();
        address.connect_activate(move |_| start());
    }
    {
        let win_key = win.clone();
        let rebuild = rebuild.clone();
        let service = service.clone();
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| match key {
            gtk::gdk::Key::Escape => {
                win_key.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::r if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                service.announce();
                rebuild();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        win.add_controller(keys);
    }

    service.announce();
    rebuild();
    win.present();
    search.grab_focus();
}

/// Run the transfer on a worker thread with a progress dialog in front.
fn send_to(
    ui: &Rc<AppUi>,
    service: &localsend::Service,
    device: Device,
    paths: Vec<PathBuf>,
    pin: Option<String>,
) {
    let progress = ProgressDialog::new(&ui.window, "Sending with LocalSend");
    let tx = progress.tx.clone();
    let cancelled = progress.cancel_flag();
    let service = service.clone();
    let alias = device.label();
    // Worker threads cannot touch widgets, so the final message comes back
    // through a channel that this main-loop future drains.
    let (notice, receiver) = async_channel::unbounded::<String>();
    {
        let weak = Rc::downgrade(ui);
        glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                if let Some(ui) = weak.upgrade() {
                    ui.toast(&message);
                }
            }
        });
    }
    std::thread::Builder::new()
        .name("nook-localsend-send".into())
        .spawn(move || {
            let report_tx = tx.clone();
            let report = move |progress: Progress| {
                let percent = if progress.file_size > 0 {
                    progress.file_done as f64 / progress.file_size as f64 * 100.0
                } else {
                    0.0
                };
                let _ = report_tx.try_send(ProgressMsg::Message(format!(
                    "{} · file {} of {} · {percent:.0}%",
                    progress.file,
                    progress.index + 1,
                    progress.count
                )));
                if progress.total > 0 {
                    let _ = report_tx.try_send(ProgressMsg::Fraction(
                        progress.total_done as f64 / progress.total as f64,
                    ));
                }
            };
            let outcome = service.send_files(&device, &paths, pin.as_deref(), &report, &cancelled);
            let _ = tx.try_send(ProgressMsg::Done);
            let message = match outcome {
                Ok(count) => format!(
                    "Sent {count} file{} to {alias}",
                    if count == 1 { "" } else { "s" }
                ),
                Err(err) => format!("LocalSend: {err}"),
            };
            let _ = notice.try_send(message);
        })
        .ok();
}

/* ------------------------------- receiving ------------------------------- */

/// Ask the user to accept a transfer that is waiting on the wire.
pub fn incoming(ui: &Rc<AppUi>, request: Incoming) {
    let count = request.files.len();
    let win = dialogs::base_window(&ui.window, "Incoming transfer");
    win.set_default_size(460, 420);
    win.set_resizable(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&dialogs::title_label(&format!(
        "{} wants to send {count} item{}",
        request.from,
        if count == 1 { "" } else { "s" }
    )));
    root.append(&dialogs::body_label(&format!(
        "{} · {} · from {} ({})",
        fsutil::human_size(request.total_bytes),
        request.device.kind(),
        request.device.address(),
        request.device.protocol.to_uppercase()
    )));

    let names = gtk::ListBox::new();
    names.set_selection_mode(gtk::SelectionMode::None);
    names.add_css_class("nook-library");
    for file in request.files.iter().take(50) {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.set_margin_top(5);
        line.set_margin_bottom(5);
        line.set_margin_start(8);
        line.set_margin_end(8);
        let name = gtk::Label::new(Some(&file.name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_hexpand(true);
        name.add_css_class("nook-file-name");
        let size = gtk::Label::new(Some(&fsutil::human_size(file.size)));
        size.add_css_class("nook-meta");
        line.append(&name);
        line.append(&size);
        row.set_child(Some(&line));
        names.append(&row);
    }
    let folder = ui.receive_dir();
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&names)
        .build();
    scroller.add_css_class("nook-surface-bare");
    root.append(&scroller);
    root.append(&dialogs::body_label(&format!(
        "Files are written to {}",
        dialogs::pretty_path(&folder)
    )));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let decline = gtk::Button::with_label("Decline");
    decline.add_css_class("nook-action");
    let accept = gtk::Button::with_label("Accept");
    accept.add_css_class("nook-suggested");
    actions.append(&spacer);
    actions.append(&decline);
    actions.append(&accept);
    root.append(&actions);
    win.set_child(Some(&root));

    let request = Rc::new(request);
    let answered = Rc::new(Cell::new(false));
    let decide: Rc<dyn Fn(bool)> = {
        let request = request.clone();
        let answered = answered.clone();
        let win = win.clone();
        let weak = Rc::downgrade(ui);
        Rc::new(move |accept| {
            if answered.replace(true) {
                return;
            }
            if accept {
                request.accept();
                if let Some(ui) = weak.upgrade() {
                    // With auto-accept on, accepting is what makes a device
                    // trusted: the next transfer from it needs no prompt.
                    if ui.config.borrow().transfer_auto_accept
                        && !request.device.fingerprint.is_empty()
                    {
                        let fingerprint = request.device.fingerprint.clone();
                        let mut config = ui.config.borrow_mut();
                        if !config
                            .transfer_trusted
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(&fingerprint))
                        {
                            config.transfer_trusted.push(fingerprint);
                            config.save();
                        }
                        drop(config);
                        let config = ui.config.borrow().clone();
                        if let Some(service) = ui.transfer_service() {
                            service.update(&config);
                        }
                    }
                    ui.toast(&format!("Receiving from {}…", request.from));
                }
            } else {
                request.decline();
            }
            win.close();
        })
    };
    {
        let decide = decide.clone();
        accept.connect_clicked(move |_| decide(true));
    }
    {
        let decide = decide.clone();
        decline.connect_clicked(move |_| decide(false));
    }
    win.set_default_widget(Some(&accept));
    {
        let decide = decide.clone();
        win.connect_close_request(move |_| {
            decide(false);
            glib::Propagation::Proceed
        });
    }
    win.present();
    accept.grab_focus();
}

/* -------------------------------- settings ------------------------------- */

/// LocalSend settings: who we are, where files land, auto-accept and the
/// trusted devices that may skip the prompt.
pub fn settings_dialog(ui: &Rc<AppUi>) {
    let service = ui.transfer_service();
    let win = dialogs::base_window(&ui.window, "LocalSend");
    win.set_default_size(560, 600);
    win.set_resizable(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(&dialogs::title_label("LocalSend transfers"));
    root.append(&dialogs::body_label(
        "Nook speaks the LocalSend protocol directly: no LocalSend window opens and \
         nothing runs in the background. Devices find each other with the LocalSend \
         multicast group and transfer over HTTPS.",
    ));

    let config = ui.config.borrow().clone();

    let enabled = gtk::Switch::new();
    enabled.set_active(config.transfer_enabled);
    let alias = gtk::Entry::new();
    alias.add_css_class("nook-entry");
    let alias_text = config.transfer_alias.clone().unwrap_or_else(|| {
        service
            .as_ref()
            .map(|service| service.alias())
            .unwrap_or_default()
    });
    alias.set_text(&alias_text);
    alias.set_placeholder_text(Some("Name other devices see (defaults to the hostname)"));
    let port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
    port.set_value(f64::from(config.transfer_port));
    let receive = gtk::Entry::new();
    receive.add_css_class("nook-entry");
    receive.set_text(
        config
            .transfer_receive_dir
            .as_deref()
            .unwrap_or("~/Downloads"),
    );
    let auto = gtk::Switch::new();
    auto.set_active(config.transfer_auto_accept);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(10);
    let mut row = 0;
    let label = |text: &str| {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.add_css_class("nook-meta");
        label
    };
    grid.attach(&label("Receive transfers"), 0, row, 1, 1);
    grid.attach(&enabled, 1, row, 1, 1);
    row += 1;
    grid.attach(&label("Device name"), 0, row, 1, 1);
    grid.attach(&alias, 1, row, 1, 1);
    row += 1;
    grid.attach(&label("Port"), 0, row, 1, 1);
    grid.attach(&port, 1, row, 1, 1);
    row += 1;
    grid.attach(&label("Save incoming files to"), 0, row, 1, 1);
    grid.attach(&receive, 1, row, 1, 1);
    row += 1;
    grid.attach(&label("Auto-accept from trusted devices"), 0, row, 1, 1);
    grid.attach(&auto, 1, row, 1, 1);
    root.append(&grid);

    root.append(&dialogs::body_label(
        "Accepted transfers keep the sender's folder structure and never overwrite: \
         a name that already exists gets “(2)” appended.",
    ));

    let trusted_label = gtk::Label::new(Some("TRUSTED DEVICES"));
    trusted_label.set_xalign(0.0);
    trusted_label.add_css_class("nook-section-label");
    root.append(&trusted_label);
    let trusted_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.append(&trusted_box);

    let trusted_holder: Rc<RefCell<Vec<String>>> =
        Rc::new(RefCell::new(config.transfer_trusted.clone()));
    let known: Vec<Device> = service
        .as_ref()
        .map(|service| service.peers())
        .unwrap_or_default();
    {
        let trusted_box = trusted_box.clone();
        let trusted_holder = trusted_holder.clone();
        let slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let slot_for_children = slot.clone();
        let rebuild: Rc<dyn Fn()> = Rc::new(move || {
            while let Some(child) = trusted_box.first_child() {
                trusted_box.remove(&child);
            }
            let list = trusted_holder.borrow().clone();
            if list.is_empty() {
                let empty = gtk::Label::new(Some(
                    "None yet. Turn on auto-accept and accept a transfer to trust a device…",
                ));
                empty.set_xalign(0.0);
                empty.set_wrap(true);
                empty.add_css_class("nook-meta");
                trusted_box.append(&empty);
                return;
            }
            for fingerprint in list {
                let name = known
                    .iter()
                    .find(|device| device.fingerprint.eq_ignore_ascii_case(&fingerprint))
                    .map(|device| device.label())
                    .unwrap_or_else(|| fingerprint[..fingerprint.len().min(16)].to_string());
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let label = gtk::Label::new(Some(&name));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.add_css_class("nook-meta");
                let forget = gtk::Button::with_label("Forget");
                forget.add_css_class("nook-action");
                {
                    let holder = trusted_holder.clone();
                    let slot = slot_for_children.clone();
                    forget.connect_clicked(move |_| {
                        holder.borrow_mut().retain(|item| item != &fingerprint);
                        if let Some(rebuild) = slot.borrow().clone() {
                            rebuild();
                        }
                    });
                }
                line.append(&label);
                line.append(&forget);
                trusted_box.append(&line);
            }
        });
        *slot.borrow_mut() = Some(rebuild.clone());
        rebuild();
    }

    if let Some(service) = service.as_ref() {
        root.append(&dialogs::body_label(&format!(
            "This device: {} · port {} · fingerprint {}",
            service.alias(),
            service.port(),
            &service.fingerprint()[..16]
        )));
    } else {
        root.append(&dialogs::body_label(
            "The transfer service is not running. Close any other LocalSend instance, \
             then restart Nook.",
        ));
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("nook-action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("nook-suggested");
    actions.append(&spacer);
    actions.append(&cancel);
    actions.append(&save);
    root.append(&actions);
    win.set_child(Some(&root));

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let weak = Rc::downgrade(ui);
        let win = win.clone();
        let enabled = enabled.clone();
        let alias = alias.clone();
        let port = port.clone();
        let receive = receive.clone();
        let auto = auto.clone();
        let trusted_holder = trusted_holder.clone();
        save.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let previous = ui.config.borrow().clone();
            {
                let mut config = ui.config.borrow_mut();
                config.transfer_enabled = enabled.is_active();
                let name = alias.text().to_string();
                config.transfer_alias = (!name.trim().is_empty()).then(|| name.trim().to_string());
                config.transfer_port = port.value_as_int() as u16;
                let dir = receive.text().to_string();
                config.transfer_receive_dir =
                    (!dir.trim().is_empty()).then(|| dir.trim().to_string());
                config.transfer_auto_accept = auto.is_active();
                config.transfer_trusted = trusted_holder.borrow().clone();
                config.save();
            }
            let now = ui.config.borrow().clone();
            // The listener, alias and port are fixed for the lifetime of the
            // service, so a change there restarts it; everything else applies
            // immediately.
            if now.transfer_enabled != previous.transfer_enabled
                || now.transfer_port != previous.transfer_port
                || now.transfer_alias != previous.transfer_alias
            {
                ui.restart_transfer();
            } else if let Some(service) = ui.transfer_service() {
                service.update(&now);
            }
            ui.toast("LocalSend settings saved");
            win.close();
        });
    }

    win.present();
    alias.grab_focus();
}
