# Keyboard

Nook is designed to be used without a mouse. This page lists every default
shortcut and then walks through complete keyboard-only workflows.

## Contents

- [Global shortcuts](#global-shortcuts)
- [Menus](#menus)
- [File list](#file-list)
- [Sidebar](#sidebar)
- [Library views (Trash and friends)](#library-views-trash-and-friends)
- [LocalSend](#localsend)
- [Dialogs](#dialogs)
- [Workflows](#workflows)
- [Rebinding](#rebinding)

## Global shortcuts

### Tabs and windows

| Keys | Action |
| --- | --- |
| `Ctrl+T` | New tab |
| `Ctrl+W` | Close tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+Q` | Quit |

### Menus

| Keys | Action |
| --- | --- |
| `F10` | Open the main (`…`) menu |
| `Ctrl+,` | Open the settings (gear) menu |
| `↑` `↓` | Move between entries |
| `→` / `Enter` | Open the focused dropdown |
| `←` / `Esc` | Leave the dropdown / close the menu |
| type the first letters | Jump to a matching entry |
| `Shift+F10` or the `Menu` key | Context menu for the selection |

### Navigation

| Keys | Action |
| --- | --- |
| `Alt+Left` / `Alt+Right` | Back / forward |
| `Alt+Up` | Parent folder |
| `Alt+Home` | Home folder |
| `Ctrl+L` | Edit the location (Enter to go, `Esc` to cancel) |
| `Ctrl+R` | Refresh (also re-reads `fm_run_task.json`) |
| `Ctrl+Shift+B` | Move focus into the sidebar |
| `Ctrl+Shift+F` | Move focus into the file list |
| `Ctrl+B` | Collapse / expand the sidebar |
| `Ctrl+H` | Show / hide hidden files |
| `Ctrl+1` | Cycle icons → details → recursive |
| `Ctrl+Shift+P` | Toggle the preview pane |

### Files

| Keys | Action |
| --- | --- |
| `Enter` | Open the selection, or enter the folder |
| `F2` | Rename |
| `Ctrl+Shift+R` | Bulk rename |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste |
| `Delete` | Move to trash |
| `Shift+Delete` | Delete permanently (asks first) |
| `Ctrl+Shift+N` | New folder |
| `Ctrl+Shift+M` | Quick Shift (move to…) |
| `Ctrl+Shift+C` | Copy the selected paths |
| `Ctrl+Shift+O` | Open with… |
| `Alt+Return` | Properties |
| `Ctrl+D` | Bookmark this folder |
| `Ctrl+Shift+S` | Send the selection with LocalSend |
| `Ctrl+A` | Select all |
| `Ctrl+Shift+A` | Deselect all |
| `Ctrl+Shift+I` | Invert selection |

### Search and view

| Keys | Action |
| --- | --- |
| `Ctrl+F` | Focus search |
| `Ctrl+Shift+K` | Folders with this colour… |
| `Alt+PageDown` / `Alt+PageUp` | Next / previous same-colour folder |
| `F1` | Keyboard shortcut editor |

Actions without a default binding include: duplicate tab, open in Zed, icon
theme, preview icons, folder colour, run commands, configure folder commands,
Git dashboard, default applications, create link, organise by extension, icon
and detailed view, search mode by name/extension, sorting, and the four Library
views. All of them are in the menus, and all of them can be assigned a key in
the shortcut editor.

## Menus

The `…` button opens a compact list of dropdowns — Tabs, Files, Edit, Selection,
View, Go, Places, Share, Tools, Help — and each one expands instead of showing
everything at once. The gear button opens Appearance, Behaviour, Repository and
About.

Because every entry is an action, `F10` → `↓` → `→` → `↓` → `Enter` reaches
anything in the application, including every file operation, all settings
dialogs and the LocalSend features.

## File list

| Keys | Action |
| --- | --- |
| `↑` / `↓` | Move the cursor |
| `Home` / `End` | First / last item |
| `PageUp` / `PageDown` | Scroll a page |
| `Shift` + arrows | Extend the selection |
| `Ctrl+Space` | Add / remove the cursor row from the selection |
| `Ctrl+A` | Select all |
| typing | Type-to-filter the current folder |
| `Backspace` | Delete the last typed character |
| `Esc` | Clear the filter |
| `Enter` | Open the selected item |
| `Shift+F10` / `Menu` | Context menu for the selection (or the folder) |

## Sidebar

`Ctrl+Shift+B` puts the keyboard in the sidebar.

| Keys | Action |
| --- | --- |
| `↑` / `↓` | Previous / next place |
| `Home` / `End` | First / last place |
| `Enter` | Open the place (or the Library view) |
| `Esc` | Return to the file list |
| `Tab` | Continue to the next focusable control |

## Library views (Trash and friends)

Trash, Recent Files, Recent Locations and Network open inside the current tab.
The keyboard lands on the list.

| Keys | Action |
| --- | --- |
| `↑` / `↓` | Move between rows |
| `Space` | Select / deselect the focused row |
| `Ctrl+A` | Select every row |
| `Enter` | Main action: Restore (Trash) or Open (the others) |
| `Delete` | Destructive action: Delete Permanently (Trash) |
| `Esc` | Back to the folder the tab was showing |
| `Tab` | Reach the action bar and its buttons |

## LocalSend

**Sending** (`Ctrl+Shift+S`):

| Keys | Action |
| --- | --- |
| type | Filter the device list |
| `↓` | Move from the filter into the device list |
| `↑` / `↓` | Choose a device |
| `Enter` | Send to the chosen device |
| `Ctrl+R` | Rescan the network |
| `Esc` | Cancel |

**Incoming transfers** ask in their own window: `Enter` accepts (the default),
`Esc` or *Decline* refuses.

## Dialogs

Every dialog follows the desktop conventions: `Tab`/`Shift+Tab` move,
`Enter` activates the default button, `Esc` cancels or closes, and the list-based
dialogs (Open With, Folder Colours, icon theme, shortcuts, Quick Shift) accept
arrow keys and type-to-search.

## Workflows

### Open a folder and work in it, keyboard only

1. `Ctrl+L`, type the path, `Enter`.
2. `↓` or `Ctrl+Shift+F` to focus the list.
3. `↓` to the file, `Ctrl+Space` to pick several, `Enter` to open.
4. `F2` to rename, `Delete` to trash, `Shift+Delete` to delete for good.

### Operate on the current folder

`Shift+F10` opens the context menu for the folder itself when nothing is
selected (new folder, paste, properties, terminal, colours, run commands …).

### Reach a Library view

`F10` → `Places` → `Trash` (or `↓` through Places). While there: `Space` to
select rows, `Enter` to restore, `Delete` to delete permanently, `Esc` to
return.

### Send a file without touching the mouse

Select it (`↓`, `Ctrl+Space`), `Ctrl+Shift+S`, type part of the device name,
`↓`, `Enter`. Progress and the result appear as a dialog and a toast; Cancel in
the progress dialog stops the transfer.

### Bookmark and jump back

`Ctrl+D` bookmarks the current folder. Later, `Ctrl+Shift+B`, `↓` to the
bookmark under Bookmarks, `Enter`.

## Rebinding

`F1` (or *Settings → Appearance → Keyboard Shortcuts…*) lists every action with
its current accelerator. Clearing a field removes the binding; assigning a new
one applies immediately and is stored in `config.json` under `shortcuts`
([configuration.md](configuration.md#shortcuts)).
