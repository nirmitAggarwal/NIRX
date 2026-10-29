# Architecture

How the code is organised, which type owns what, and how work moves between
threads. Read this together with the module headers in the source: every file
starts with a short explanation of its own job.

## Shape of the program

```
                      ┌─────────────────────────────┐
                      │           main.rs           │
                      │  gtk::Application, CLI dir  │
                      └──────────────┬──────────────┘
                                     │ ui::build(app)
                     ┌───────────────▼────────────────┐
                     │            ui/mod.rs           │
                     │  AppUi: window, toolbar, tabs, │
                     │  sidebar, status bar, actions  │
                     └───┬────────┬─────────┬─────────┘
        ┌────────────────┘        │         └────────────────┐
        ▼                         ▼                          ▼
   ui/pane.rs               ui/library.rs              ui/send.rs
   one directory view     Trash/recents/network      LocalSend dialogs
        │                         │                          │
        └───────────┬─────────────┴──────────┬───────────────┘
                    ▼                        ▼
             fsutil.rs, icons.rs,      localsend.rs
             apps.rs, preview.rs,     protocol client/server
             thumb.rs, search.rs,
             git.rs, run_tasks.rs, settings.rs, theme.rs
```

`ui` talks to everything; nothing below `ui` ever touches a widget.

## Files

| File | Responsibility |
| --- | --- |
| `main.rs` | `gtk::Application`, `HANDLES_OPEN`, the `app.quit` action, resolving the optional command-line directory, `ui::build` |
| `settings.rs` | `Config` and its parts, XDG paths, `expand_path`, atomic save/load, history helpers |
| `theme.rs` | DMS/Material 3 palette loading, the generated stylesheet, `ThemeWatcher` (GIO file monitors), live reload |
| `icons.rs` | Icon-theme choice (Colloid preference), colour parsing, folder-icon tinting, `FolderStyle` |
| `fsutil.rs` | `FileEntry`, listing and sorting, crumbs, size formatting, copy/move with conflicts and progress, trash/restore/purge, organise-by-extension, `which`, archive detection |
| `apps.rs` | MIME resolution, registered applications, launching, setting defaults, custom opener commands |
| `preview.rs` | `PreviewService`: worker threads, classification, disk cache, text snippets |
| `thumb.rs` | Real thumbnails for images (`gdk-pixbuf`), videos (`ffmpeg`) and PDFs (`pdftoppm`), composited onto a square canvas |
| `search.rs` | Fuzzy name scoring (`fuzzy_score`) and extension matching |
| `run_tasks.rs` | `fm_run_task.json` lookup, validation, atomic save, destructive-command detection |
| `git.rs` | `git` invocation, status/branch/graph parsing, repository snapshot for the dashboard |
| `localsend.rs` | The LocalSend protocol: discovery, TLS, HTTP client and server, sessions and tokens |
| `ui/mod.rs` | `AppUi`: window, header (tabs + toolbar), sidebar, breadcrumbs, status bar, preview wiring, actions, shortcuts, menus, session persistence |
| `ui/pane.rs` | `Pane`: one directory view — model, filters, list/grid/recursive views, history, selection, rubber band, drag and drop, loading |
| `ui/library.rs` | `LibraryKind` and `LibraryView`: Trash, Recent Files, Recent Locations, Network, with the bottom action bar |
| `ui/preview_pane.rs` | The preview sidebar widget and its metadata rendering |
| `ui/send.rs` | LocalSend picker, send progress, incoming transfer prompt, transfer settings |
| `ui/dialogs.rs` | Every modal dialog: confirm, prompts, Open With, properties, bulk rename, checksums, conflicts, progress, shortcuts, icon theme, colours, folder commands |
| `ui/quickshift.rs` | The Quick Shift picker and its move flow |
| `ui/git_pane.rs` | The Git dashboard window |

## Key types

### `AppUi` (`ui/mod.rs`)

One per window. It owns the widgets that belong to the window rather than to a
tab — toolbar buttons, breadcrumbs, the path field, the status bar, the sidebar,
the preview pane, the tab strip, the toast and the LocalSend service — and
exposes the actions every other module calls (`add_tab`, `set_view`,
`open_folder_here`, `trash_selection`, `send_selection`, …). It is an
`Rc<AppUi>`; signal handlers hold only `Weak` references, and a thread-local
`LIVE_STATE` list keeps the one strong reference that gives it its lifetime.

### `Tab` (`ui/mod.rs`)

A tab is a `gtk::Stack` holding the folder view, plus the folder's
[`Pane`](#pane-uipaners), a `Cell<Option<LibraryKind>>` for the library state it
may be presenting instead, the folder the library view was opened from, a
refresh closure for that view, and the widgets of its strip entry (title,
location, icon, tooltip).

Everything else in the window (`chrome_path`, status bar, preview note, toolbar
sensitivity) is derived from the active tab by `refresh_chrome`.

### `Pane` (`ui/pane.rs`)

One directory. It owns:

- a `gio::ListStore` of all entries and a `CustomFilter` over them (name or
  extension search, recursive mode),
- two independent selections (`MultiSelection` for the list view and for the
  grid) over the same filter model,
- a `ColumnView` (details) and a `GridView` (icons), each in its own
  `ScrolledWindow`, switched by an inner `Stack`,
- history (`back`/`forward`/`navigate`), sort key, view mode, hidden-file state,
- an overlay with the empty/loading placeholder and the rubber-band rectangle,
- callbacks the window wires up: `on_changed`, `on_open`, `on_context`,
  `set_on_notice`, `set_on_search`, `set_on_drop`.

### `LibraryView` (`ui/library.rs`)

The widget for one library state: a list plus a bottom action bar. It reports a
`refresh` closure (rebuild rows + status line) and a `focus` closure (hand the
keyboard to the list). Rows carry a `RowItem` (`Trashed`, `File`, `Folder`,
`Uri`), which is what the action bar acts on.

### `Config` (`settings.rs`)

Everything persisted. See [configuration.md](configuration.md).

## Data flow

### Opening a tab

```
sidebar / menu / Ctrl+T
   → AppUi::add_tab(path, view, activate)
       → Pane::new(...)                 creates models and both views
       → AppUi::wire_pane(pane)         connects pane callbacks to the window
       → Tab { stack, pane, strip… } appended to notebook + tab strip
       → refresh_chrome()               breadcrumbs, sidebar, status bar, preview
```

### Loading a folder

```
Pane::navigate / reload
   → worker thread reads the directory (std::fs, git status per folder)
   → async_channel::Sender<LoadResult>
   → glib::spawn_future_local on the GTK main loop
   → Pane::apply_load: replace the store, restore the selection, update counts
   → notify() → AppUi::refresh_chrome
```

Nothing blocking ever runs on the main loop: directory reads, Git status,
transfers, previews, thumbnail generation and archive work all live on worker
threads and hand results back over `async_channel` (Gapless/asynchronous, no
runtime required), drained by `glib::spawn_future_local`.

### Showing a library view

```
sidebar row / menu
   → AppUi::open_library(kind)
       → library::build(ui, kind) → LibraryView { root, refresh, focus }
       → replace the stack's "library" child, switch to it
       → mark the tab: library = Some(kind), library_path = pane.path()
       → refresh_chrome() → refresh_library_chrome(kind, tab)
       → focus()
```

`refresh_chrome` compares the pane's path with `library_path` on every update:
navigating anywhere (Place, breadcrumb, path field, history) clears the library
state and shows the folder again, which is why the views behave like a mode of
the tab rather than a separate window.

### Sending with LocalSend

```
send button / Ctrl+Shift+S
   → ui::send::send_selection → picker window
       → localsend::Service::announce()          UDP, multicast + unicast
       → peers answer POST /register             → Inner::remember → picker poll
   → send_to(...)                                  worker thread
       → Service::send_files
           → collect_files / sha256 / metadata
           → HTTPS POST /prepare-upload            → sessionId + tokens
           → HTTPS POST /upload per file           → progress + cancel
       → ProgressMsg over async_channel → progress dialog
       → toast on the main loop
```

Receiving works the other way round: the service's own HTTPS server answers
`/prepare-upload` by sending `Event::Incoming` to the main loop, which shows the
prompt; the answer travels back over the `decide` channel and the connection
thread either writes the files or refuses.

## Threads

| Thread | Created by | Work |
| --- | --- | --- |
| GTK main | `main.rs` | Widgets, models, actions, all UI decisions |
| Directory load | `pane.rs` per load | Reading a folder, Git status per entry |
| Preview | `preview.rs` pool | Raster previews, text snippets, cache writes |
| Thumbnail | `thumb.rs` | `ffmpeg`/`pdftoppm`/`gdk-pixbuf` preview icons |
| Transfers | `ui/mod.rs` | Copy/move/delete with progress |
| Archive | `ui/mod.rs` | `tar`/`zip`/`7z` create and extract |
| LocalSend server | `localsend.rs` | Accept loop, one thread per connection |
| LocalSend discovery | `localsend.rs` | UDP multicast loop, periodic announcements |
| LocalSend send | `ui/send.rs` | One thread per outgoing transfer |
| Git dashboard | `ui/git_pane.rs` | Refreshing the repository view |

Rules that keep this safe:

1. Widgets are only touched on the GTK main loop. Worker threads send on an
   `async_channel`; a future started with `glib::spawn_future_local` applies the
   result.
2. Blocking sockets live in `localsend.rs` and are only reached from its own
   threads.
3. Cancellation is a `std::sync::atomic::AtomicBool` shared with the dialog's
   Cancel button.
4. `Rc`/`RefCell` for the UI, `Arc`/`Mutex` for anything crossing a thread.

## Styling

`theme.rs` builds the whole stylesheet from the palette and installs it as a
single `gtk::CssProvider`. Classes are prefixed `nook-`: `nook-surface` for the
working area, `nook-toolbar`, `nook-crumb`, `nook-tab`, `nook-library`,
`nook-library-actions` for the library action bar, `nook-dialog`, `nook-meta`,
`nook-btn`, `nook-action`, `nook-suggested`, `nook-destructive`,
`nook-empty-title`, and so on. `ThemeWatcher` re-applies the stylesheet when the
palette files change (the provider is replaced, not stacked).

## Tests

`cargo test` runs everything headlessly — no display is required.

| Module | What is covered |
| --- | --- |
| `theme` | The fixed dark file surface, selection styling, square grid tiles |
| `search` | Fuzzy scoring order, extension matching |
| `apps` | Custom opener precedence, placeholder quoting |
| `fsutil` | Organise by extension |
| `icons` | Colour parsing, tinting, Colloid variant choice |
| `preview` | Cache keys, text snippet limits |
| `run_tasks` | Lookup walking up, round-trip save, malformed files |
| `git` | Status/branch/graph parsing on a real repository |
| `ui::pane` | Recursive tree building and depth caps |
| `localsend` | **A real transfer**: a sender and a receiver start on loopback, the receiver accepts, two files (one nested) arrive byte-identical, plus name sanitising, unique names, folder paths, address parsing and URL escaping |

The LocalSend test is the end-to-end check for that integration: TLS with a
self-signed certificate, `/prepare-upload`, tokens, `/upload`, SHA-256
verification and the completion event all run in it.

UI behaviour (library states, the action bar, the menus, the incoming prompt)
was verified with temporary GTK probes during development; they are not kept in
the tree because they need a display, which `cargo test` should not require.

## Conventions

- A doc comment at the top of every file states what it is for.
- Comments explain *why* a decision was made (hysteresis on sidebar thresholds,
  no keep-alive in the LocalSend client, square thumbnail canvas, …), not what
  the next line does.
- User-visible strings are plain sentences; errors name the file and the
  operation ("Cannot open /home/me/x: Permission denied").
- Anything destructive asks first, reports per item, and never half-succeeds
  silently.
- New behaviour comes with a test wherever it does not need a display.
