# Pix macOS design contract — light

Pix is a quiet menu bar host control, not a desktop IDE. The menu should make
three facts obvious at a glance: whether the host is available, which folders
are authorized, and whether a phone is waiting for explicit approval.

- Use native `MenuBarExtra`, `Settings`, `Form`, `List`, and `NSOpenPanel`.
- Pairing confirmation is monospaced, prominent, and never color-only.
- Workspace rows show display name and host-local path; iOS receives only the
  approved display data through the Rust protocol.
- Empty, setup, unavailable, and approval states explain the next action.
- Paired-device rows show the device name plus a selectable identity; revoke
  is an explicit destructive action.
- Active-session rows name the authorized workspace and offer Release only.
- No chat, terminal, file browser, Git UI, or conversation cache belongs here.
