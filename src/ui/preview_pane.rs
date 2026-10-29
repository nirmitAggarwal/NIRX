//! The toggleable preview sidebar: thumbnail + metadata for the current
//! selection, generated off the main thread by [`crate::preview`].

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::fsutil::{self, FileEntry};
use crate::preview::{PreviewKind, PreviewRequest, PreviewService};

pub struct PreviewPane {
    pub root: gtk::Box,
    picture: gtk::Picture,

    title: gtk::Label,
    subtitle: gtk::Label,
    meta: gtk::Box,
    text_view: gtk::TextView,

    note: gtk::Label,
    stack: gtk::Stack,
    service: PreviewService,
    current: RefCell<Option<PathBuf>>,

    debounce: RefCell<Option<glib::SourceId>>,
    max_bytes: Cell<u64>,
    image_size: Cell<u32>,
    spinner: gtk::Spinner,
}

impl PreviewPane {
    pub fn new(service: PreviewService) -> Rc<Self> {
        let title = gtk::Label::new(None);
        title.add_css_class("nook-preview-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("nook-meta");
        subtitle.set_xalign(0.0);

        let header = gtk::Box::new(gtk::Orientation::Vertical, 2);
        header.add_css_class("nook-preview-header");
        header.append(&title);
        header.append(&subtitle);

        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.add_css_class("nook-preview-image");
        picture.set_size_request(-1, 190);

        let spinner = gtk::Spinner::new();
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);

        let media_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        media_box.append(&picture);

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_wrap_mode(gtk::WrapMode::Char);
        text_view.add_css_class("nook-preview-text");
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.set_left_margin(8);
        text_view.set_right_margin(8);
        let text_scroller = gtk::ScrolledWindow::builder()
            .min_content_height(200)
            .vexpand(true)
            .child(&text_view)
            .build();
        text_scroller.add_css_class("nook-preview-text");
        text_scroller.set_visible(false);

        let note = gtk::Label::new(None);
        note.add_css_class("nook-meta");
        note.set_wrap(true);
        note.set_justify(gtk::Justification::Center);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(140);
        stack.add_named(&media_box, Some("media"));
        stack.add_named(&text_scroller, Some("text"));
        stack.add_named(&note, Some("note"));
        stack.add_named(&spinner, Some("loading"));
        stack.set_visible_child_name("note");

        let meta = gtk::Box::new(gtk::Orientation::Vertical, 3);
        meta.add_css_class("nook-props");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.add_css_class("nook-preview-body");
        body.append(&stack);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&meta);

        let body_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&body)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("nook-preview");
        root.append(&header);
        root.append(&body_scroller);
        // A narrow minimum keeps the preview from forcing the window wider
        // than the user wants; the pane still grows with its paned slot.
        root.set_width_request(220);

        let pane = Rc::new(Self {
            root,
            picture,
            title,
            subtitle,
            meta,
            text_view,
            note,
            stack,
            service: service.clone(),
            current: RefCell::new(None),
            debounce: RefCell::new(None),
            max_bytes: Cell::new(8 * 1024 * 1024),
            image_size: Cell::new(360),
            spinner,
        });

        let weak = Rc::downgrade(&pane);
        glib::spawn_future_local(async move {
            while let Ok(result) = service.rx.recv().await {
                let Some(pane) = weak.upgrade() else { break };
                let is_current = pane
                    .current
                    .borrow()
                    .as_ref()
                    .map(|p| p == &result.path)
                    .unwrap_or(false);
                if is_current {
                    pane.display(result.kind);
                }
            }
        });

        pane
    }

    pub fn set_max_bytes(&self, bytes: u64) {
        self.max_bytes.set(bytes);
    }

    /// Clear the preview for views without a folder or selection, such as the
    /// library tabs.
    pub fn show_note(&self, heading: &str, note: &str) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        *self.current.borrow_mut() = None;
        self.spinner.stop();
        self.set_header(heading, "Library");
        self.stack.set_visible_child_name("note");
        self.note.set_text(note);
        self.set_meta(&[]);
    }

    /// Show previews for the current selection (first item wins) or, when there
    /// is no selection, the folder itself.
    pub fn update(self: &Rc<Self>, selection: &[FileEntry], dir: &Path) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let target = selection.first().cloned();
        let weak = Rc::downgrade(self);
        let dir = dir.to_path_buf();
        let id = glib::timeout_add_local_once(Duration::from_millis(170), move || {
            let Some(pane) = weak.upgrade() else { return };
            // This source has already fired; forget its id so a later update
            // never tries to remove a source that no longer exists.
            pane.debounce.borrow_mut().take();
            match target {
                Some(entry) => pane.show_entry(&entry),
                None => pane.show_folder(&dir),
            }
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    fn set_header(&self, name: &str, sub: &str) {
        self.title.set_text(name);
        self.subtitle.set_text(sub);
    }

    fn show_folder(&self, dir: &Path) {
        *self.current.borrow_mut() = None;
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".into());
        self.set_header(&name, "Folder");
        self.stack.set_visible_child_name("note");
        self.note.set_text("Select a file to preview it.");
        self.set_meta(&[
            ("Location".into(), dir.display().to_string()),
            ("Items".into(), fsutil::count_items(dir).to_string()),
        ]);
    }

    fn show_entry(self: &Rc<Self>, entry: &FileEntry) {
        *self.current.borrow_mut() = Some(entry.path.clone());
        self.set_header(&entry.name, &fsutil::kind_label(entry));

        let mut rows: Vec<(String, String)> = Vec::new();
        rows.push((
            "Size".into(),
            if entry.is_dir {
                "—".into()
            } else {
                fsutil::human_size(entry.size)
            },
        ));
        rows.push(("Kind".into(), fsutil::kind_label(entry)));
        if !entry.mime.is_empty() {
            rows.push(("MIME".into(), entry.mime.clone()));
        }
        rows.push(("Modified".into(), fsutil::format_time(entry.modified)));
        rows.push((
            "Permissions".into(),
            format!(
                "{} ({:04o})",
                fsutil::mode_string(entry.mode, entry.is_dir),
                entry.mode & 0o7777
            ),
        ));
        if entry.is_symlink {
            if let Ok(target) = std::fs::read_link(&entry.path) {
                rows.push(("Link target".into(), target.display().to_string()));
            }
        }
        rows.push((
            "Location".into(),
            entry
                .path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        self.set_meta(&rows);

        if entry.is_dir {
            self.stack.set_visible_child_name("note");
            self.note
                .set_text(&format!("{} items", fsutil::count_items(&entry.path)));
            return;
        }

        self.stack.set_visible_child_name("loading");
        self.spinner.start();
        self.service.request(PreviewRequest {
            path: entry.path.clone(),
            size: self.image_size.get(),
            max_bytes: self.max_bytes.get(),
        });
    }

    fn display(&self, kind: PreviewKind) {
        self.spinner.stop();
        match kind {
            PreviewKind::Image(path) => {
                self.picture.set_filename(Some(&path));
                self.stack.set_visible_child_name("media");
            }
            PreviewKind::Text { snippet, truncated } => {
                let buf = self.text_view.buffer();
                let mut text = snippet;
                if truncated {
                    text.push_str("\n\n… preview truncated …");
                }
                buf.set_text(&text);
                self.stack.set_visible_child_name("text");
            }
            PreviewKind::Info(note) => {
                self.stack.set_visible_child_name("note");
                self.note.set_text(&if note.is_empty() {
                    "No preview available".to_string()
                } else {
                    format!("No preview available\n{note}")
                });
            }
            PreviewKind::Error(err) => {
                self.stack.set_visible_child_name("note");
                self.note.set_text(&err);
            }
        }
    }

    fn set_meta(&self, rows: &[(String, String)]) {
        while let Some(child) = self.meta.first_child() {
            self.meta.remove(&child);
        }
        for (key, value) in rows {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_top(1);
            row.set_margin_bottom(1);
            let k = gtk::Label::new(Some(key));
            k.add_css_class("nook-prop-key");
            k.set_xalign(0.0);
            k.set_width_chars(11);
            k.set_valign(gtk::Align::Start);
            let v = gtk::Label::new(Some(value));
            v.add_css_class("nook-prop-value");
            v.set_xalign(0.0);
            v.set_hexpand(true);
            v.set_wrap(true);
            v.set_selectable(true);
            v.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            row.append(&k);
            row.append(&v);
            self.meta.append(&row);
        }
    }
}
