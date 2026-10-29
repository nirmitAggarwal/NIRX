# LocalSend

Nook implements the LocalSend protocol itself. There is no LocalSend window, no
LocalSend installation on the sending machine, no daemon and no shell command:
the protocol runs inside Nook, on Nook's own threads, with Nook's own dialogs.

```text
        Nook (sender)                          phone / PC (LocalSend or Nook)
   ┌──────────────────────┐                ┌──────────────────────────────┐
   │ 1 announce on        │  UDP 53317     │                              │
   │   224.0.0.167:53317  ├───────────────►│  "who is there?"             │
   │                      │                │                              │
   │ 2 POST /register     │  HTTPS 53317   │  answers with alias, device  │
   │   (to every peer)    ├───────────────►│  type, port, fingerprint      │
   │                      │◄───────────────┤                              │
   │ 3 picker lists them  │                │                              │
   │ 4 POST /prepare-upload│  HTTPS        │  user accepts there          │
   │   + file metadata     ├──────────────►│                              │
   │                      │◄───────────────┤  sessionId + per-file tokens │
   │ 5 POST /upload        │  file bytes    │                              │
   │    ?sessionId&fileId&token ├─────────►│  writes to its folder        │
   └──────────────────────┘                └──────────────────────────────┘
```

## What the integration covers

| Feature | Status |
| --- | --- |
| Discover devices (multicast + HTTP register) | yes |
| Discover devices on networks without multicast | yes, "Add by address" |
| Send files | yes, with per-file progress and Cancel |
| Send folders | yes, folder structure is preserved |
| Send to devices set to plain HTTP | yes (the announced protocol decides) |
| Receive files | yes, with an Accept/Decline prompt |
| Auto-accept from trusted devices | yes |
| PIN-protected receivers | yes, the picker has a PIN field |
| SHA-256 verification | yes, sent when the file is under 512 MiB and always verified when the sender provides one |
| Cancel a session | yes, `/cancel` is sent when you cancel |
| Text / clipboard messages | no (LocalSend sends those as files; Nook does not implement the text message UI) |
| Reverse transfer (browser download) | no |

## Sending

1. Select files and/or folders in a tab.
2. `Ctrl+Shift+S`, the toolbar send button, or *Share → Send to Device…*.
3. The picker opens and announces Nook on the network; devices that answer (or
   that are already known) appear as you type or after a moment.
   - Type to filter, `↑`/`↓` to move, `Enter` to send, `Ctrl+R` to rescan.
   - **Add by address** reaches a device that multicast cannot: `192.168.1.20`
     or `host:port`. Both HTTPS and plain HTTP are probed.
   - **PIN** is only needed when the other device asks for one.
4. The transfer runs on a worker thread behind a progress dialog showing the
   current file, which file of how many, the percentage and the overall bar.
   Cancel stops the current file and tells the peer with `/cancel`.

Files are read in 256 KiB blocks, so a 10 GB file never lands in memory.
Folders are walked recursively and sent with their relative paths
(`album/2026/photo.png`), which is how LocalSend preserves folder structure.

## Receiving

Any device that finds Nook can send to it. With `transfer_enabled` on (the
default) Nook answers `/register`, `/prepare-upload`, `/upload` and `/cancel`
itself.

- Without auto-accept, a window shows the sender, device type, address,
  protocol, file list and total size, with **Accept** (default, `Enter`) and
  **Decline** (`Esc`). No answer for 90 seconds declines automatically, so a
  forgotten prompt never pins a connection thread.
- With **auto-accept** on, transfers from *trusted* devices start immediately.
  A device becomes trusted when you accept one of its transfers while
  auto-accept is on; the list is in the settings dialog and each entry can be
  forgotten.
- Accepted files are written into the receive folder (default `~/Downloads`),
  keeping the sender's subfolders. Nothing is ever overwritten: an existing name
  gets `(2)`, `(3)`, … .
- When every file of a session has arrived, Nook refreshes the panes and shows
  how many items were received.

Incoming names are sanitised: absolute paths, `..` components and separators
that would escape the receive folder are stripped, so a hostile sender cannot
write outside it. Unit tests cover this (`incoming_names_stay_inside_the_receive_folder`).

## Settings

*Settings (gear) → Behaviour → LocalSend Transfers…*, *Share → LocalSend
Transfers…* or `F10` → Share, and also from the menu bar:

| Setting | Meaning |
| --- | --- |
| Receive transfers | Whether Nook participates in discovery and receiving at all |
| Device name | The alias other devices see (defaults to the hostname) |
| Port | 53317 by default; changing it restarts the service |
| Save incoming files to | Receive folder (`~/Downloads` by default) |
| Auto-accept from trusted devices | Skip the prompt for devices you have accepted before |
| Trusted devices | The list, with **Forget** per entry |

The dialog also shows this device's alias, port and certificate fingerprint.

The port is announced per device, so if something else already holds 53317 (the
LocalSend desktop app, for example), Nook picks a free port and announces that
one instead — LocalSend still finds it.

## Security

- **Transport**: HTTPS with a self-signed certificate generated when Nook
  starts. LocalSend has no certificate authority on a home network, so Nook
  accepts self-signed certificates and identifies peers by fingerprint instead:
  every device announces the SHA-256 of its certificate, and the fingerprint is
  compared with the certificate actually presented. A mismatch is reported as a
  toast rather than silently accepted. Fingerprints are uppercase hex, the
  format LocalSend uses on the wire.
- **Mutual TLS**: the same certificate is presented as the *client* certificate
  on every outgoing connection. LocalSend receivers (1.18 and newer) demand a
  client certificate and abort the handshake with `certificate_required` — the
  `certificateRequired` error — when a sender shows none, so Nook always
  presents its identity before the first HTTP byte is written. The certificate
  announced, the certificate served and the certificate presented are the same
  key pair, which is what makes the fingerprint comparison above meaningful.
- **Scope**: the receiver only writes into the receive folder, only after a
  token from `/prepare-upload`, and only for a session it created. Tokens are
  random per file, sessions are random per transfer.
- **Confirmations**: without auto-accept, nothing is written until you press
  Accept. The sender is shown with its alias, type and address.
- **Nothing runs in the background**: the sockets are opened by the window and
  closed when Nook exits.

## Ports and firewall

| Port | Protocol | Direction | Needed for |
| --- | --- | --- | --- |
| 53317 | UDP (multicast 224.0.0.167) | out | Sending and receiving announcements |
| 53317 | UDP (unicast) | in | Direct replies to your announcement |
| 53317 | TCP | in | Receiving files, and answering discovery |
| announced port | TCP | out | Sending files |

If nobody shows up in the picker: allow TCP **and** UDP 53317 in the firewall,
keep both devices on the same subnet (guest networks and AP isolation block
multicast), and use *Add by address* when multicast is blocked. Devices only
announce while their LocalSend app (or Nook) is running.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| The picker stays empty | LocalSend is not open on the other device, multicast is blocked, or the firewall drops UDP 53317 — try Add by address |
| "This device asks for a PIN" | The other device requires a PIN; put it in the PIN field |
| "… declined the transfer" | Rejected on the receiving side |
| "… is busy with another transfer" | Finish or cancel the other session |
| "The device reported a checksum mismatch" | The file changed while it was being read |
| "presented a different certificate" | The other device reinstalled or regenerated its certificate; confirm it is really that device |
| Nothing arrives on this device | `Receive transfers` is off, or the sender cannot reach the announced port through the firewall |

## Where it lives in the code

- `src/localsend.rs` — the protocol: discovery, TLS, HTTP client and server,
  sessions, tokens, sanitising, progress. Tests including a real loopback
  transfer are at the bottom of the file.
- `src/ui/send.rs` — the picker, the progress dialog, the incoming prompt and
  the settings dialog.
- `src/ui/mod.rs` — starts the service, drains its events on the GTK main loop,
  and owns the toolbar button, actions and shortcuts.
- `src/settings.rs` — the `transfer_*` configuration keys.

Details of the threading model are in [architecture.md](architecture.md#threads).
