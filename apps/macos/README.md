# Pix macOS

Pix for macOS is a native SwiftUI menu bar host surface. It keeps the desktop
scope deliberately small: Pi detection, explicitly authorized workspaces,
pairing approval, remote pairing QR presentation, paired-device revoke,
active-session release, launch at login, diagnostics, and service status.

When the host has a relay endpoint configured (`pix relay set <url>`), the
menu offers **Pair iPhone Remotely…**, which asks the host service for a
two-minute single-use pairing channel and renders it as a QR code. Approval
still happens in this app with the same six-digit confirmation as local
pairing, and the relay never sees plaintext or pairing secrets.

The Rust host core remains the authority for secure transport, workspace
boundaries, paired-device persistence, and Pi process ownership. The menu bar
app installs/starts the platform-managed Host service, consumes only
payload-free events from the config-scoped event socket, and sends explicit
approval commands over the mode-0600 control socket. The app never starts a
competing foreground daemon.

## Run locally

Pix for macOS is a menu bar app. It has no Dock icon. After Run, look at the
status item on the right side of the menu bar.

1. For a source checkout, build the public Host CLI:

```bash
cargo build --release -p pix-cli --locked
export PIX_CLI="$PWD/target/release/pix"
"$PIX_CLI" doctor
```

2. Open `Pix.xcodeproj` in Xcode and run the `Pix` scheme on **My Mac**.
3. If macOS shows **Developer Tools Access**, type your Mac login password.
   That prompt is Xcode's debugger asking to attach. It is not a Pix crash.
4. Click the Pix menu bar icon → **Add Workspace…**, then start the iOS app
   on a phone or simulator on the same LAN.

Xcode GUI launches do not inherit your shell `PATH`. The app looks for `pix`
inside a release bundle first, then in the current `PATH`, the interactive
login-shell `PATH`, and common user install paths such as `~/.local/bin` and
mise shims. Override with `PIX_CLI` if needed during development.

The release workflow builds and embeds the matching Rust CLI from the same
source commit at
`Pix.app/Contents/Resources/pix`, so an installed release does not depend on
Cargo or a pre-existing `pix` executable. The CLI stores its long-term host
identity in macOS Keychain; the mode-0600 file is only a migration/development
fallback.

## Develop

```bash
xcodegen generate
xcodebuild -project Pix.xcodeproj -scheme Pix \
  -destination 'platform=macOS' build
xcodebuild test -project Pix.xcodeproj -scheme Pix \
  -destination 'platform=macOS'
```

Unsigned Debug/CI builds use `CODE_SIGNING_ALLOWED=NO`. A local signed build
may provide `DEVELOPMENT_TEAM` and `CODE_SIGN_IDENTITY` as xcodebuild overrides;
no signing material is stored in this repository.

## Local release-bundle check

The public release script can produce an unsigned archive. Developer ID
signing and notarization are opt-in through environment variables; production
builds must pass Gatekeeper assessment after stapling.

After building, verify the bundle with (production builds run both signature and
Gatekeeper checks):

```bash
../../packaging/macos/verify-release.sh ../../build/macos-release/Pix.app
```

For an unsigned source build, use `MACOS_SKIP_CODESIGN=1`. Do not use that
override for a distribution check.
