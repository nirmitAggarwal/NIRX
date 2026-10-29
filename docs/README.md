# NIRX documentation

NIRX is a GTK4 file manager for Linux (Hyprland / DankMaterialShell especially),
written in Rust. These documents describe what it does, how to drive it without
a mouse, how the code is laid out, and how the LocalSend integration works.

| Document | What is in it |
| --- | --- |
| [features.md](features.md) | Every feature, one section each: what it does, how to use it |
| [keyboard.md](keyboard.md) | All shortcuts and complete keyboard-only workflows |
| [localsend.md](localsend.md) | The LocalSend integration: protocol, security, ports, troubleshooting |
| [architecture.md](architecture.md) | Source map: every module, key types, data flow, threading |
| [configuration.md](configuration.md) | `config.json` keys, `fm_run_task.json`, caches, environment |
| [troubleshooting.md](troubleshooting.md) | Common problems and what to do about them |

## Orientation

```
┌──────────────────────────────────────────────────────────────┐
│ tab strip                                       _ □ ×        │  ← tabs, one per folder
├──────────────────────────────────────────────────────────────┤
│ ← → ↑  breadcrumbs  ✎  search  view hidden preview send …    │  ← toolbar
├────────────┬─────────────────────────────────────────────────┤
│ PLACES     │                                                 │
│  Home      │            folder contents                      │ ← file view
│  Desktop   │      (icons, details, or recursive)             │
│  …         │                                                 │
│ LIBRARY    │                                                 │
│  Trash     │                                                 │
│  Network   │                                                 │
├────────────┴─────────────────────────────────────────────────┤
│ 12 items · 2 selected                            ~/Documents  │  ← status bar
└──────────────────────────────────────────────────────────────┘
```

- The **sidebar** shows Places, Bookmarks, Library views and System locations.
  It becomes an icon rail on narrow windows and hides entirely when there is no
  room (see [features.md](features.md#adaptive-layout)).
- The **toolbar** holds navigation, the path, search and the folder actions.
  The `…` menu is a short list of dropdown headings: Tabs, Files, Edit,
  Selection, View, Go, Places, Share, Tools, Help.
- **Tabs** are folders. Trash, Recent Files, Recent Locations and Network are
  *states* of the current tab, not extra tabs.
- The **status bar** shows counts/selection on the left and the location on the
  right.

## Quick start

```sh
cargo run --release            # opens your home directory
cargo run --release -- ~/Work  # opens a specific folder
```

Everything is persisted in `~/.config/nirx/config.json`; nothing outside that
file, `~/.cache/nirx` and the folders you touch is modified.

## Where to look in the source

- `src/main.rs` — application start-up, command-line directory, GTK actions.
- `src/ui/mod.rs` — window, toolbar, sidebar, tabs, actions, shortcuts.
- `src/ui/pane.rs` — one directory view (model, filters, list/grid/recursive).
- [architecture.md](architecture.md) has the full map.
