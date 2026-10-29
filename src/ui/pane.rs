//! A single directory pane: model, filter, list/grid/recursive views, selection
//! and navigation history. Each tab owns one pane.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::fsutil::{self, FileEntry};
use crate::icons::{self, FolderStyle};
use crate::search::SearchMode;
use crate::settings::{Config, SortKey, ViewMode};
use crate::thumb;

type Callback = Rc<dyn Fn()>;

/// Drag and drop: sources, and an optional target folder (`None` means the
/// folder the pane is showing).
type DropCallback = Rc<dyn Fn(Vec<PathBuf>, Option<PathBuf>)>;

/// Icon size (px) and tile width used by the icon grid. The tile is wide enough
/// for two wrapped lines of a typical file name under the icon.
const GRID_ICON_SIZE: i32 = 64;
const GRID_CELL_WIDTH: i32 = 118;

/// A thumbnail that finished generating.
struct ThumbResult {
    path: PathBuf,
    mtime: u64,
    size: i32,
    ok: bool,
}

/// Result of loading one directory for the lazy recursive tree.
struct ExpandResult {
    dir: PathBuf,
    children: Vec<FileEntry>,
    error: Option<String>,
}

/// Live rectangle selection state. `selected` holds the positions this drag has
/// selected, so additive dragging can undo only what it added.
#[derive(Default)]
struct RubberState {
    active: bool,
    additive: bool,
    selected: HashSet<u32>,
}

/// Queue for preview icons: one worker thread per missing thumbnail, with the
/// in-flight and permanently failed keys kept so scrolling never re-spawns them.
#[derive(Clone)]
pub struct ThumbHandle {
    pending: Rc<RefCell<std::collections::HashSet<(PathBuf, u64, i32)>>>,
    failed: Rc<RefCell<std::collections::HashSet<(PathBuf, u64, i32)>>>,
    done: async_channel::Sender<ThumbResult>,
}

impl ThumbHandle {
    fn request(&self, entry: &FileEntry, size: i32, kind: thumb::Kind) {
        let mtime = thumb::mtime_key(entry);
        let key = (entry.path.clone(), mtime, size);
        if self.failed.borrow().contains(&key) || self.pending.borrow().contains(&key) {
            return;
        }
        self.pending.borrow_mut().insert(key);
        let tx = self.done.clone();
        let path = entry.path.clone();
        let _ = std::thread::Builder::new()
            .name("nook-thumb".into())
            .spawn(move || {
                let ok = thumb::prefetch(&path, mtime, size, kind);
                let _ = tx.send_blocking(ThumbResult {
                    path,
                    mtime,
                    size,
                    ok,
                });
            });
    }
}

struct LoadResult {
    path: PathBuf,
    entries: std::io::Result<Vec<FileEntry>>,
}

pub struct Pane {
    pub root: gtk::Box,
    pub path: RefCell<PathBuf>,
    history: RefCell<Vec<PathBuf>>,
    history_pos: Cell<usize>,
    pub store: gio::ListStore,
    pub filter: gtk::FilterListModel,
    pub list_sel: gtk::MultiSelection,
    pub grid_sel: gtk::MultiSelection,
    pub stack: gtk::Stack,
    /// Direct children of the current folder. The recursive view flattens this
    /// list lazily using `expanded` and `children`.
    base: RefCell<Vec<FileEntry>>,
    expanded: Rc<RefCell<HashSet<PathBuf>>>,
    children: RefCell<HashMap<PathBuf, Vec<FileEntry>>>,
    recursive: Rc<Cell<bool>>,
    expand_sender: async_channel::Sender<ExpandResult>,
    /// Selection to reapply after an asynchronous refresh finishes.
    restore: RefCell<Option<(PathBuf, Vec<PathBuf>)>>,
    /// File to select as soon as the next listing arrives (path-bar entry).
    pending_select: RefCell<Option<PathBuf>>,
    rubber_area: gtk::DrawingArea,
    rubber_rect: Rc<RefCell<Option<(f64, f64, f64, f64)>>>,
    rubber: Rc<RefCell<RubberState>>,
    pub view_mode: Cell<ViewMode>,
    pub show_hidden: Cell<bool>,
    sort_key: Cell<SortKey>,
    sort_desc: Cell<bool>,
    query: Rc<RefCell<String>>,
    search_mode: Rc<Cell<SearchMode>>,
    filter_obj: gtk::CustomFilter,
    empty_title: gtk::Label,
    empty_sub: gtk::Label,
    empty_box: gtk::Box,
    spinner: gtk::Spinner,
    loading: Cell<bool>,
    load_token: Cell<u64>,
    on_changed: RefCell<Option<Callback>>,
    on_search: RefCell<Option<Rc<dyn Fn(&str)>>>,
    /// Transient user-facing message (for example an unreadable subfolder).
    on_notice: RefCell<Option<Rc<dyn Fn(&str)>>>,
    on_open: RefCell<Option<Callback>>,
    context: Rc<RefCell<Option<Rc<dyn Fn(&FileEntry, f64, f64)>>>>,
    sender: async_channel::Sender<LoadResult>,
    refreshing: Cell<bool>,
    thumbs: ThumbHandle,
    /// Shared with the views: resolves `None` to this pane's folder and hands
    /// the move to the UI, which can refresh every open pane.
    drop_ctx: Rc<RefCell<Option<DropCallback>>>,
    on_drop: RefCell<Option<Rc<dyn Fn(Vec<PathBuf>, PathBuf)>>>,
}

fn entry_at(model: &impl IsA<gio::ListModel>, pos: u32) -> Option<FileEntry> {
    entry_of(model.item(pos)?)
}

fn entry_of(obj: glib::Object) -> Option<FileEntry> {
    let boxed = obj.downcast::<glib::BoxedAnyObject>().ok()?;
    let entry = boxed.borrow::<FileEntry>();
    Some(entry.clone())
}

impl Pane {
    pub fn new(
        start: PathBuf,
        view: ViewMode,
        show_hidden: bool,
        folder_style: Rc<RefCell<FolderStyle>>,
        config: Rc<RefCell<Config>>,
    ) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let search_mode: Rc<Cell<SearchMode>> = Rc::new(Cell::new(SearchMode::Name));
        let q = query.clone();
        let mode = search_mode.clone();
        let filter_obj = gtk::CustomFilter::new(move |obj| {
            let needle = q.borrow();
            if needle.trim().is_empty() {
                return true;
            }
            obj.downcast_ref::<glib::BoxedAnyObject>()
                .map(|b| {
                    let entry = b.borrow::<FileEntry>();
                    match mode.get() {
                        SearchMode::Name => crate::search::matches(needle.trim(), &entry.name),
                        SearchMode::Extension => {
                            crate::search::matches_extension(needle.trim(), &entry.extension())
                        }
                    }
                })
                .unwrap_or(true)
        });
        let filter = gtk::FilterListModel::new(Some(store.clone()), Some(filter_obj.clone()));
        // Both visual representations share one selection object over the
        // filtered model. This keeps focus-independent selection, bulk actions,
        // and selection restoration consistent when switching layouts.
        let list_sel = gtk::MultiSelection::new(Some(filter.clone()));
        let grid_sel = list_sel.clone();

        let context: Rc<RefCell<Option<Rc<dyn Fn(&FileEntry, f64, f64)>>>> =
            Rc::new(RefCell::new(None));
        let drop_ctx: Rc<RefCell<Option<DropCallback>>> = Rc::new(RefCell::new(None));
        // Lazy recursive tree state, shared with the row factories so the
        // expander arrows can read and toggle it.
        let expanded: Rc<RefCell<HashSet<PathBuf>>> = Rc::new(RefCell::new(HashSet::new()));
        let recursive = Rc::new(Cell::new(view == ViewMode::Recursive));
        let expand_cb: Rc<RefCell<Option<Rc<dyn Fn(PathBuf, bool)>>>> = Rc::new(RefCell::new(None));
        let (expand_sender, expand_rx) = async_channel::unbounded::<ExpandResult>();
        let (thumb_done, thumb_rx) = async_channel::unbounded::<ThumbResult>();
        let thumbs = ThumbHandle {
            pending: Rc::new(RefCell::new(std::collections::HashSet::new())),
            failed: Rc::new(RefCell::new(std::collections::HashSet::new())),
            done: thumb_done,
        };
        let list_view = build_list_view(
            &list_sel,
            context.clone(),
            folder_style.clone(),
            config.clone(),
            thumbs.clone(),
            drop_ctx.clone(),
            expanded.clone(),
            recursive.clone(),
            expand_cb.clone(),
        );
        let grid_view = build_grid_view(
            &grid_sel,
            context.clone(),
            folder_style.clone(),
            config.clone(),
            thumbs.clone(),
            drop_ctx.clone(),
            expanded.clone(),
            recursive.clone(),
            expand_cb.clone(),
        );

        // Each view scrolls inside its own GtkScrolledWindow, and the stack
        // switches between those scrollers. The scrollable widget must be the
        // scroller's *direct* child: wrapping a Stack makes GtkScrolledWindow
        // put a GtkViewport around it, which allocates the stack its natural
        // height (one column × every file) and leaves GtkGridView stretching
        // rows over that height — the huge row gaps and endless scrollbar.
        let list_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&list_view)
            .build();
        list_scroller.add_css_class("nook-surface-bare");
        // The icon grid must fit the viewport, so it never scrolls sideways;
        // the detailed list keeps horizontal scrolling for its fixed columns.
        let grid_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&grid_view)
            .build();
        grid_scroller.add_css_class("nook-surface-bare");

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        stack.add_named(&list_scroller, Some("list"));
        stack.add_named(&grid_scroller, Some("grid"));
        stack.set_visible_child_name(match view {
            ViewMode::List => "list",
            ViewMode::Grid => "grid",
            ViewMode::Recursive => "list",
        });

        let spinner = gtk::Spinner::new();
        spinner.set_size_request(22, 22);
        let empty_title = gtk::Label::new(None);
        empty_title.add_css_class("nook-empty-title");
        let empty_sub = gtk::Label::new(None);
        empty_sub.add_css_class("nook-empty-sub");
        empty_sub.set_wrap(true);
        empty_sub.set_justify(gtk::Justification::Center);
        let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        empty_box.set_halign(gtk::Align::Center);
        empty_box.set_valign(gtk::Align::Center);
        empty_box.append(&spinner);
        empty_box.append(&empty_title);
        empty_box.append(&empty_sub);
        empty_box.set_visible(false);

        // Selection rectangle drawn over the view while dragging on empty space.
        let rubber_rect: Rc<RefCell<Option<(f64, f64, f64, f64)>>> = Rc::new(RefCell::new(None));
        let rubber_area = gtk::DrawingArea::new();
        rubber_area.set_can_target(false);
        rubber_area.set_hexpand(true);
        rubber_area.set_vexpand(true);
        rubber_area.set_visible(false);
        {
            let rect = rubber_rect.clone();
            // The drag reports stack coordinates, but the rectangle is painted
            // on an overlay above the scroller. Translate by the stack's origin
            // so the band stays under the pointer while the view is scrolled.
            let area_weak = rubber_area.downgrade();
            let stack_weak = stack.downgrade();
            rubber_area.set_draw_func(move |_, cr, _, _| {
                let Some((x, y, w, h)) = *rect.borrow() else {
                    return;
                };
                let mut origin = (0.0f64, 0.0f64);
                if let (Some(area), Some(stack)) = (area_weak.upgrade(), stack_weak.upgrade()) {
                    if let Some(point) =
                        stack.compute_point(&area, &gtk::graphene::Point::new(0.0, 0.0))
                    {
                        origin = (f64::from(point.x()), f64::from(point.y()));
                    }
                }
                let (x, y) = (x + origin.0, y + origin.1);
                // Material blue: readable against the fixed #0f1318 surface.
                cr.set_source_rgba(0.32, 0.58, 0.89, 0.20);
                cr.rectangle(x, y, w, h);
                let _ = cr.fill();
                cr.set_source_rgba(0.45, 0.68, 0.95, 0.90);
                cr.set_line_width(1.0);
                cr.rectangle(x + 0.5, y + 0.5, (w - 1.0).max(0.0), (h - 1.0).max(0.0));
                let _ = cr.stroke();
            });
        }

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&empty_box);
        overlay.add_overlay(&rubber_area);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("nook-surface");
        root.append(&overlay);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let (sender, receiver) = async_channel::unbounded::<LoadResult>();

        let pane = Rc::new(Pane {
            root,
            path: RefCell::new(start.clone()),
            history: RefCell::new(vec![start.clone()]),
            history_pos: Cell::new(0),
            store,
            filter,
            list_sel,
            grid_sel,
            stack,
            base: RefCell::new(Vec::new()),
            expanded,
            children: RefCell::new(HashMap::new()),
            recursive,
            expand_sender,
            restore: RefCell::new(None),
            pending_select: RefCell::new(None),
            rubber_area: rubber_area.clone(),
            rubber_rect,
            rubber: Rc::new(RefCell::new(RubberState::default())),
            view_mode: Cell::new(view),
            show_hidden: Cell::new(show_hidden),
            sort_key: Cell::new(SortKey::Name),
            sort_desc: Cell::new(false),
            query,
            search_mode,
            filter_obj,
            empty_title,
            empty_sub,
            empty_box,
            spinner,
            loading: Cell::new(false),
            load_token: Cell::new(0),
            on_changed: RefCell::new(None),
            on_search: RefCell::new(None),
            on_notice: RefCell::new(None),
            on_open: RefCell::new(None),
            context,
            sender,
            refreshing: Cell::new(false),
            thumbs,
            drop_ctx,
            on_drop: RefCell::new(None),
        });

        // Load results are delivered on the GTK main context.
        let weak = Rc::downgrade(&pane);
        glib::spawn_future_local(async move {
            while let Ok(result) = receiver.recv().await {
                let Some(pane) = weak.upgrade() else { break };
                pane.apply_load(result);
            }
        });

        // Finished thumbnails rebind the rows that show their file.
        let weak = Rc::downgrade(&pane);
        glib::spawn_future_local(async move {
            while let Ok(result) = thumb_rx.recv().await {
                let Some(pane) = weak.upgrade() else { break };
                pane.finish_thumb(result);
            }
        });

        // Lazy recursive tree: one directory is read at a time, when its
        // expander is clicked, so opening a folder with a huge tree stays fast.
        {
            let weak = Rc::downgrade(&pane);
            glib::spawn_future_local(async move {
                while let Ok(result) = expand_rx.recv().await {
                    let Some(pane) = weak.upgrade() else { break };
                    pane.finish_expand(result);
                }
            });
        }
        {
            let weak = Rc::downgrade(&pane);
            *expand_cb.borrow_mut() = Some(Rc::new(move |dir: PathBuf, expand: bool| {
                if let Some(pane) = weak.upgrade() {
                    pane.toggle_expand(dir, expand);
                }
            }));
        }
        pane.install_selection_gestures();

        // Drag and drop: a drop on empty space targets this pane's folder, and
        // the actual move runs through the UI so every pane can refresh.
        {
            let ctx = pane.drop_ctx.clone();
            let weak = Rc::downgrade(&pane);
            *ctx.borrow_mut() = Some(Rc::new(move |sources, target| {
                let Some(pane) = weak.upgrade() else { return };
                let target = target.unwrap_or_else(|| pane.path());
                if sources.is_empty() || !target.is_dir() {
                    return;
                }
                let callback = pane.on_drop.borrow().clone();
                if let Some(callback) = callback {
                    callback(sources, target);
                }
            }));
        }
        {
            let ctx = pane.drop_ctx.clone();
            let background = gtk::DropTarget::new(
                gtk::gdk::FileList::static_type(),
                gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY,
            );
            background.connect_drop(move |_, value, _, _| {
                let Ok(list) = value.get::<gtk::gdk::FileList>() else {
                    return false;
                };
                let sources = file_list_paths(&list);
                if sources.is_empty() {
                    return false;
                }
                let callback = ctx.borrow().clone();
                if let Some(callback) = callback {
                    callback(sources, None);
                }
                true
            });
            pane.root.add_controller(background);
        }

        // Type-to-filter: printable characters edit the filter, Backspace
        // removes one and Escape clears it. Capture phase so the list views do
        // not consume the keys first.
        {
            let weak = Rc::downgrade(&pane);
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(pane) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let plain = state
                    .difference(gtk::gdk::ModifierType::SHIFT_MASK)
                    .is_empty();
                if !plain {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gtk::gdk::Key::Escape => {
                        if pane.search_query().is_empty() {
                            return glib::Propagation::Proceed;
                        }
                        pane.set_search("");
                        pane.notify_search("");
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::BackSpace => {
                        if pane.search_query().is_empty() {
                            return glib::Propagation::Proceed;
                        }
                        let mut query = pane.search_query();
                        query.pop();
                        pane.set_search(&query);
                        pane.notify_search(&query);
                        glib::Propagation::Stop
                    }
                    _ => {
                        let Some(character) = key.to_unicode() else {
                            return glib::Propagation::Proceed;
                        };
                        if character.is_control() || character.is_whitespace() {
                            return glib::Propagation::Proceed;
                        }
                        let mut query = pane.search_query();
                        query.push(character);
                        pane.set_search(&query);
                        pane.notify_search(&query);
                        glib::Propagation::Stop
                    }
                }
            });
            pane.root.add_controller(keys);
        }

        pane.reload();
        pane
    }

    pub fn on_changed(&self, f: impl Fn() + 'static) {
        *self.on_changed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn on_open(&self, f: impl Fn() + 'static) {
        *self.on_open.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_on_notice(&self, f: impl Fn(&str) + 'static) {
        *self.on_notice.borrow_mut() = Some(Rc::new(f));
    }

    fn notice(&self, message: &str) {
        if let Some(callback) = self.on_notice.borrow().as_ref() {
            callback(message);
        }
    }

    pub fn on_context(&self, f: impl Fn(&FileEntry, f64, f64) + 'static) {
        *self.context.borrow_mut() = Some(Rc::new(f));
    }

    /// Called with the dragged paths and the folder they were dropped on.
    pub fn set_on_drop(&self, f: impl Fn(Vec<PathBuf>, PathBuf) + 'static) {
        *self.on_drop.borrow_mut() = Some(Rc::new(f));
    }

    fn notify(&self) {
        if let Some(cb) = self.on_changed.borrow().as_ref() {
            cb();
        }
    }

    /// Wire double-click / Enter activation for both views.
    pub fn wire_activation(self: &Rc<Self>) {
        let list = self
            .view_widget("list")
            .and_then(|w| w.downcast::<gtk::ColumnView>().ok());
        if let Some(list) = list {
            let weak = Rc::downgrade(self);
            list.connect_activate(move |_, pos| {
                if let Some(pane) = weak.upgrade() {
                    pane.open_position(pos);
                }
            });
        }
        let grid = self
            .view_widget("grid")
            .and_then(|w| w.downcast::<gtk::GridView>().ok());
        if let Some(grid) = grid {
            let weak = Rc::downgrade(self);
            grid.connect_activate(move |_, pos| {
                if let Some(pane) = weak.upgrade() {
                    pane.open_position(pos);
                }
            });
        }

        // The list and grid deliberately share a selection model, so one
        // observer keeps selection-dependent chrome in sync in either view.
        let weak = Rc::downgrade(self);
        self.list_sel.connect_selection_changed(move |_, _, _| {
            if let Some(pane) = weak.upgrade() {
                pane.notify();
            }
        });
    }

    fn open_position(self: &Rc<Self>, pos: u32) {
        let model = self
            .active_selection()
            .upcast_ref::<gio::ListModel>()
            .clone();
        let Some(entry) = entry_at(&model, pos) else {
            return;
        };
        if entry.is_dir {
            Pane::navigate(self, entry.path, true);
        } else if let Some(cb) = self.on_open.borrow().as_ref() {
            // Select this entry first so the callback acts on the right file.
            self.select_only(pos);
            cb();
        }
    }

    pub fn active_selection(&self) -> &gtk::MultiSelection {
        match self.view_mode.get() {
            ViewMode::List | ViewMode::Recursive => &self.list_sel,
            ViewMode::Grid => &self.grid_sel,
        }
    }

    fn active_model(&self) -> gio::ListModel {
        self.active_selection()
            .model()
            .expect("selection model always has a model")
    }

    fn select_only(&self, pos: u32) {
        let sel = self.active_selection();
        sel.unselect_all();
        sel.select_item(pos, true);
    }

    pub fn selected_entries(&self) -> Vec<FileEntry> {
        let sel = self.active_selection();
        let model = self.active_model();
        let bitset = sel.selection();
        let mut out = Vec::new();
        for pos in 0..model.n_items() {
            if bitset.contains(pos) {
                if let Some(entry) = entry_at(&model, pos) {
                    out.push(entry);
                }
            }
        }
        out
    }

    pub fn select_all(&self) {
        self.active_selection().select_all();
    }

    /// Select `path` once the current folder finishes loading; used when the
    /// path bar is given a file instead of a directory.
    pub fn reveal(&self, path: PathBuf) {
        *self.pending_select.borrow_mut() = Some(path);
    }

    pub fn select_none(&self) {
        self.active_selection().unselect_all();
    }

    /// Flip the selection: every selected row becomes unselected and vice versa.
    pub fn invert_selection(&self) {
        let sel = self.active_selection();
        let model = self.active_model();
        let bitset = sel.selection();
        for pos in 0..model.n_items() {
            if bitset.contains(pos) {
                sel.unselect_item(pos);
            } else {
                sel.select_item(pos, false);
            }
        }
        sel.selection_changed(0, model.n_items());
    }

    /// Give the visible icon/detail view the keyboard focus, so type-to-filter,
    /// arrows and Enter work without clicking first.
    pub fn focus_view(&self) {
        if let Some(child) = self.stack.visible_child() {
            // The stack holds scrollers now; focus the view inside one.
            match child.downcast::<gtk::ScrolledWindow>() {
                Ok(scroller) => {
                    match scroller.child() {
                        Some(inner) => {
                            inner.grab_focus();
                        }
                        None => {
                            scroller.grab_focus();
                        }
                    };
                }
                Err(child) => {
                    child.grab_focus();
                }
            }
        }
    }

    /// The scroller the stack shows for `name`, which owns the real view.
    fn view_widget(&self, name: &str) -> Option<gtk::Widget> {
        let child = self.stack.child_by_name(name)?;
        let scroller = child.downcast::<gtk::ScrolledWindow>().ok()?;
        scroller.child()
    }

    /// The view widget that is currently visible.
    fn visible_view(&self) -> Option<gtk::Widget> {
        let child = self.stack.visible_child()?;
        match child.downcast::<gtk::ScrolledWindow>() {
            Ok(scroller) => scroller.child(),
            Err(child) => Some(child),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    fn pane_root(&self) -> PathBuf {
        let p = self.path();
        p
    }

    pub fn set_view(self: &Rc<Self>, mode: ViewMode) {
        if self.view_mode.get() == mode {
            return;
        }
        let selected: Vec<PathBuf> = self
            .selected_entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        self.view_mode.set(mode);
        self.recursive.set(mode == ViewMode::Recursive);
        self.stack.set_visible_child_name(match mode {
            ViewMode::List | ViewMode::Recursive => "list",
            ViewMode::Grid => "grid",
        });
        // The same direct children feed both layouts; the recursive mode adds
        // only the subtrees that are expanded right now.
        self.refresh_visible();
        if !selected.is_empty() {
            let sel = self.active_selection();
            let model = self.active_model();
            let wanted: HashSet<PathBuf> = selected.into_iter().collect();
            for pos in 0..model.n_items() {
                if let Some(entry) = entry_at(&model, pos) {
                    if wanted.contains(&entry.path) {
                        sel.select_item(pos, false);
                    }
                }
            }
        }
    }

    pub fn set_sort(self: &Rc<Self>, key: SortKey, desc: bool) {
        self.sort_key.set(key);
        self.sort_desc.set(desc);
        Pane::reload(self);
    }

    pub fn set_show_hidden(self: &Rc<Self>, show: bool) {
        self.show_hidden.set(show);
        Pane::reload(self);
    }

    pub fn set_search(&self, query: &str) {
        *self.query.borrow_mut() = query.to_string();
        self.filter_obj.changed(gtk::FilterChange::Different);
        self.update_empty_state();
        self.notify();
    }

    /// Switch between matching on file names and on file extensions.
    pub fn set_search_mode(&self, mode: SearchMode) {
        if self.search_mode.get() == mode {
            return;
        }
        self.search_mode.set(mode);
        self.filter_obj.changed(gtk::FilterChange::Different);
        self.update_empty_state();
    }

    /// Called when the pane itself edits the filter (type-to-filter), so the
    /// chrome can mirror the query.
    pub fn set_on_search<F: Fn(&str) + 'static>(&self, callback: F) {
        *self.on_search.borrow_mut() = Some(Rc::new(callback));
    }

    fn notify_search(&self, text: &str) {
        let callback = self.on_search.borrow().clone();
        if let Some(callback) = callback {
            callback(text);
        }
    }

    pub fn search_query(&self) -> String {
        self.query.borrow().clone()
    }

    /// Replace the model with `entries`, which are the direct children of the
    /// current folder. The recursive view then adds only the expanded subtrees.
    pub fn set_entries(&self, entries: Vec<FileEntry>) {
        // Replace the underlying model without allowing stale positions to
        // survive a navigation, filter refresh, or directory replacement.
        self.list_sel.unselect_all();
        *self.base.borrow_mut() = entries;
        self.children.borrow_mut().clear();
        self.expanded.borrow_mut().clear();
        self.refresh_visible();
    }

    /// Push `entries` into the list store without touching the tree state.
    fn splice(&self, entries: Vec<FileEntry>) {
        let objects: Vec<glib::BoxedAnyObject> =
            entries.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.update_empty_state();
    }

    /// Rebuild the visible rows from the direct children plus every expanded
    /// subtree that has already been loaded.
    fn refresh_visible(&self) {
        let visible = if self.recursive.get() {
            self.flatten()
        } else {
            self.base.borrow().clone()
        };
        self.splice(visible);
    }

    fn flatten(&self) -> Vec<FileEntry> {
        let expanded = self.expanded.borrow();
        let children = self.children.borrow();
        let mut out = Vec::new();
        for entry in self.base.borrow().iter() {
            out.push(entry.clone());
            if entry.is_dir && expanded.contains(&entry.path) {
                if let Some(kids) = children.get(&entry.path) {
                    append_subtree(kids, &expanded, &children, entry.depth, &mut out);
                }
            }
        }
        out
    }

    /// Expand or collapse one folder in the recursive view. Children are read on
    /// a worker thread the first time a folder is expanded.
    fn toggle_expand(self: &Rc<Self>, dir: PathBuf, expand: bool) {
        if !expand {
            self.expanded.borrow_mut().remove(&dir);
            self.refresh_visible();
            return;
        }
        if self.children.borrow().contains_key(&dir) {
            self.expanded.borrow_mut().insert(dir);
            self.refresh_visible();
            return;
        }
        let show_hidden = self.show_hidden.get();
        let (key, desc) = (self.sort_key.get(), self.sort_desc.get());
        let root = self.path();
        let sender = self.expand_sender.clone();
        std::thread::Builder::new()
            .name("nook-tree".into())
            .spawn(move || {
                let (children, error) = match fsutil::list_dir(&dir, show_hidden) {
                    Ok(mut children) => {
                        fsutil::sort_entries(&mut children, key, desc);
                        let rel = dir
                            .strip_prefix(&root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        for child in &mut children {
                            child.rel_parent = rel.clone();
                        }
                        (children, None)
                    }
                    Err(err) => (Vec::new(), Some(err.to_string())),
                };
                let _ = sender.send_blocking(ExpandResult {
                    dir,
                    children,
                    error,
                });
            })
            .ok();
    }

    fn finish_expand(&self, result: ExpandResult) {
        if let Some(error) = result.error {
            // Do not pretend the folder is open when it could not be read.
            self.notice(&format!("Cannot open {}: {error}", result.dir.display()));
            return;
        }
        self.children
            .borrow_mut()
            .insert(result.dir.clone(), result.children);
        self.expanded.borrow_mut().insert(result.dir);
        self.refresh_visible();
    }

    /// Select every row whose visible cell intersects `rect`, given in stack
    /// coordinates. Additive drags keep the existing selection.
    fn apply_rubber_selection(&self, rect: (f64, f64, f64, f64)) {
        let Some(view) = self.visible_view() else {
            return;
        };
        let mut rects: Vec<(u32, gtk::graphene::Rect)> = Vec::new();
        collect_item_rects(&view, self.stack.upcast_ref(), &mut rects);
        // Tiles outside the viewport keep a placeholder box; skip them so a
        // band can only ever select what is actually on screen.
        rects.retain(|(_, r)| r.width() >= 40.0 && r.height() >= 40.0);
        let hit =
            gtk::graphene::Rect::new(rect.0 as f32, rect.1 as f32, rect.2 as f32, rect.3 as f32);
        let mut inside: HashSet<u32> = HashSet::new();
        for (pos, bounds) in rects {
            if bounds.intersection(&hit).is_some() {
                inside.insert(pos);
            }
        }

        let sel = self.active_selection().clone();
        // Take the borrow scope before touching the selection model, whose
        // change notifications run UI code that must not see a live borrow.
        let (previous, additive) = {
            let mut rubber = self.rubber.borrow_mut();
            (std::mem::take(&mut rubber.selected), rubber.additive)
        };
        for pos in &previous {
            if !inside.contains(pos) && !additive {
                sel.unselect_item(*pos);
            }
        }
        for pos in &inside {
            if !sel.selection().contains(*pos) {
                sel.select_item(*pos, false);
            }
        }
        self.rubber.borrow_mut().selected = inside;
    }

    /// Clear the rubber band and finish the current drag selection.
    fn end_rubber_selection(&self) {
        {
            let mut rubber = self.rubber.borrow_mut();
            rubber.active = false;
            rubber.additive = false;
            rubber.selected.clear();
        }
        *self.rubber_rect.borrow_mut() = None;
        self.rubber_area.set_visible(false);
        self.rubber_area.queue_draw();
    }

    /// Empty-space clicks clear the selection and start a rubber band drag; item
    /// clicks are left to GTK, which already handles Ctrl/Shift selection.
    fn install_selection_gestures(self: &Rc<Self>) {
        let press: Rc<RefCell<Option<(f64, f64, bool)>>> = Rc::new(RefCell::new(None));

        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(self);
            let press = press.clone();
            click.connect_pressed(move |gesture, _, x, y| {
                let Some(pane) = weak.upgrade() else { return };
                let state = gesture.current_event_state();
                let additive = state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
                );
                let empty = is_empty_space(&pane.stack, x, y);
                *press.borrow_mut() = Some((x, y, empty));
                if !empty {
                    // Item presses belong to GTK's list/grid selection model.
                    // This capture gesture must not claim their event sequence.
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
                // Do not clear selection on press: an empty click only clears
                // on release, while a drag preserves its base selection.
                let mut rubber = pane.rubber.borrow_mut();
                rubber.additive = additive;
            });
        }

        let drag = gtk::GestureDrag::new();
        drag.set_button(gtk::gdk::BUTTON_PRIMARY);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            // A drag that starts on an item is a file drag, not a rubber band.
            // Denying at `begin` leaves the sequence to the row's drag source.
            let weak = Rc::downgrade(self);
            let press = press.clone();
            drag.connect_begin(move |gesture, _| {
                let Some(pane) = weak.upgrade() else { return };
                // The capture click handler recorded where the press landed.
                let empty = match *press.borrow() {
                    Some((_, _, empty)) => empty,
                    None => gesture
                        .point(None)
                        .map(|(x, y)| is_empty_space(&pane.stack, x, y))
                        .unwrap_or(false),
                };
                if !empty {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let press = press.clone();
            drag.connect_drag_begin(move |gesture, x, y| {
                let Some(pane) = weak.upgrade() else { return };
                let Some((px, py, empty)) = *press.borrow() else {
                    return;
                };
                if !empty {
                    return;
                }
                // Remember that this sequence became a rubber-band drag so
                // its eventual click release cannot be mistaken for a click.
                *press.borrow_mut() = Some((px, py, false));
                let _ = (x, y);
                let mut rubber = pane.rubber.borrow_mut();
                if !rubber.additive {
                    pane.select_none();
                }
                rubber.active = true;
                rubber.selected.clear();
                let start = gesture.start_point().unwrap_or((px, py));
                pane.rubber_rect
                    .borrow_mut()
                    .replace((start.0, start.1, 0.0, 0.0));
            });
        }
        {
            let weak = Rc::downgrade(self);
            drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                let Some(pane) = weak.upgrade() else { return };
                if !pane.rubber.borrow().active {
                    return;
                }
                let Some((start_x, start_y)) = gesture.start_point() else {
                    return;
                };
                let current_x = start_x + offset_x;
                let current_y = start_y + offset_y;
                let rect = (
                    start_x.min(current_x),
                    start_y.min(current_y),
                    (current_x - start_x).abs(),
                    (current_y - start_y).abs(),
                );
                *pane.rubber_rect.borrow_mut() = Some(rect);
                pane.rubber_area.set_visible(true);
                pane.rubber_area.queue_draw();
                let big_enough = (offset_x.abs() > 3.0) || (offset_y.abs() > 3.0);
                if big_enough {
                    pane.apply_rubber_selection(rect);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let press = press.clone();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(pane) = weak.upgrade() {
                    pane.end_rubber_selection();
                }
                *press.borrow_mut() = None;
            });
        }
        {
            let weak = Rc::downgrade(self);
            let press = press.clone();
            drag.connect_cancel(move |_, _| {
                if let Some(pane) = weak.upgrade() {
                    pane.end_rubber_selection();
                }
                *press.borrow_mut() = None;
            });
        }
        {
            let weak = Rc::downgrade(self);
            let press = press.clone();
            click.connect_released(move |gesture, _, x, y| {
                let start = press.borrow_mut().take();
                let Some(pane) = weak.upgrade() else { return };
                let Some((start_x, start_y, was_empty)) = start else {
                    return;
                };
                if !was_empty
                    || gesture.current_event_state().intersects(
                        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
                    )
                {
                    return;
                }
                // A click with no movement clears; the paired drag gesture
                // handles movement, preserving the old selection as a base.
                let moved = (x - start_x).abs() > 3.0 || (y - start_y).abs() > 3.0;
                if !moved && is_empty_space(&pane.stack, x, y) {
                    pane.select_none();
                }
            });
        }

        self.stack.add_controller(click);
        self.stack.add_controller(drag);
    }

    fn restore_paths(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let wanted: HashSet<PathBuf> = paths.into_iter().collect();
        let model = self.active_model();
        let sel = self.active_selection();
        for pos in 0..model.n_items() {
            if entry_at(&model, pos).is_some_and(|entry| wanted.contains(&entry.path)) {
                sel.select_item(pos, false);
            }
        }
    }

    /// Reapply the selection captured before a background refresh.
    fn restore_selection(&self, path: &std::path::Path) {
        let take = {
            let mut slot = self.restore.borrow_mut();
            match slot.as_ref() {
                Some((target, _)) if target == path => slot.take().map(|(_, paths)| paths),
                _ => None,
            }
        };
        let Some(paths) = take else { return };
        self.restore_paths(paths);
    }

    fn apply_load(self: &Rc<Self>, result: LoadResult) {
        if *self.path.borrow() != result.path {
            return;
        }
        self.loading.set(false);
        self.spinner.stop();
        self.spinner.set_visible(false);
        match result.entries {
            Ok(entries) => {
                self.empty_title.remove_css_class("nook-error");
                let mut entries = entries;
                fsutil::sort_entries(&mut entries, self.sort_key.get(), self.sort_desc.get());
                // Keep the recursive tree open across refreshes: remember the
                // expanded folders, then load their children again.
                let was_expanded: Vec<PathBuf> = self.expanded.borrow().iter().cloned().collect();
                self.set_entries(entries);
                if self.recursive.get() && !was_expanded.is_empty() {
                    self.expanded
                        .borrow_mut()
                        .extend(was_expanded.iter().cloned());
                    for dir in was_expanded {
                        if dir.is_dir() {
                            self.toggle_expand(dir, true);
                        }
                    }
                }
                self.restore_selection(&result.path);
                if let Some(target) = self.pending_select.borrow_mut().take() {
                    let sel = self.active_selection();
                    let model = self.active_model();
                    for pos in 0..model.n_items() {
                        if let Some(entry) = entry_at(&model, pos) {
                            if entry.path == target {
                                sel.unselect_all();
                                sel.select_item(pos, true);
                                break;
                            }
                        }
                    }
                }
            }
            Err(err) => {
                self.set_entries(Vec::new());
                self.empty_title.set_text("Cannot open this folder");
                self.empty_sub.set_text(&format!("{err}"));
                self.empty_title.add_css_class("nook-error");
                self.empty_box.set_visible(true);
                self.show_dialog_error(&err);
            }
        }
        self.notify();
    }

    fn show_dialog_error(&self, err: &std::io::Error) {
        let path = self.path();
        let message = match err.kind() {
            std::io::ErrorKind::PermissionDenied => {
                format!("Permission denied.\n\n{}", path.display())
            }
            std::io::ErrorKind::NotFound => {
                format!("Folder no longer exists.\n\n{}", path.display())
            }
            _ => format!("{}\n\n{}", err, path.display()),
        };
        let dialog = gtk::AlertDialog::builder()
            .message("Could not open folder")
            .detail(&message)
            .modal(true)
            .build();
        if let Some(win) = self.root.root().and_downcast::<gtk::Window>() {
            dialog.show(Some(&win));
        }
    }

    fn update_empty_state(&self) {
        let has_items = self.filter.n_items() > 0;
        if self.loading.get() {
            self.empty_box.set_visible(true);
            self.spinner.set_visible(true);
            self.spinner.start();
            self.empty_title.set_text("Loading…");
            self.empty_sub.set_text("");
            return;
        }
        if has_items {
            self.empty_box.set_visible(false);
            return;
        }
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.empty_box.set_visible(true);
        let query = self.query.borrow().clone();
        if !query.trim().is_empty() {
            self.empty_title.set_text("No matches");
            self.empty_sub
                .set_text(&format!("Nothing here matches “{}”.", query.trim()));
        } else if self.show_hidden.get() {
            self.empty_title.set_text("This folder is empty");
            self.empty_sub.set_text("Nothing to show here yet.");
        } else {
            self.empty_title.set_text("This folder is empty");
            self.empty_sub
                .set_text("Hidden files are not shown — press Ctrl+H to reveal them.");
        }
    }

    pub fn reload(self: &Rc<Self>) {
        let path = self.path();
        let token = self.load_token.get() + 1;
        self.load_token.set(token);
        self.loading.set(true);
        self.update_empty_state();
        if token == 1 {
            self.spinner.start();
        }
        let sender = self.sender.clone();
        let show_hidden = self.show_hidden.get();
        // Every view starts from the direct children; the recursive mode then
        // adds subtrees lazily, so a huge tree never loads all at once.
        std::thread::Builder::new()
            .name("nook-list".into())
            .spawn(move || {
                let entries = fsutil::list_dir(&path, show_hidden);
                let _ = sender.send_blocking(LoadResult {
                    path: path.clone(),
                    entries,
                });
            })
            .ok();
    }

    pub fn navigate(self: &Rc<Self>, path: PathBuf, push_history: bool) {
        if !path.exists() {
            return;
        }
        if path.is_file() {
            return;
        }
        if push_history {
            let mut history = self.history.borrow_mut();
            let pos = self.history_pos.get();
            history.truncate(pos + 1);
            if history.get(pos) != Some(&path) {
                history.push(path.clone());
                self.history_pos.set(history.len() - 1);
            }
        }
        *self.path.borrow_mut() = path;
        *self.query.borrow_mut() = String::new();
        *self.restore.borrow_mut() = None;
        *self.pending_select.borrow_mut() = None;
        self.filter_obj.changed(gtk::FilterChange::Different);
        self.set_entries(Vec::new());
        Pane::reload(self);
        self.notify();
    }

    pub fn go_up(self: &Rc<Self>) {
        let path = self.pane_root();
        if let Some(parent) = path.parent() {
            self.navigate(parent.to_path_buf(), true);
        }
    }

    pub fn go_back(self: &Rc<Self>) {
        let pos = self.history_pos.get();
        if pos == 0 {
            return;
        }
        self.history_pos.set(pos - 1);
        let target = self.history.borrow()[pos - 1].clone();
        *self.path.borrow_mut() = target;
        *self.restore.borrow_mut() = None;
        self.set_entries(Vec::new());
        Pane::reload(self);
        self.notify();
    }

    pub fn go_forward(self: &Rc<Self>) {
        let pos = self.history_pos.get();
        let len = self.history.borrow().len();
        if pos + 1 >= len {
            return;
        }
        self.history_pos.set(pos + 1);
        let target = self.history.borrow()[pos + 1].clone();
        *self.path.borrow_mut() = target;
        *self.restore.borrow_mut() = None;
        self.set_entries(Vec::new());
        Pane::reload(self);
        self.notify();
    }

    pub fn can_go_back(&self) -> bool {
        self.history_pos.get() > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_pos.get() + 1 < self.history.borrow().len()
    }

    pub fn current_dir_name(&self) -> String {
        let p = self.path();
        p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string())
    }

    pub fn item_count(&self) -> u32 {
        self.filter.n_items()
    }

    /// Rebind every row showing a file whose thumbnail just became available.
    fn finish_thumb(&self, result: ThumbResult) {
        let key = (result.path.clone(), result.mtime, result.size);
        self.thumbs.pending.borrow_mut().remove(&key);
        if !result.ok {
            // Remember the failure so a missing tool never spins up threads.
            self.thumbs.failed.borrow_mut().insert(key);
            return;
        }
        thumb::invalidate(&result.path, result.mtime, result.size);
        for pos in 0..self.store.n_items() {
            if let Some(entry) = entry_at(&self.store, pos) {
                if entry.path == result.path && thumb::mtime_key(&entry) == result.mtime {
                    self.store.items_changed(pos, 1, 1);
                }
            }
        }
    }

    /// Rebuild the icons of every row, used when preview icons are toggled.
    pub fn rebind_all(&self) {
        thumb::clear_cache();
        let count = self.store.n_items();
        if count > 0 {
            self.store.items_changed(0, count, count);
        }
    }

    /// Called after a filesystem mutation so the view refreshes without losing
    /// the current scroll position or selection.
    pub fn soft_refresh(self: &Rc<Self>) {
        if self.refreshing.get() {
            return;
        }
        self.refreshing.set(true);
        // Remember which paths were selected; they are reselected when the new
        // listing arrives instead of waiting for an arbitrary timeout.
        let path = self.path();
        let selected = self
            .selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        *self.restore.borrow_mut() = Some((path.clone(), selected));
        let sender = self.sender.clone();
        let show_hidden = self.show_hidden.get();
        std::thread::Builder::new()
            .name("nook-list".into())
            .spawn(move || {
                let entries = fsutil::list_dir(&path, show_hidden);
                let _ = sender.send_blocking(LoadResult { path, entries });
            })
            .ok();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(pane) = weak.upgrade() {
                pane.refreshing.set(false);
            }
        });
    }
}

/// Append a loaded subtree to a flattened recursive listing. Depth is capped so
/// a pathological tree cannot overflow the `u8` used for indentation.
fn append_subtree(
    kids: &[FileEntry],
    expanded: &HashSet<PathBuf>,
    children: &HashMap<PathBuf, Vec<FileEntry>>,
    parent_depth: u8,
    out: &mut Vec<FileEntry>,
) {
    for kid in kids {
        let mut kid = kid.clone();
        kid.depth = parent_depth.saturating_add(1).min(16);
        out.push(kid.clone());
        if kid.is_dir && expanded.contains(&kid.path) {
            if let Some(deep) = children.get(&kid.path) {
                append_subtree(deep, expanded, children, kid.depth, out);
            }
        }
    }
}

/// Collect the visible item widgets in `widget` with their model positions.
/// Rows and cells carry their position in widget data, which is what makes
/// rubber-band hit testing possible without private GTK internals.
fn collect_item_rects(
    widget: &gtk::Widget,
    target: &gtk::Widget,
    out: &mut Vec<(u32, gtk::graphene::Rect)>,
) {
    if let Some(bounds) = widget.compute_bounds(target) {
        // SAFETY: the only writer is our own factory bind, with the same type.
        if let Some(pos) = unsafe { widget.data::<u32>("nook-pos") } {
            let pos = unsafe { *pos.as_ptr() };
            out.push((pos, bounds));
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect_item_rects(&current, target, out);
        child = current.next_sibling();
    }
}

/// True when the point lands on the view background instead of an item cell.
fn is_empty_space(stack: &gtk::Stack, x: f64, y: f64) -> bool {
    let Some(mut current) = stack.pick(x, y, gtk::PickFlags::DEFAULT) else {
        return true;
    };
    loop {
        if current.has_css_class("nook-file") || current.has_css_class("nook-grid-cell") {
            return false;
        }
        // Column headers and scrollbars are chrome, not empty working-space
        // area: clicking them sorts or scrolls and must not clear selection.
        if current.css_name() == "header" || current.is::<gtk::Scrollbar>() {
            return false;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return true,
        }
    }
}

/// Apply the themed icon for `entry`, using a recoloured folder icon when the
/// folder style gives this path a colour.
fn apply_entry_icon(icon: &gtk::Image, entry: &FileEntry, folder_style: &FolderStyle, size: i32) {
    if entry.is_dir {
        if let Some(color) = folder_style.color_for(&entry.path) {
            if let Some(texture) = icons::folder_texture(color, size) {
                icon.set_paintable(Some(&texture));
                icon.set_pixel_size(size);
                return;
            }
        }
    }
    icon.set_icon_name(Some(&fsutil::icon_name(entry)));
    icon.set_pixel_size(size);
}

/// Drag the whole selection: pressing an unselected row selects it first, then
/// every selected path travels as `text/uri-list`.
fn add_drag_source(widget: &impl IsA<gtk::Widget>, item: &gtk::ListItem, sel: gtk::MultiSelection) {
    let drag = gtk::DragSource::new();
    drag.set_actions(gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY);
    let item = item.clone();
    drag.connect_prepare(move |_, _, _| {
        let pos = item.position();
        if !sel.selection().contains(pos) {
            sel.unselect_all();
            sel.select_item(pos, true);
        }
        let model = sel.model()?;
        let bitset = sel.selection();
        let mut uris = String::new();
        for position in 0..model.n_items() {
            if !bitset.contains(position) {
                continue;
            }
            let Some(entry) = entry_at(&model, position) else {
                continue;
            };
            uris.push_str(&gio::File::for_path(&entry.path).uri());
            uris.push('\n');
        }
        if uris.is_empty() {
            return None;
        }
        Some(gtk::gdk::ContentProvider::for_bytes(
            "text/uri-list",
            &glib::Bytes::from(uris.as_bytes()),
        ))
    });
    widget.add_controller(drag);
}

/// Dropping onto a folder moves inside it; dropping onto a file moves next to
/// it, in the folder the pane is showing.
fn add_drop_target(
    widget: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    ctx: Rc<RefCell<Option<DropCallback>>>,
) {
    let target = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY,
    );
    // Highlight only real folder targets while a drag hovers over them.
    {
        let highlight: gtk::Widget = widget.clone().upcast();
        let item = item.clone();
        target.connect_enter(move |_, _, _| {
            let is_dir = item
                .item()
                .and_then(entry_of)
                .map(|entry| entry.is_dir)
                .unwrap_or(false);
            if is_dir {
                highlight.add_css_class("nook-drop-target");
            }
            gtk::gdk::DragAction::MOVE
        });
        let highlight: gtk::Widget = widget.clone().upcast();
        target.connect_leave(move |_| {
            highlight.remove_css_class("nook-drop-target");
        });
    }
    let item = item.clone();
    let highlight: gtk::Widget = widget.clone().upcast();
    target.connect_drop(move |_, value, _, _| {
        highlight.remove_css_class("nook-drop-target");
        let Ok(list) = value.get::<gtk::gdk::FileList>() else {
            return false;
        };
        let sources = file_list_paths(&list);
        if sources.is_empty() {
            return false;
        }
        let entry = item.item().and_then(entry_of);
        let destination = match entry {
            Some(entry) if entry.is_dir => Some(entry.path),
            _ => None,
        };
        let callback = ctx.borrow().clone();
        let Some(callback) = callback else {
            return false;
        };
        callback(sources, destination);
        true
    });
    widget.add_controller(target);
}

/// Local paths carried by a `text/uri-list` drop.
fn file_list_paths(list: &gtk::gdk::FileList) -> Vec<PathBuf> {
    list.files().iter().filter_map(|file| file.path()).collect()
}

/// Replace the generic file icon with a preview thumbnail when the category is
/// switched on and one exists (or can be generated off-thread).
fn apply_thumb(
    image: &gtk::Image,
    entry: &FileEntry,
    size: i32,
    config: &Rc<RefCell<Config>>,
    thumbs: &ThumbHandle,
) {
    let Some(kind) = thumb::kind_for(entry) else {
        return;
    };
    let flags = thumb::Flags::from_config(&config.borrow());
    if !flags.allows(kind) {
        return;
    }
    let mtime = thumb::mtime_key(entry);
    if let Some(texture) = thumb::texture_for(&entry.path, mtime, size, kind) {
        image.set_paintable(Some(&texture));
        image.set_pixel_size(size);
    } else {
        thumbs.request(entry, size, kind);
    }
}

fn build_list_view(
    sel: &gtk::MultiSelection,
    context: Rc<RefCell<Option<Rc<dyn Fn(&FileEntry, f64, f64)>>>>,
    folder_style: Rc<RefCell<FolderStyle>>,
    config: Rc<RefCell<Config>>,
    thumbs: ThumbHandle,
    drop_ctx: Rc<RefCell<Option<DropCallback>>>,
    expanded: Rc<RefCell<HashSet<PathBuf>>>,
    recursive: Rc<Cell<bool>>,
    expand_cb: Rc<RefCell<Option<Rc<dyn Fn(PathBuf, bool)>>>>,
) -> gtk::ColumnView {
    let view = gtk::ColumnView::new(Some(sel.clone()));
    view.add_css_class("nook-list");
    view.set_show_row_separators(false);
    view.set_show_column_separators(false);
    view.set_single_click_activate(false);

    // Name column
    let name_factory = gtk::SignalListItemFactory::new();
    let sel_setup = sel.clone();
    let expanded_setup = expanded.clone();
    name_factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("nook-file");
        // Expander for the lazy recursive tree; hidden outside that view.
        let expander = gtk::Button::new();
        expander.add_css_class("nook-tree-toggle");
        expander.set_focus_on_click(false);
        expander.set_valign(gtk::Align::Center);
        expander.set_has_frame(false);
        expander.set_child(Some(&gtk::Image::from_icon_name("pan-end-symbolic")));
        expander.set_visible(false);
        let icon = gtk::Image::new();
        icon.set_pixel_size(18);
        icon.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(None);
        label.add_css_class("nook-file-name");
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        // Parent folder of a recursive result, so every row has context.
        let parent = gtk::Label::new(None);
        parent.add_css_class("nook-meta");
        parent.add_css_class("nook-file-parent");
        parent.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        parent.set_visible(false);
        let badge = gtk::Label::new(None);
        badge.add_css_class("nook-meta");
        let link = gtk::Image::from_icon_name("emblem-symbolic-link-symbolic");
        link.set_pixel_size(11);
        link.set_tooltip_text(Some("Symbolic link"));
        link.set_visible(false);
        row.append(&expander);
        row.append(&icon);
        row.append(&label);
        row.append(&parent);
        row.append(&link);
        row.append(&badge);

        {
            let item = item.clone();
            let state = expanded_setup.clone();
            let callback = expand_cb.clone();
            expander.connect_clicked(move |_| {
                let Some(entry) = item.item().and_then(entry_of) else {
                    return;
                };
                if !entry.is_dir {
                    return;
                }
                let expand = !state.borrow().contains(&entry.path);
                let callback = callback.borrow().clone();
                if let Some(callback) = callback {
                    callback(entry.path, expand);
                }
            });
        }

        let item_for_click = item.clone();
        let sel_for_click = sel_setup.clone();
        let ctx = context.clone();
        let row_for_click = row.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        gesture.connect_pressed(move |_, _, x, y| {
            let pos = item_for_click.position();
            let item_obj = item_for_click.item();
            // Right-clicking inside an existing multi-selection keeps it, so
            // actions apply to every selected item.
            if !sel_for_click.selection().contains(pos) {
                sel_for_click.unselect_all();
                sel_for_click.select_item(pos, true);
            }
            let Some(entry) = item_obj.and_then(entry_of) else {
                return;
            };
            let (wx, wy) = window_coords(&row_for_click, x, y);
            if let Some(cb) = ctx.borrow().as_ref() {
                cb(&entry, wx, wy);
            }
        });
        row.add_controller(gesture);
        add_drag_source(&row, item, sel_setup.clone());
        add_drop_target(&row, item, drop_ctx.clone());
        item.set_child(Some(&row));
    });
    let style_for_bind = folder_style.clone();
    let expanded_for_bind = expanded.clone();
    name_factory.connect_bind(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(entry) = item.item().and_then(entry_of) else {
            return;
        };
        // Position is needed by rubber-band hit testing.
        // SAFETY: only our own bind writes this key, with the same type.
        unsafe { row.set_data("nook-pos", item.position()) };
        let tree = recursive.get();
        let indent = if tree { entry.depth as i32 * 16 } else { 0 };
        let mut child = row.first_child();
        if let Some(expander) = child.clone().and_downcast::<gtk::Button>() {
            let has_kids = tree && entry.is_dir;
            expander.set_visible(has_kids);
            if has_kids {
                expander.set_margin_start(indent);
                let open = expanded_for_bind.borrow().contains(&entry.path);
                if let Some(image) = expander.child().and_downcast::<gtk::Image>() {
                    image.set_icon_name(Some(if open {
                        "pan-down-symbolic"
                    } else {
                        "pan-end-symbolic"
                    }));
                }
                expander.set_tooltip_text(Some(if open {
                    "Collapse this folder"
                } else {
                    "Show the contents of this folder"
                }));
            }
        }
        child = child.and_then(|c| c.next_sibling());
        if let Some(icon) = child.clone().and_downcast::<gtk::Image>() {
            let style = style_for_bind.borrow();
            apply_entry_icon(&icon, &entry, &style, 18);
            apply_thumb(&icon, &entry, 18, &config, &thumbs);
        }
        child = child.and_then(|c| c.next_sibling());
        if let Some(label) = child.clone().and_downcast::<gtk::Label>() {
            // Files have no expander, so they carry the tree indent themselves.
            label.set_margin_start(if tree && !entry.is_dir { indent } else { 0 });
            label.set_text(&entry.name);
            label.remove_css_class("nook-hidden");
            if entry.is_hidden {
                label.add_css_class("nook-hidden");
            }
        }
        child = child.and_then(|c| c.next_sibling());
        if let Some(parent) = child.clone().and_downcast::<gtk::Label>() {
            let show = tree && !entry.rel_parent.is_empty();
            parent.set_visible(show);
            parent.set_text(&format!("in {}", entry.rel_parent));
            parent.set_tooltip_text(Some(&entry.path.display().to_string()));
        }
        child = child.and_then(|c| c.next_sibling());
        if let Some(link) = child.clone().and_downcast::<gtk::Image>() {
            link.set_visible(entry.is_symlink);
            link.remove_css_class("nook-error");
            if entry.symlink_broken {
                link.add_css_class("nook-error");
            }
        }
        child = child.and_then(|c| c.next_sibling());
        if let Some(badge) = child.and_downcast::<gtk::Label>() {
            match entry.git {
                Some(crate::fsutil::GitState::Modified) => {
                    badge.set_text("M");
                    badge.remove_css_class("nook-git-added");
                    badge.add_css_class("nook-git-modified");
                }
                Some(crate::fsutil::GitState::Added) | Some(crate::fsutil::GitState::Untracked) => {
                    badge.set_text("A");
                    badge.remove_css_class("nook-git-modified");
                    badge.add_css_class("nook-git-added");
                }
                Some(crate::fsutil::GitState::Deleted) => badge.set_text("D"),
                Some(crate::fsutil::GitState::Conflicted) => badge.set_text("!"),
                None => {
                    badge.set_text("");
                    badge.remove_css_class("nook-git-modified");
                    badge.remove_css_class("nook-git-added");
                }
            }
        }
    });
    let name_col = gtk::ColumnViewColumn::new(Some("Name"), Some(name_factory));
    name_col.set_expand(true);
    name_col.set_resizable(true);
    view.append_column(&name_col);

    let size_col = text_column("Size", 90.0, |e| {
        if e.is_dir {
            String::new()
        } else {
            fsutil::human_size(e.size)
        }
    });
    view.append_column(&size_col);

    let modified_col = text_column("Modified", 150.0, |e| fsutil::format_time(e.modified));
    view.append_column(&modified_col);

    let kind_col = text_column("Kind", 130.0, |e| fsutil::kind_label(e));
    view.append_column(&kind_col);

    view
}

fn text_column<F>(title: &str, width: f32, f: F) -> gtk::ColumnViewColumn
where
    F: Fn(&FileEntry) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("nook-meta");
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(entry) = item.item().and_then(entry_of) else {
            return;
        };
        // SAFETY: only our own bind writes this key, with the same type.
        unsafe { label.set_data("nook-pos", item.position()) };
        label.set_text(&f(&entry));
    });
    let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    col.set_fixed_width(width as i32);
    col.set_resizable(true);
    col
}

fn build_grid_view(
    sel: &gtk::MultiSelection,
    context: Rc<RefCell<Option<Rc<dyn Fn(&FileEntry, f64, f64)>>>>,
    folder_style: Rc<RefCell<FolderStyle>>,
    config: Rc<RefCell<Config>>,
    thumbs: ThumbHandle,
    drop_ctx: Rc<RefCell<Option<DropCallback>>>,
    expanded: Rc<RefCell<HashSet<PathBuf>>>,
    recursive: Rc<Cell<bool>>,
    expand_cb: Rc<RefCell<Option<Rc<dyn Fn(PathBuf, bool)>>>>,
) -> gtk::GridView {
    let factory = gtk::SignalListItemFactory::new();
    let setup_sel = sel.clone();
    let expanded_setup = expanded.clone();
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        // A fixed square slot keeps every tile identical in every state; the
        // hover highlight, selection and previews can never change its size.
        // `halign: Center` stops the tile from stretching to the column width
        // (columns always divide the full viewport), which would make it wider
        // than tall.
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
        cell.add_css_class("nook-grid-cell");
        cell.set_size_request(GRID_CELL_WIDTH, GRID_CELL_WIDTH);
        cell.set_hexpand(false);
        cell.set_halign(gtk::Align::Center);
        cell.set_valign(gtk::Align::Start);
        cell.set_overflow(gtk::Overflow::Hidden);

        // The thumbnail lives in a fixed 64x64 frame, so a wide video frame or
        // a tall PDF page cannot stretch the tile.
        let frame = gtk::Overlay::new();
        frame.set_size_request(GRID_ICON_SIZE, GRID_ICON_SIZE);
        frame.set_halign(gtk::Align::Center);
        frame.set_valign(gtk::Align::Start);
        let image = gtk::Image::new();
        image.set_pixel_size(GRID_ICON_SIZE);
        image.add_css_class("nook-grid-thumb");
        image.set_halign(gtk::Align::Center);
        image.set_valign(gtk::Align::Center);
        frame.set_child(Some(&image));

        let expander = gtk::Button::new();
        expander.add_css_class("nook-tree-toggle");
        expander.set_has_frame(false);
        expander.set_focus_on_click(false);
        expander.set_halign(gtk::Align::Start);
        expander.set_valign(gtk::Align::Start);
        expander.set_child(Some(&gtk::Image::from_icon_name("pan-end-symbolic")));
        expander.set_visible(false);
        frame.add_overlay(&expander);

        // A single fixed-height line keeps the tile's height constant even for
        // very long names; the full name is in the tile tooltip.
        let label = gtk::Label::new(None);
        label.set_size_request(-1, 20);
        label.set_valign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_justify(gtk::Justification::Center);
        label.set_lines(1);
        label.set_width_chars(1);
        label.set_max_width_chars(14);
        cell.append(&frame);
        cell.append(&label);

        {
            let item = item.clone();
            let state = expanded_setup.clone();
            let callback = expand_cb.clone();
            expander.connect_clicked(move |_| {
                let Some(entry) = item.item().and_then(entry_of) else {
                    return;
                };
                if !entry.is_dir {
                    return;
                }
                let open = !state.borrow().contains(&entry.path);
                let callback = callback.borrow().clone();
                if let Some(callback) = callback {
                    callback(entry.path, open);
                }
            });
        }

        let item_for_menu = item.clone();
        let sel_for_menu = setup_sel.clone();
        let ctx = context.clone();
        let cell_for_menu = cell.clone();
        let menu_gesture = gtk::GestureClick::new();
        menu_gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        menu_gesture.connect_pressed(move |_, _, x, y| {
            let pos = item_for_menu.position();
            let item_obj = item_for_menu.item();
            // Keep an existing multi-selection intact for bulk actions.
            if !sel_for_menu.selection().contains(pos) {
                sel_for_menu.unselect_all();
                sel_for_menu.select_item(pos, true);
            }
            let Some(entry) = item_obj.and_then(entry_of) else {
                return;
            };
            let (wx, wy) = window_coords(&cell_for_menu, x, y);
            if let Some(cb) = ctx.borrow().as_ref() {
                cb(&entry, wx, wy);
            }
        });
        cell.add_controller(menu_gesture);
        add_drag_source(&cell, item, setup_sel.clone());
        add_drop_target(&cell, item, drop_ctx.clone());
        item.set_child(Some(&cell));
    });
    let style_for_bind = folder_style.clone();
    let expanded_for_bind = expanded.clone();
    factory.connect_bind(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(entry) = item.item().and_then(entry_of) else {
            return;
        };
        // SAFETY: only our own bind writes this key, with the same type.
        unsafe { cell.set_data("nook-pos", item.position()) };
        let tree = recursive.get();
        if let Some(frame) = cell.first_child().and_downcast::<gtk::Overlay>() {
            if let Some(image) = frame.child().and_downcast::<gtk::Image>() {
                let style = style_for_bind.borrow();
                apply_entry_icon(&image, &entry, &style, GRID_ICON_SIZE);
                apply_thumb(&image, &entry, GRID_ICON_SIZE, &config, &thumbs);
            }
            if let Some(expander) = frame.last_child().and_downcast::<gtk::Button>() {
                let has_kids = tree && entry.is_dir;
                expander.set_visible(has_kids);
                if has_kids {
                    let open = expanded_for_bind.borrow().contains(&entry.path);
                    if let Some(image) = expander.child().and_downcast::<gtk::Image>() {
                        image.set_icon_name(Some(if open {
                            "pan-down-symbolic"
                        } else {
                            "pan-end-symbolic"
                        }));
                    }
                }
            }
        }
        if let Some(label) = cell.last_child().and_downcast::<gtk::Label>() {
            label.set_text(&entry.name);
            label.remove_css_class("nook-hidden");
            if entry.is_hidden {
                label.add_css_class("nook-hidden");
            }
        }
        let detail = if !entry.rel_parent.is_empty() && tree {
            format!("in {}", entry.rel_parent)
        } else if entry.is_dir {
            "Folder".to_string()
        } else {
            fsutil::human_size(entry.size)
        };
        cell.set_tooltip_text(Some(&format!("{}\n{}", entry.name, detail)));
    });

    let view = gtk::GridView::builder()
        .model(sel)
        .factory(&factory)
        .min_columns(1)
        .max_columns(32)
        .single_click_activate(false)
        .build();
    view.set_vexpand(true);
    view.set_hexpand(true);
    view
}

/// Translate widget-relative coordinates into window coordinates so popovers
/// can be anchored at the pointer.
fn window_coords(widget: &impl IsA<gtk::Widget>, x: f64, y: f64) -> (f64, f64) {
    let Some(root) = widget.root() else {
        return (x, y);
    };
    let point = widget.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32));
    match point {
        Some(p) => (p.x() as f64, p.y() as f64),
        None => (x, y),
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    fn entry(path: &str, is_dir: bool, depth: u8) -> FileEntry {
        let mut entry =
            FileEntry::from_path(std::path::Path::new(path), None).unwrap_or_else(|| FileEntry {
                path: PathBuf::from(path),
                name: path.to_string(),
                is_dir,
                is_symlink: false,
                symlink_broken: false,
                size: 0,
                modified: None,
                mime: String::new(),
                is_hidden: false,
                git: None,
                mode: 0,
                depth,
                rel_parent: String::new(),
            });
        entry.is_dir = is_dir;
        entry.depth = depth;
        entry
    }

    #[test]
    fn collapsed_folders_do_not_append_children() {
        let kids = vec![entry("/tmp/a/one.txt", false, 0)];
        let expanded = HashSet::new();
        let children = HashMap::from([(PathBuf::from("/tmp/a"), kids)]);
        let mut out = Vec::new();
        append_subtree(
            children.get(std::path::Path::new("/tmp/a")).unwrap(),
            &expanded,
            &children,
            0,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].depth, 1);
    }

    #[test]
    fn expansion_nests_and_caps_depth() {
        let deep = vec![entry("/tmp/a/b/two.txt", false, 0)];
        let kids = vec![entry("/tmp/a/b", true, 0)];
        let expanded = HashSet::from([PathBuf::from("/tmp/a/b")]);
        let children = HashMap::from([
            (PathBuf::from("/tmp/a"), kids),
            (PathBuf::from("/tmp/a/b"), deep),
        ]);
        let mut out = Vec::new();
        append_subtree(
            children.get(std::path::Path::new("/tmp/a")).unwrap(),
            &expanded,
            &children,
            0,
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].depth, 1);
        assert_eq!(out[1].depth, 2);

        // A pathologically deep chain stops indenting instead of overflowing.
        let mut out = Vec::new();
        append_subtree(
            children.get(std::path::Path::new("/tmp/a/b")).unwrap(),
            &expanded,
            &children,
            u8::MAX,
            &mut out,
        );
        assert_eq!(out[0].depth, 16);
    }

    #[test]
    fn destructive_run_commands_are_recognised() {
        assert!(crate::run_tasks::is_destructive("rm -rf target"));
        assert!(!crate::run_tasks::is_destructive("npm run dev"));
    }
}
