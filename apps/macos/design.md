# Pix macOS design contract — light

Pix is a quiet menu bar host control, not a desktop IDE. The status item makes
host availability obvious, while the menu keeps key counts and actions visible
and progressively discloses workspace/device inventories in submenus. The
pairing guide remains a focused window. Menu inventory snapshots prevent live
Host events from interrupting pointer hover.

- Use native `MenuBarExtra`, `Settings`, `Form`, `List`, and `NSOpenPanel`.
- Keep the menu's top level concise: status, Add Device, pairing attention,
  Workspaces, Devices, sessions, Settings, and Quit. Workspaces and Devices
  use nested `Menu` disclosure for their inventories and destructive actions.
- Pairing approval stays in the focused Add Device window and opens
  automatically when a request arrives.
- Settings owns Pi path, launch at login, diagnostics, privacy, workspaces,
  and paired-device management.
- Pairing confirmation is monospaced, prominent, and never color-only.
- Workspace rows show display name and host-local path; iOS receives only the
  approved display data through the Rust protocol.
- Empty, setup, unavailable, and approval states explain the next action
  without repeating a full tutorial on every pane.
- Paired-device rows show the device name plus a selectable identity; revoke
  is an explicit destructive action.
- Active-session rows name the authorized workspace and offer Release only.
- No chat, terminal, file browser, Git UI, or conversation cache belongs here.
