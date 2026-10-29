# Features

Every feature of NIRX, what it does and how to reach it. Shortcuts are the
defaults; all of them can be rebound (see
[keyboard.md](keyboard.md#rebinding)).

## Contents

- [Theming](#theming)
- [Tabs](#tabs)
- [Views](#views)
- [Navigation](#navigation)
- [Sidebar](#sidebar)
- [Adaptive layout](#adaptive-layout)
- [Selection](#selection)
- [Search and filtering](#search-and-filtering)
- [Sorting](#sorting)
- [File operations](#file-operations)
- [Drag and drop](#drag-and-drop)
- [Archives](#archives)
- [Checksums and properties](#checksums-and-properties)
- [Preview pane](#preview-pane)
- [Preview icons (thumbnails)](#preview-icons-thumbnails)
- [Folder colours](#folder-colours)
- [Library views: Trash, recents, network](#library-views-trash-recents-network)
- [Quick Shift](#quick-shift)
- [Bulk rename](#bulk-rename)
- [Run commands](#run-commands)
- [Git integration](#git-integration)
- [Opening files, terminal and Zed](#opening-files-terminal-and-zed)
- [LocalSend transfers](#localsend-transfers)
- [The menus](#the-menus)
- [Persistence](#persistence)

## Theming

NIRX reads the Material 3 palette that Matugen / DankMaterialShell generate from
the wallpaper (`~/.cache/DankMaterialShell/` and the DMS GTK colour file), and
falls back to a built-in dark palette. Palette files are watched with GIO file
monitors, so changing the wallpaper re-themes NIRX while it runs — no restart.
The file surface stays a fixed dark colour so thumbnails and text keep their
contrast; every other colour follows the palette.

Nothing system-wide is changed: NIRX never edits `.desktop` files, the icon
theme or the DMS configuration.

## Tabs

Each tab is a folder with its own history, selection, sort and view mode.

| Action | Where |
| --- | --- |
| New tab | `Ctrl+T`, `+` in the tab strip, *Tabs → New Tab* |
| Close tab | `Ctrl+W`, `×` on the tab |
| Duplicate | *Tabs → Duplicate Tab* |
| Reorder | drag a tab along the strip |
| Switch | `Ctrl+Tab` / `Ctrl+Shift+Tab`, or click |

Closing the last tab opens your home folder, so there is always a working
surface. The 16 most recent tabs are described in the config and restored on the
next start, together with the active tab.

## Views

Three layouts, switched with the toolbar button, `Ctrl+1`, or *View*:

- **Icon view** — a grid of tiles with thumbnails and names.
- **Detailed view** — a list with name, size, kind, modified time and Git status.
- **Recursive view** — the current folder plus a lazily expanded tree of its
  descendants. Children load only when an expander is opened, so a huge tree
  stays responsive; nested rows show the folder they came from.

The chosen view is per tab and remembered for the session.

## Navigation

- Back / forward / parent: toolbar arrows, `Alt+Left`, `Alt+Right`, `Alt+Up`.
- Home: `Alt+Home`.
- **Breadcrumbs** in the toolbar: every segment is a button.
- **Editable path**: `Ctrl+L` (or the pencil icon) turns the breadcrumbs into a
  field. Absolute paths, `~/…` and relative paths work; Enter validates and
  navigates, a file path opens its folder with the file selected, Escape
  cancels, and a bad path is reported in the field without navigating.
- **Bookmarks**: `Ctrl+D` toggles a bookmark for the current folder; bookmarks
  appear in the sidebar between Places and Library.
- Single-click navigates nothing; double-click (or Enter) opens.

## Sidebar

Four groups: **Places** (Home, Desktop, Documents, Downloads, Pictures, Videos,
Music — only those that exist), **Bookmarks**, **Library** (Trash, Recent Files,
Recent Locations, Network) and **System** (File System, Temporary).

- `Ctrl+B` collapses the sidebar to an icon rail and back. The choice is saved.
- The rail keeps every row usable at 56px wide with centred icons and tooltips.
- Clicking a place while a library view is open returns the tab to that folder.

## Adaptive layout

NIRX stays usable at any window size instead of clipping:

| Window width | Sidebar |
| --- | --- |
| ≥ 880px (with hysteresis) | full list (222px) |
| < 820px | icon rail (56px) |
| < 560px | hidden |

The breadcrumbs and path field shrink instead of pushing the toolbar out of the
window, the preview pane has a small minimum, and the toolbar scrolls sideways
as a last resort. Restoring the full sidebar waits for a little more width than
collapsing it needs, so dragging the window edge does not make it flicker.

## Selection

Single click, `Ctrl+click`, `Shift+click`, `Ctrl+A` (all), `Ctrl+Shift+A`
(none) and `Ctrl+Shift+I` (invert). Dragging from empty space draws a
rubber-band rectangle; holding `Ctrl` adds the band to the existing selection.
The selection survives refreshes that follow file operations and stays
highlighted while the pointer is elsewhere.

## Search and filtering

- `Ctrl+F` opens the search bar; `Esc` closes it.
- **By name** — fuzzy subsequence matching (`dms` finds `DankMaterialShell`).
- **By extension** — `jp` finds `.jpg`/`.jpeg`, `png` finds `.png`, `none`
  finds extension-less files.
- **Recursive** — the toggle searches subfolders too.
- **Type to filter** — with the file list focused, just typing filters. Backspace
  removes a character, `Esc` clears the filter.

The mode is also in *View → Search Mode*.

## Sorting

*View → Sort by →* Name / Size / Modified / Kind. Choosing the same key again
reverses the order. The sort applies to every tab and is stored.

## File operations

Copy (`Ctrl+C`), cut (`Ctrl+X`), paste (`Ctrl+V`), rename (`F2`), new folder
(`Ctrl+Shift+N`), create link (symbolic), move to trash (`Delete`), delete
permanently (`Shift+Delete`), compress, extract, organise by extension, and a
full context menu on right-click.

- Moves and copies run on worker threads with a progress dialog and Cancel.
- Cross-filesystem moves fall back to copy + delete.
- Conflicts offer per-item or all-items choices (replace, keep both, skip) before
  anything is written.
- Permission and I/O errors are reported per item, never swallowed; the summary
  says how many succeeded, were skipped or failed.
- Deleting asks for confirmation (`confirm_delete` in the config can turn the
  trash prompt off).
- Trashed items are recoverable from the Trash view.

## Drag and drop

Drag selected files onto a folder row or tile to move them. Valid targets
highlight, a folder cannot be dropped into itself or a descendant, and a drop
that would overwrite anything asks first.

## Archives

*Files → Compress Selection…* creates an archive in the current folder; the
format follows the name you type (`.tar.gz`, `.zip`, `.tar.xz`, `.7z`, …).
*Files → Extract Here* extracts a selection of archives. `tar`, `unzip` and `7z`
are detected at runtime; a missing tool is reported instead of failing silently.
Archives are recognised by extension (`.zip`, `.tar`, `.tar.gz`, `.tgz`,
`.tar.xz`, `.tar.bz2`, `.tar.zst`, `.7z`, `.rar`, `.gz`, `.xz`, `.bz2`, `.zst`,
`.iso`).

## Checksums and properties

- **SHA-256 checksums** for the selection, computed off the main thread in a
  monospaced, copyable window.
- **Properties** (`Alt+Return`): name, path, MIME type, size, mode, timestamps,
  symlink target and the recursive size of folders.

## Preview pane

`Ctrl+Shift+P` toggles a right-hand pane that shows:

- images (scaled to the pane),
- the first page of PDFs,
- a video frame,
- a text/code snippet for text files,
- and metadata for everything else.

Generation happens on worker threads with an on-disk cache in `~/.cache/nirx`
and a size limit (`preview_max_bytes`, 8 MiB by default).

## Preview icons (thumbnails)

Images, videos and PDFs can show real thumbnails in both list and grid views.
*Settings → Preview Icons…* turns the three categories on or off independently.
Thumbnails are letterboxed onto a fixed square canvas so tiles never jump
around, cached on disk, and fall back to the themed file-type icon when a tool
is missing or a file cannot be decoded.

## Folder colours

- Every folder uses a default colour (Colloid blue unless changed) for its icon.
- Any folder can override it from its context menu (*Folder Colour…*).
- *Tools → Subfolders Inherit Colour* makes a folder without its own colour use
  the nearest coloured ancestor instead of the default.
- *Tools → Folders With This Colour…* (`Ctrl+Shift+K`) scans the repository root
  and lists every folder that resolves to the current colour; `Alt+PageDown` /
  `Alt+PageUp` jump straight to the next / previous match.

Colours are stored in NIRX's config and painted onto the icon theme's folder
icon in memory — neither the theme nor any `.desktop` file is written.

## Library views: Trash, recents, network

These are **states of the current tab**, not separate windows or extra tabs:

- **Trash** — everything in the freedesktop trash, with the original path and
  deletion date.
- **Recent Files** — files opened from NIRX, newest first.
- **Recent Locations** — folders visited recently.
- **Network** — mounted network shares plus Windows shares, network servers,
  afc (iPhone/iPad) and MTP addresses.

Each view fills the tab like a folder does: the breadcrumb names the view, the
status bar shows `12 items · 2 selected`, and the tab keeps its own title and
icon. Browse anywhere (a Place, the path field, a breadcrumb, history) and the
tab returns to the folder it was showing.

Actions live in a bar along the bottom edge, so they stay reachable for any
number of selected rows and in a narrow window:

| View | Actions |
| --- | --- |
| Trash | Refresh, Restore, Delete Permanently |
| Recent Files | Refresh, Open, Show Folder, Copy Path |
| Recent Locations | Refresh, Open, Copy Path |
| Network | Refresh, Open |

Rows are multi-select (`Ctrl+click`, `Ctrl+A`), the buttons grey out with
nothing selected and act on the whole selection, and permanent delete confirms
once for the batch. `Esc` goes back to the folder, `Enter` runs the main action,
`Delete` the destructive one.

## Quick Shift

`Ctrl+Shift+M` opens a keyboard-driven "move these items to…" picker: type to
search recents, bookmarks, XDG folders or any path; `↑`/`↓` choose; Enter moves.
Conflicts are resolved with one decision for the whole batch, and the move runs
on a worker thread with progress and Cancel.

## Bulk rename

`Ctrl+Shift+R` renames many files at once with find/replace, numbering, case
changes and extension handling, previewed before anything is renamed.

## Run commands

`fm_run_task.json` in (or above) the current folder defines named commands:

```json
{
  "commands": [
    { "name": "Build", "command": "cargo build" },
    { "name": "Test",  "command": "cargo test" }
  ]
}
```

The file is found by walking up from the current folder, so one file at a
project root serves every subfolder. NIRX loads it automatically, adds a ▶
button to the toolbar, runs the chosen command in a terminal in that folder and
shows the exit status. Commands are edited from NIRX and saved back atomically
after validation. A command that looks destructive (`rm -rf`, `sudo`, …) asks
for confirmation first.

## Git integration

- **Status badges** on rows in detailed view and in the tile tooltip.
- **Git Repository…** opens a dashboard window: branch, upstream, ahead/behind,
  staged/modified/untracked/conflicted counts, a live commit graph with
  connectors, staging actions and diffs. It refreshes while it is open.

Git is optional; without it the badges simply do not appear.

## Opening files, terminal and Zed

Files open through GIO, respecting system MIME associations, `.desktop` entries
and `XDG_CURRENT_DESKTOP` overrides.

- **Open With…** (`Ctrl+Shift+O`) lists the apps registered for the type and can
  set the default; "always" stores a NIRX-local override instead of changing
  system configuration.
- **Default Applications & Commands…** manages those overrides and custom
  opener commands (keys may be a path, an extension or a MIME type; `%f`/`{}`
  expand to the selected files).
- **Open Terminal Here** (`Ctrl+Alt+T`) uses the first terminal NIRX finds
  (kitty, foot, alacritty, wezterm, ghostty, konsole, ptyxis) in the current
  folder; the list is configurable.
- **Open Folder in Zed** (configurable editor) is one menu entry away.

## LocalSend transfers

NIRX speaks the LocalSend protocol natively: no LocalSend window opens, and
nothing is shelled out.

- Select files or folders and press `Ctrl+Shift+S` (or the toolbar send button).
- The picker lists devices found on the network — type to filter, `↑`/`↓` to
  choose, Enter to send, `Ctrl+R` to rescan, or add a device by address when
  multicast is blocked.
- Uploads show per-file progress with Cancel; the sender gets a toast with the
  number of files delivered.
- Incoming transfers ask **Accept / Decline** with the sender, device and file
  list. Accepted files keep the sender's folder structure, are written to the
  receive folder, and never overwrite (a name that exists gets `(2)`).
- *Settings → Behaviour → LocalSend Transfers…* (or *Share → LocalSend
  Transfers…*) sets the device name, port, receive folder, auto-accept and the
  list of trusted devices.

Security, ports and troubleshooting: [localsend.md](localsend.md).

## The menus

The `…` button and the gear button open **dropdowns**: each section in the
hamburger menu is a heading that expands on hover, on Enter or with `→`, so the
popover stays a short list of ten entries instead of one long scroll.

Every entry in both menus is a GTK action, which means it can also be triggered
from the keyboard (`F10`, then arrows and Enter) and rebound in *Keyboard
Shortcuts*.

## Persistence

One JSON file keeps everything: window geometry and maximised state, tabs and
active tab, bookmarks, view mode, sort order, hidden-files state, preview
visibility, sidebar shape, Quick Shift history, recent files and locations,
folder colours and inheritance, custom openers and app overrides, run-command
history, thumbnail toggles, icon theme preference, shortcuts and the LocalSend
settings ([configuration.md](configuration.md)). Writes are atomic
(temp file + rename), so an interrupted save can never leave you without a
working configuration.
