# NIRX

NIRX is the next product identity for the existing Nook file manager. This release
preserves the legacy `nook` configuration as a read-only fallback; user data is
not moved or deleted automatically.

NIRX is a native GTK4 file manager for Linux, written in Rust and designed for
Wayland desktops such as Hyprland and [DankMaterialShell][dms]. It follows the
Material 3 palette that Matugen/DMS generates from your wallpaper, while relying
on desktop standards (XDG directories, GIO MIME associations and `.desktop`
files) rather than KDE internals.

[dms]: https://github.com/AvengeMedia/DankMaterialShell

## Features

- **DMS theming that updates live.** NIRX reads the generated palette, watches
  the palette files with GIO file monitors and swaps its stylesheet without a
  restart. It falls back to the DMS GTK colour file and finally to a built-in
  dark palette.
- **Desktop-standard launching.** Files open through GIO, so system MIME
  associations, `.desktop` entries and `XDG_CURRENT_DESKTOP` overrides are
  respected. Open With, "set as default for this file type" and a local,
  optional per-extension override are available.
- **Icon theme support.** NIRX uses the desktop's icon theme for file types and
  prefers **Colloid** automatically when it is installed (dark/light variant
  chosen from the palette). _Tools → Icon Theme…_ switches the theme live, or
  pins one with `icon_theme` in the config; only NIRX is affected, never the
  desktop.
- **Three views, your choice.** A full icon grid, a detailed list and a lazy
  recursive tree, with a toolbar switch, _View → Icon View / Detailed View_ and
  `Ctrl+1`. The recursive view lists direct children first and loads a folder's
  contents only when its expander is clicked, so huge trees stay responsive;
  each nested row shows the relative folder it came from.
- **Previews everywhere.** Images, videos and PDFs show real thumbnails in both
  list and grid views, generated off the GTK thread, cached on disk and
  letterboxed onto a fixed square canvas. _Settings → Preview Icons…_ switches
  the three categories independently; missing tools or failed previews fall
  back to the themed file-type icon.
- **Selection that behaves.** Single, Ctrl- and Shift-click, `Ctrl+A`, and a
  visible rubber-band rectangle dragged over empty space (Ctrl keeps the
  existing selection). The selection stays highlighted when the pointer moves
  away and survives refreshes after file operations.
- **Drag and drop.** Drag one or many selected items onto a folder to move them;
  valid targets highlight, folders cannot be dropped into themselves, conflicts
  ask before overwriting, and cross-filesystem moves fall back to copy+delete.
- **Folder colours with inheritance.** Every folder follows a configurable
  default colour (Colloid blue by default) and any folder can override it from
  the right-click menu. Subfolders can inherit the nearest coloured ancestor
  (on by default, toggle under _Tools → Subfolders Inherit Colour_). Colours are
  applied to the theme's folder icon, stored in NIRX's config, and never modify
  the icon theme or `.desktop` files.
- **Recursive colour browsing.** _Tools → Folders With This Colour…_
  (`Ctrl+Shift+K`) scans the repository root for every folder that resolves to
  the current folder's colour and lists them in a filterable, keyboard-driven
  picker. `Alt+PageDown` / `Alt+PageUp` jump straight to the next / previous
  match, wrapping around.
- **Search by name or by extension.** The search bar has a mode selector:
  fuzzy name matching, or extension matching where `jp` finds `.jpg`/`.jpeg`,
  `png` finds `.png`, and `none` finds extension-less files. Works recursively
  too.
- **Organise by extension.** One menu action moves every loose file in the
  current folder into sub-folders named after its extension (`PNG/`, `PDF/`,
  `TXT/`, `No extension/`). Hidden files and existing folders are untouched and
  existing files are never overwritten, so it is safe to re-run.
- **Preview sidebar** (toggleable) with image previews, PDF first pages, video
  thumbnails, text/code snippets and file metadata, generated on worker threads
  with an on-disk cache and size limits.
- **Tabs** with independent history and selection, plus create, close, duplicate,
  reorder, and session restore.
- **Quick Shift**: keyboard-driven "move these files to…" with destination
  search over recents, bookmarks and XDG folders, conflict handling and progress.
- **File operations**: copy, move, rename, new folder, trash, restore from trash,
  symlinks, safe conflict prompts, permission errors reported per item.
- **File-based Run commands.** `fm_run_task.json` at (or above) the current
  folder defines named commands; they are loaded automatically, edited from
  NIRX, saved back atomically with validation, and run in a terminal in the
  current folder with output and exit status. Destructive commands ask first.
- **Editable path bar.** `Ctrl+L` (or the pencil button) turns the breadcrumbs
  into a field that accepts typed or pasted absolute, `~` and relative paths;
  Enter validates and navigates, Escape cancels, and files open in their parent
  folder with the file selected.
- **LocalSend, built in.** Send the selection to a phone or another computer
  from the toolbar (`Ctrl+Shift+S`): devices are discovered over the LocalSend
  protocol, transfers use HTTPS, progress and Cancel live in NIRX, and incoming
  transfers ask Accept / Decline in NIRX's own window. No LocalSend window, no
  CLI, no daemon — see [docs/localsend.md](docs/localsend.md).
- **Keyboard only.** `F10` opens a menu of dropdowns, `Ctrl+Shift+B` focuses the
  sidebar, `Shift+F10` (or the Menu key) opens the context menu on the
  selection, and the Library views are driven with arrows, `Space`, `Enter`,
  `Delete` and `Esc`. Every dialog, including Quick Shift and the LocalSend
  picker, takes arrows and Enter. Full guide:
  [docs/keyboard.md](docs/keyboard.md).
- **Compact menus.** The `…` button lists ten dropdown headings instead of one
  long scroll; each section expands on hover, `→` or Enter.
- **Power features**: fuzzy filter and recursive search, type-to-filter,
  hidden files, multiple selection, bulk rename, archive create/extract, SHA-256
  checksums, properties, terminal in the current folder, "open in Zed", Git
  status badges, configurable shortcuts.
- **Keyboard first**: type on a file list to filter it, `Ctrl+L` to go to a
  path, `Ctrl+Shift+C` to copy the selection's path, plus rebindable shortcuts
  for navigation, view switching, colour browsing and every file operation.
- **Persistence**: tabs, bookmarks, window geometry, view mode, sort order,
  sidebar (full, icon rail or hidden) and preview visibility, Quick Shift
  history, app overrides and shortcut overrides in one JSON file.
- **Responsive**: directory listings, previews, search, transfers and archive
  operations run off the main thread; listings are only rebuilt when the folder
  actually changes.

## Requirements

Runtime:

- GTK 4.18 or newer (`libgtk-4-1`), GLib/GIO, gdk-pixbuf
- A Wayland or X11 session

Build dependencies (Debian 13 "trixie"):

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libgdk-pixbuf-2.0-dev
```

Rust 1.85 or newer (`rustup` recommended).

Optional tools, detected at runtime:

| Tool                                                                        | Used for                            |
| --------------------------------------------------------------------------- | ----------------------------------- |
| `kitty` (or `foot`, `alacritty`, `wezterm`, `ghostty`, `konsole`, `ptyxis`) | "Open terminal here"                |
| `zed`                                                                       | "Open folder in Zed" (configurable) |
| `tar`, `unzip`, `7z`                                                        | Create and extract archives         |
| `pdftoppm` (poppler-utils)                                                  | PDF previews                        |
| `ffmpeg` / `ffprobe`                                                        | Video thumbnails                    |
| `git`                                                                       | Status badges                       |

NIRX never installs `.desktop` files, never edits system-wide MIME databases and
never overwrites DMS configuration.

## Build and run

```sh
cargo build --release
./target/release/nirx            # home folder
./target/release/nirx ~/Projects # start elsewhere
```

Development build and tests:

```sh
cargo run
cargo test
```

### Note about this checkout's `.cargo/config.toml`

This machine has the GTK 4 runtime but not the `-dev` headers, and `sudo`
requires a password, so GTK was installed into a project-local, rootless
sysroot. `.cargo/config.toml` points `PKG_CONFIG_PATH`/`PKG_CONFIG_SYSROOT_DIR`
at `.sysroot/`, which is why builds work without root here.

On a machine with the real development packages installed, delete both to build
against the system libraries:

```sh
rm -rf .cargo .sysroot
```

## Documentation

Detailed documentation lives in [`docs/`](docs/README.md):

| Document | Contents |
| --- | --- |
| [features.md](docs/features.md) | Every feature, what it does and how to use it |
| [keyboard.md](docs/keyboard.md) | All shortcuts and keyboard-only workflows |
| [localsend.md](docs/localsend.md) | LocalSend integration: protocol, security, ports |
| [architecture.md](docs/architecture.md) | Source map, key types, data flow, threads |
| [configuration.md](docs/configuration.md) | Every `config.json` key, `fm_run_task.json`, caches |
| [troubleshooting.md](docs/troubleshooting.md) | Common problems and fixes |

## Configuration and data

| Path                                | Contents                            |
| ----------------------------------- | ----------------------------------- |
| `$XDG_CONFIG_HOME/nirx/config.json` | All settings (see below)            |
| `fm_run_task.json` (project folder) | Per-tree Run commands (see below)   |
| `$XDG_CACHE_HOME/nirx/previews/`    | Preview thumbnails (safe to delete) |

`config.json` is written atomically (temporary file + rename) on every change
and on window close. An unreadable document is renamed to `config.json.broken`
and defaults are used, so a corrupt file never leaves NIRX unusable.

Important keys:

```jsonc
{
  "terminal": ["kitty"], // first entry that exists on PATH wins
  "editor": "zed",
  "bookmarks": [{ "name": "Projects", "path": "/home/me/Projects" }],
  "app_overrides": { ".md": "org.gnome.TextEditor.desktop" },
  "shortcuts": { "win.quick-shift": "<Control><Shift>m" },
  "preview_max_bytes": 8388608,
  "confirm_delete": true,

  "icon_theme": "Colloid-Dark", // null/absent = auto (Colloid, else desktop)
  "folder_color": "#5294e2", // null = leave the theme's own folder icon
  "folder_colors": {
    "/home/me/Projects": "#8e5bc4",
    "/home/me/Pictures": "#3fa34d",
  },
  "folder_color_inherit": true, // subfolders use the nearest coloured parent
  "view": "grid", // "grid" = icons, "list" = detailed, "recursive" = tree
  "sidebar_mini": false, // true = icon-only rail
  "show_sidebar": true,
  "thumbs_images": true, // preview icons per category
  "thumbs_videos": true,
  "thumbs_pdfs": true,
  "recent_files": ["/home/me/notes.md"],

  "transfer_enabled": true, // LocalSend discovery and receiving
  "transfer_alias": null, // null = hostname
  "transfer_port": 53317,
  "transfer_receive_dir": "~/Downloads",
  "transfer_auto_accept": false, // only trusted devices skip the prompt
  "transfer_trusted": [], // certificate fingerprints
}
```

### Run commands: `fm_run_task.json`

NIRX looks for `fm_run_task.json` in the current folder and then in each parent,
so one file at a project root serves the whole tree. Structure:

```json
{
  "version": 1,
  "commands": [
    { "name": "Dev server", "command": "npm run dev" },
    { "name": "Tests", "command": "cargo test" }
  ]
}
```

Commands are directory-specific. _Settings → Folder Run Commands…_ (or the
folder's `Run Commands…` menu) adds, edits and removes them; saving writes the
nearest file, or creates one in the current folder when none exists. The JSON is
validated before writing and an existing invalid file is reported rather than
overwritten. NIRX only runs a command you pick, shows its exact text first, and
confirms commands that can delete or overwrite data.

`app_overrides` maps a file extension (`".png"`) or MIME type
(`"image/png"`) to a desktop ID. It only affects NIRX — the system association
stays untouched. "Set as default" in the Open With dialog goes through GIO,
which writes a user-level `mimeapps.list`, never a system file.

`folder_colors` keys are absolute folder paths; values accept `#rgb`, `#rrggbb`
or no leading `#`. Removing an entry (or using **Folder Colour… → Use default
colour**) makes the folder follow `folder_color` again. Setting `folder_color`
to `null` (or **Default Folder Colour… → Theme icon**) leaves folders exactly as
the icon theme draws them.

With `folder_color_inherit` (default `true`), a folder without its own colour
uses the nearest ancestor folder that has one before falling back to
`folder_color`; the folder-colour dialog tells you which ancestor it currently
inherits from. Turn it off with _Tools → Subfolders Inherit Colour_ to make every
folder follow only its own override or the global default.

## Icons

NIRX reads the icon theme from the desktop and uses full-colour MIME icons, not
symbolic ones. When a theme named `Colloid*` is installed, it is chosen
automatically, preferring `Colloid-Dark`/`Colloid-Light` to match the wallpaper
palette; otherwise the desktop's current theme is used. Use _Tools → Icon
Theme…_ to switch at runtime (the picker lists every theme with an
`index.theme`, plus **Automatic**), or set `icon_theme` in the config.

This machine already has Colloid installed for the current user
(`~/.local/share/icons/Colloid{,-Dark,-Light}`). To install it elsewhere
without root:

```sh
git clone https://github.com/vinceliuice/Colloid-icon-theme
cd Colloid-icon-theme
./install.sh -t default -s default -d ~/.local/share/icons
```

Folder colours are produced by tinting that theme's own `folder` icon (SVG or
PNG), preserving highlights and transparency; themes that only ship symbolic
folder icons fall back to the untinted icon. Switching the icon theme clears the
tint cache and reloads folder icons immediately.

## Theming

NIRX looks for a palette in this order:

1. `~/.cache/DankMaterialShell/dms-colors.json` — the full Material 3 token set
   (`mode`, `colors.dark` / `colors.light`), preferred.
2. `~/.config/gtk-4.0/dank-colors.css` — the `@define-color` subset DMS writes
   for GTK apps.
3. A built-in dark palette, so NIRX always renders correctly.

The cache file and the GTK colour file are watched with GIO monitors; when
Matugen regenerates them (wallpaper change, theme switch) NIRX re-reads the
palette and replaces its CSS provider, debounced to avoid flicker. `mode:
"light"` switches the whole UI to the light token set.

## Keyboard shortcuts

All of these are rebindable in **F1 → Keyboard shortcuts**; overrides are stored
in `config.json`.

| Action                             | Default                             |
| ---------------------------------- | ----------------------------------- |
| New tab / close tab                | `Ctrl+T` / `Ctrl+W`                 |
| Next / previous tab                | `Ctrl+Tab` / `Ctrl+Shift+Tab`       |
| Back / forward / parent            | `Alt+Left` / `Alt+Right` / `Alt+Up` |
| Home folder                        | `Alt+Home`                          |
| Switch icon / detailed view        | `Ctrl+1`                            |
| Show hidden files                  | `Ctrl+H`                            |
| Preview pane                       | `Ctrl+Shift+P`                      |
| Sidebar visibility / icon rail     | `Ctrl+B` / sidebar header button    |
| Focus search                       | `Ctrl+F`                            |
| Edit path (inline)                 | `Ctrl+L`                            |
| Focus file list                    | menu → View                         |
| Quick Shift                        | `Ctrl+Shift+M`                      |
| New folder                         | `Ctrl+Shift+N`                      |
| Rename / bulk rename               | `F2` / `Ctrl+Shift+R`               |
| Copy / cut / paste                 | `Ctrl+C` / `Ctrl+X` / `Ctrl+V`      |
| Select all                         | `Ctrl+A`                            |
| Move to trash / delete permanently | `Delete` / `Shift+Delete`           |
| Properties                         | `Alt+Enter`                         |
| Checksums                          | menu → Tools                        |
| Open with…                         | `Ctrl+Shift+O`                      |
| Bookmark current folder            | `Ctrl+D`                            |
| Copy selection path                | `Ctrl+Shift+C`                      |
| Folders with this colour           | `Ctrl+Shift+K`                      |
| Next / previous same-colour folder | `Alt+PageDown` / `Alt+PageUp`       |
| Open terminal here                 | `Ctrl+Alt+T`                        |
| Open folder in Zed                 | toolbar button / menu               |
| Refresh                            | `Ctrl+R`                            |
| Shortcuts dialog                   | `F1`                                |
| Main menu / settings menu          | `F10` / `Ctrl+,`                    |
| Context menu for the selection     | `Shift+F10` or `Menu`               |
| Focus sidebar / file list          | `Ctrl+Shift+B` / `Ctrl+Shift+F`     |
| Send with LocalSend                | `Ctrl+Shift+S`                      |

In the file view, arrows move the cursor and `Enter` opens the focused item;
`Ctrl`/`Shift`+click extend the selection as usual. Typing a printable character
anywhere in the file list starts a filter (Backspace edits, Escape clears), and
the query is mirrored into the search bar. `Ctrl+F` focuses that bar, which
filters the current folder and can switch to recursive search with the toggle
next to it. The drop-down switches between matching file **names** and file
**extensions** (also available under _View → Search Mode_).

Right-click a folder for **Folder Colour…**; _Tools → Default Folder Colour…_
sets the colour for all folders, _Tools → Subfolders Inherit Colour_ toggles
parent-colour inheritance, and _Tools → Organise by Extension…_ tidies a messy
folder. _Tools → Folders With This Colour…_ lists every folder under the
repository root with the same effective colour; `Alt+PageDown`/`Alt+PageUp`
cycle through them without opening the dialog.

## Architecture

```
src/
  main.rs            GApplication setup, CLI argument handling
  settings.rs        Config schema, XDG paths, atomic save, tab/session types
  theme.rs           Palette discovery/parsing, CSS generation, live watcher
  fsutil.rs          Listing, sorting, Git status, trash, copy/move, naming,
                     extension grouping
  icons.rs           Icon-theme choice/picker support, colour parsing,
                     inheritance, tinted folder icons
  apps.rs            MIME resolution, overrides, launching, defaults
  run_tasks.rs       fm_run_task.json lookup, validation and atomic save
  search.rs          Fuzzy scoring and recursive search
  preview.rs         Worker-thread preview service, classification, cache
  thumb.rs           Image/video/PDF thumbnail icons, square canvas, cache
  git.rs             Git status/branch/graph parsing and repository snapshots
  localsend.rs       LocalSend protocol: discovery, TLS, sender and receiver
  ui/mod.rs          Window, toolbar, sidebar, breadcrumbs, tabs, actions
  ui/pane.rs         One directory pane: model, filter, icons/details, history
  ui/preview_pane.rs Preview sidebar and metadata rendering
  ui/library.rs      In-tab Trash, Recent Files/Locations and Network views
  ui/send.rs         LocalSend picker, progress, incoming prompt, settings
  ui/dialogs.rs      Prompts, conflicts, progress, checksums, shortcuts
  ui/quickshift.rs   Quick Shift destination picker and move flow
  ui/git_pane.rs     Git dashboard window
docs/                Feature, keyboard, architecture, config and LocalSend docs
```

`docs/architecture.md` explains what each module owns and how work moves between
threads.

Design rules followed throughout:

- **UI, navigation, filesystem, theming, previews and settings are separate
  modules**; UI code never performs blocking I/O on the main loop.
- **Destructive work is explicit**: trash and permanent delete confirm first,
  conflicts ask per operation, and errors are reported per item instead of being
  swallowed.
- **No duplicate app entries**: handlers are always GIO `AppInfo` objects keyed
  by desktop ID, so the same application never appears twice in Open With.

## Troubleshooting

**`error: Package 'gtk4' not found`** — install `libgtk-4-dev`, or if you are on
this machine keep `.cargo/config.toml`/`.sysroot` in place and rebuild.

**Window opens but looks unstyled / monochrome** — the DMS cache file may not
exist yet. Run Matugen once (`matugen image <wallpaper>`) or set a wallpaper in
DMS; NIRX falls back to its built-in palette until then.

**Theme does not change when the wallpaper changes** — verify that
`~/.cache/DankMaterialShell/dms-colors.json` is actually rewritten (watch it with
`inotifywait`). NIRX watches the cache directory, so a palette written by a
process on a different mount namespace or a bind-mounted file may not generate
events; restart NIRX in that case.

**"Open terminal here" does nothing** — NIRX uses the first existing terminal
from `terminal` in `config.json` (`kitty` by default), then common fallbacks,
then `xterm`. Install `kitty` or set `"terminal": ["/usr/bin/your-terminal"]`.

**PDF or video previews show a file icon** — install `poppler-utils` and/or
`ffmpeg`. Previews fall back to metadata when the tool is missing.

**`GtkGizmo ... (slider) reported min width -2`** — a benign GTK 4.18 warning
emitted while measuring scrollbars, reproducible without NIRX's stylesheet. It
does not affect behavior.

**"No application registered for …"** — the MIME type has no handler. Use
_Open with…_ and pick an application; choose "Set as default" if you want that
choice applied system-wide for your user.

**NIRX is not using Colloid** — verify that a folder containing `index.theme`
exists under `~/.local/share/icons/Colloid*`, `~/.icons/Colloid*` or
`/usr/share/icons/Colloid*`. NIRX only looks for an installed theme; it never
downloads one. _Tools → Icon Theme…_ shows what it found and lets you pick it
explicitly, or set `"icon_theme"` in the config.

**Folders ignore the chosen colour** — the active icon theme must ship a
non-symbolic `folder` icon (SVG or PNG) that can be tinted; symbolic-only
themes fall back to the plain icon. Choose _Default Folder Colour… → Theme
icon_ to turn tinting off entirely.

**Organise by extension moved nothing** — it only handles plain files directly
inside the current folder; sub-folders, hidden files and files whose target
already exists are skipped, and a summary toast reports the count.

**Folders with this colour finds too much / nothing** — the scan is rooted at
the nearest ancestor that contains `.git`, so run it inside the repository.
It matches the folder's _effective_ colour, which includes inherited and default
colours; when every folder uses the default, every folder matches. Set a
specific colour on a parent and rely on inheritance for narrower results. The
scan caps at 20 000 directories and 5 000 matches and skips hidden directories,
`.git` and symlinks.

**Type-to-filter does not start** — the file list needs focus: click a row or use
_View → Focus File List_. The filter is mirrored in the search bar; Escape clears
it and returns focus to the list.

## Status

Working on Debian 13 / Wayland: window, DMS palette following (including live
reload), directory browsing in icon, detailed and lazy recursive views, sorting,
hidden files, fuzzy name search and extension search (including recursive),
tabs with adaptive widths, minimum width, scrolling and restore, bookmarks,
preview pane with cached asynchronous previews, preview-based icons for images,
videos and PDFs, core file operations with conflict handling, drag and drop
with drop highlighting and overwrite prompts, trash, recent files and
locations, and network locations shown in the current tab with a bottom action
bar for restore and permanent delete, Quick Shift,
bulk rename, checksums, archives, symlinks, properties, terminal and Zed
integration, Git badges and a live-updating commit graph window, inline path
editing, file-based Run commands, shortcut customisation, themed full-colour
MIME icons with Colloid auto-detection, tinted per-folder colours with a
configurable default and subfolder inheritance, recursive same-colour browsing
with quick next/previous jumps, a live icon-theme picker, type-to-filter,
segmented view switching, and one-click organisation of a folder by extension.

Verification for the changes in this release: `cargo build` succeeds (one
pre-existing dead-code warning for two unused pane fields) and `cargo test`
passes 44 tests, including a real LocalSend transfer between two services on
loopback (self-signed TLS, `/prepare-upload`, tokens, `/upload`, SHA-256
verification), including new tests for run-task validation,
recursive tree flattening, the fixed working-surface/selection CSS, a uniform
grid-tile padding assertion, and a snapshot of a real repository containing a
two-parent merge. A temporary GTK probe confirmed the tab strip's geometry: one
tab fills the bar, N tabs split it equally (892 px → 892 / 445 + 445 / 222 × 4),
tabs stop at the 170 px floor and the bar scrolls instead of shrinking further.
The binary was launched headlessly and with the icon rail, all three views,
preview pane and thumbnails enabled without warnings or panics. Interactive
behaviours — click/drag selection, drag and drop and the Git window content —
could not be exercised through automated clicks in this environment, so they
are implemented and reviewed against GTK 4.18's own selection and list-item
sources but not machine-driven here.

Limitations: LocalSend text/clipboard messages and the browser-download mode
are not implemented, and a transfer is only offered to you while the other
device is running its LocalSend app (or NIRX). There is no undo stack beyond the
trash, archive operations cannot be cancelled once started, and trashing requires a freedesktop-compatible trash
directory (the usual `~/.local/share/Trash`) — on volumes without one, deletion
is reported as an error rather than silently falling back to permanent delete.
The recursive view loads one level per expand and caps indentation depth; a
folder that cannot be read simply expands to nothing. Network browsing hands
non-local URIs to the desktop (gvfs), since NIRX's panes browse local folders.
