# Configuration

NIRX stores settings in one JSON document. Writes are atomic (a temporary file
is renamed into place), so an interrupted save never leaves a half-written
configuration. If a config is invalid, NIRX preserves it as `config.json.broken`,
prints the parse error and starts from defaults. Existing Nook configuration is
read as a compatibility fallback only when no NIRX config exists; the legacy
file is not moved or deleted, and subsequent saves go to the NIRX path.

## Locations

| Path | Contents |
| --- | --- |
| `$XDG_CONFIG_HOME/nirx/config.json` (default `~/.config/nirx/config.json`) | Everything in this document |
| `$XDG_CACHE_HOME/nirx` (default `~/.cache/nirx`) | Preview and thumbnail cache |
| Legacy `$XDG_CONFIG_HOME/nook/config.json` (default `~/.config/nook/config.json`) | Read-only config fallback until an NIRX config is created |
| `<folder>/fm_run_task.json` | Per-folder Run commands ([run commands](#run-commands)) |
| `~/.local/share/Trash` | The freedesktop trash NIRX reads and writes |

`$XDG_CONFIG_HOME` and `$XDG_CACHE_HOME` are honoured when they are absolute
paths; otherwise the `~/.config` and `~/.cache` defaults are used. `HOME`
decides what `~` means, both for NIRX and for paths stored in the config.

## The file

```json
{
  "version": 1,
  "window": { "width": 1180, "height": 760, "maximized": false },
  "tabs": [{ "path": "/home/me", "view": "list" }],
  "active_tab": 0,
  "bookmarks": [{ "name": "Work", "path": "/home/me/Work" }],
  "show_hidden": false,
  "view": "list",
  "show_preview": false,
  "show_sidebar": true,
  "sort": "name",
  "sort_desc": false,
  "terminal": ["kitty"],
  "editor": "zed",
  "recent_destinations": [],
  "app_overrides": {},
  "shortcuts": {},
  "confirm_delete": true,
  "follow_theme_mode": true,
  "preview_max_bytes": 8388608,
  "icon_theme": null,
  "folder_color": "#7ba9e8",
  "folder_colors": {},
  "folder_color_inherit": true,
  "folder_commands": {},
  "custom_openers": {},
  "sidebar_mini": false,
  "thumbs_images": true,
  "thumbs_videos": true,
  "thumbs_pdfs": true,
  "recent_files": [],
  "transfer_alias": null,
  "transfer_port": 53317,
  "transfer_auto_accept": false,
  "transfer_receive_dir": "~/Downloads",
  "transfer_trusted": [],
  "transfer_enabled": true
}
```

Every key is optional: a missing key takes its default, which is why a config
written by an older version keeps working.

## Keys

### Window and session

| Key | Type / default | Meaning |
| --- | --- | --- |
| `version` | `1` | Schema version, for future migrations |
| `window.width`, `height` | `1180`, `760` | Restored geometry (kept when the window is at least 200×200) |
| `window.maximized` | `false` | Restore maximised |
| `tabs` | `[{path, view}]` | The open tabs, in order |
| `active_tab` | `0` | Index of the tab to activate |
| `show_sidebar` | `true` | Reserved; the sidebar is always available |
| `sidebar_mini` | `false` | Sidebar collapsed to an icon rail |

Tabs that are showing a library view (Trash, recents, network) are saved as the
folder they hold: library views are transient states, not part of the session.

### Browsing

| Key | Type / default | Meaning |
| --- | --- | --- |
| `view` | `"list"` | `"grid"`, `"list"` or `"recursive"` |
| `sort` | `"name"` | `"name"`, `"size"`, `"modified"`, `"kind"` |
| `sort_desc` | `false` | Descending order |
| `show_hidden` | `false` | Show dotfiles |
| `show_preview` | `false` | Preview pane open |
| `confirm_delete` | `true` | Ask before moving to the trash |
| `recent_destinations` | `[]` | Quick Shift history, most recent first (max 10) |
| `recent_files` | `[]` | Files opened from NIRX (max 25) |
| `bookmarks` | `[]` | `{ "name": …, "path": … }` entries in the sidebar |

### Appearance

| Key | Type / default | Meaning |
| --- | --- | --- |
| `follow_theme_mode` | `true` | Take the dark/light mode from the DMS palette |
| `icon_theme` | `null` | Icon theme used by NIRX only; `null` = auto (Colloid if installed) |
| `folder_color` | Colloid blue | Colour for folder icons without an override |
| `folder_colors` | `{}` | Per-folder colours, keyed by absolute path |
| `folder_color_inherit` | `true` | Uncoloured folders inherit the nearest coloured ancestor |
| `thumbs_images` | `true` | Image thumbnails as icons |
| `thumbs_videos` | `true` | Video thumbnails as icons |
| `thumbs_pdfs` | `true` | PDF first-page thumbnails as icons |
| `preview_max_bytes` | `8388608` | Files larger than this are never previewed |

### Applications

| Key | Type / default | Meaning |
| --- | --- | --- |
| `terminal` | `["kitty"]` | Candidate terminals, tried in order |
| `editor` | `"zed"` | Editor opened by *Open Folder in Zed* |
| `app_overrides` | `{}` | `"<mime>"` or `".<extension>"` → desktop file id; NIRX-local default apps |
| `custom_openers` | `{}` | `"<path>"`, `".<ext>"` or `"<mime>"` → shell command (`%f`/`{}` expand to the files) |
| `shortcuts` | `{}` | Action name → accelerator override (see below) |

### Run commands

| Key | Type / default | Meaning |
| --- | --- | --- |
| `folder_commands` | `{}` | Legacy: per-folder command lists keyed by absolute path. New files are read from `fm_run_task.json` |

### LocalSend

| Key | Type / default | Meaning |
| --- | --- | --- |
| `transfer_enabled` | `true` | Participate in discovery and receiving |
| `transfer_alias` | `null` | Name other devices see; `null` = hostname |
| `transfer_port` | `53317` | Preferred port; a free one is used when taken |
| `transfer_receive_dir` | `"~/Downloads"` | Where incoming files are written |
| `transfer_auto_accept` | `false` | Accept without asking from trusted devices |
| `transfer_trusted` | `[]` | Certificate fingerprints allowed to skip the prompt |

See [localsend.md](localsend.md).

## Shortcuts

`shortcuts` maps an action name to a GTK accelerator. Only the actions listed in
*Keyboard Shortcuts* can be used; an empty string removes a binding.

```json
{
  "shortcuts": {
    "win.new-tab": "<Control>t",
    "win.send": "<Super>s",
    "win.context-menu": "<Shift>F10"
  }
}
```

Accelerators use the GTK syntax: modifiers (`<Control>`, `<Shift>`, `<Alt>`,
`<Super>`) followed by a key name (`t`, `F10`, `Page_Down`, `comma`, `Return`).
The shortcut editor writes this section for you.

## Run commands

`fm_run_task.json` is looked up from the current folder upwards, so one file at
a project root serves the whole tree:

```json
{
  "version": 1,
  "commands": [
    { "name": "Dev server", "command": "npm run dev" },
    { "name": "Test", "command": "npm test" }
  ]
}
```

Rules:

- Empty commands are dropped on save; an empty name falls back to the command
  text.
- The document is validated, and re-parsed, before it is written; an existing
  file that does not parse is reported and never overwritten.
- Commands run in a terminal in the folder being browsed, with their output and
  exit status.
- Commands that look destructive (`rm`, `sudo`, `mkfs`, …) ask for confirmation
  before they start.

## Environment

| Variable | Effect |
| --- | --- |
| `HOME` | Home directory, `~` expansion, default config/cache locations |
| `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME` | Standard XDG redirection (the trash lives under `XDG_DATA_HOME/Trash`) |
| `XDG_CURRENT_DESKTOP` | Honoured by GIO when resolving default applications |
| `WAYLAND_DISPLAY`, `DISPLAY` | GTK picks the session automatically |

NIRX has no other environment variables of its own.

## Resetting

- Delete `config.json` to start from defaults (tabs, bookmarks, colours and
  shortcuts are lost).
- Delete `~/.cache/nirx` to rebuild every cached preview and thumbnail.
- The old Nook configuration is not deleted by NIRX; remove it manually only if you no longer need to roll back.
- Remove `fm_run_task.json` files to drop Run commands for those trees.
