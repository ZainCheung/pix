# Pix macOS instructions

Pix for macOS is a public SwiftUI menu-bar client for the open-source Pix
Host. Keep it a host control surface; do not add a desktop chat UI.

- Rust `pix-core` owns Host networking, pairing, workspace authorization, and
  Pi lifecycle.
- The app talks to the platform-managed Host through the config-scoped control
  and event sockets. It must not start a competing foreground daemon.
- SwiftUI owns menu-bar presentation, folder pickers, settings, and approval
  interaction.
- Do not implement cryptography, encrypted framing, or durable conversation
  storage in Swift.
- Host private keys remain in Keychain/secure storage and never enter JSON
  config or logs.
- Build with `xcodegen generate` when `project.yml` changes and verify with
  `xcodebuild`.
- Do not commit signing certificates, provisioning profiles, private keys,
  production credentials, or local workspace paths.
