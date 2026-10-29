# Troubleshooting

## The window looks wrong or the theme does not follow the wallpaper

- NIRX reads the palette Matugen / DankMaterialShell generate
  (`~/.cache/DankMaterialShell/` and the DMS GTK colour file). If neither exists
  it falls back to its built-in dark palette; that fallback is not a bug.
- The palette is watched, so a wallpaper change should re-theme NIRX while it
  runs. If it does not, check that the palette files are actually being
  rewritten by DMS.
- Only NIRX is themed. The rest of the desktop is untouched by design.

## An icon is missing (a blank square)

NIRX uses the icon theme's symbolic icons. If a theme lacks one (for example
`user-desktop-symbolic`), that row shows a blank or fallback icon. Switching
*Settings → Appearance → Icon Theme…* to Automatic (Colloid when installed) or
to Adwaita fixes it.

## The window cannot be made smaller / the sidebar vanished

The sidebar adapts on purpose: full list at ≥ 880px, an icon rail below 820px,
hidden below 560px. `Ctrl+B` toggles between the full list and the rail — it
cannot bring back a sidebar that is hidden for lack of room, because there is no
room. The file view itself always stays usable, and the toolbar scrolls rather
than clipping.

## A keyboard shortcut does nothing

- It may have been rebound or cleared: `F1` shows every action and its current
  accelerator.
- Typing in the file list filters it, so a single letter is consumed as a
  filter. `Esc` clears the filter and returns the list to normal.
- Some actions have no default binding at all (see
  [keyboard.md](keyboard.md#global-shortcuts)); reach them from `F10` or assign
  a key.

## "Open terminal here" does nothing

NIRX tries the terminals in `terminal` in the config, in order: `kitty`, `foot`,
`alacritty`, `wezterm`, `ghostty`, `konsole`, `ptyxis` by default. Install one of
them, or put your terminal's command first in `config.json`.

## A file does not open with the application I expect

System defaults come from GIO. *Settings → Behaviour → Default Applications &
Commands…* sets a NIRX-local override (`app_overrides` in the config) without
touching system configuration. *Open With…* → "always" does the same for one
file type.

## Previews or thumbnails are missing

- Images need `gdk-pixbuf`, videos need `ffmpeg`/`ffprobe`, PDFs need
  `pdftoppm` (poppler-utils).
- Categories can be switched off in *Settings → Appearance → Preview Icons…*.
- Files larger than `preview_max_bytes` (8 MiB) are not previewed.
- The cache may be stale: delete `~/.cache/nirx`.

## "Cannot open …: Permission denied"

NIRX reports the failing path and the OS error per item. It never silently skips
files; the summary line says how many succeeded and how many failed.

## Deleting went to the trash instead of removing the file

`Delete` moves to the trash; `Shift+Delete` deletes permanently (with a
confirmation). Items moved to the trash are visible and restorable in the Trash
view. On volumes without a freedesktop trash, permanent deletion is the only
option.

## Trash is empty although files were deleted

NIRX reads the freedesktop trash under `$XDG_DATA_HOME/Trash` (usually
`~/.local/share/Trash`). Files deleted on other mounts have their own trash
directory, and files deleted from another desktop may live in a different
`XDG_DATA_HOME` if that variable is set differently.

## A tab is stuck in Trash / Network / Recent Files

Those views are a state of the tab. `Esc` (with the list focused), clicking any
Place, the breadcrumb, or `Ctrl+L` and entering a folder all return the tab to
its folder. Closing the tab and opening a new one also works.

## The `…` menu looks different

It is a list of dropdowns (Tabs, Files, Edit, Selection, View, Go, Places,
Share, Tools, Help) rather than one long list, so it stays compact. `F10` opens
it, `→`/Enter opens a dropdown, `Esc` backs out.

## LocalSend

See [localsend.md](localsend.md#troubleshooting): an empty device list,
firewalls, multicast, PINs, busy sessions and certificate changes are all
covered there. Two common ones:

- **The device list is empty.** Open LocalSend on the other device (it only
  announces while running), allow UDP 53317 through the firewall, and if the
  network blocks multicast use *Add by address*.
- **Nothing arrives on this computer.** `Receive transfers` must be on, the
  announced TCP port must be reachable, and with auto-accept off the incoming
  prompt must be answered within 90 seconds.

## The configuration is broken

NIRX renames an unparsable `config.json` to `config.json.broken`, prints the
parse error and starts from defaults, so it always launches. Delete the file to
reset everything, or fix it by hand using
[configuration.md](configuration.md).

## Reporting a bug

Include:

- what you did, what you expected, what happened,
- whether it reproduces after deleting `~/.cache/nirx`,
- the output of running from a terminal (`cargo run` or `nirx`) — NIRX prints
  startup errors such as a failure to serve the LocalSend port there,
- your GTK version (`gtk4-launch --version` or the `libgtk-4-1` package
  version).
