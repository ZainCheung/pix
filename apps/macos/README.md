# Pix macOS

Pix for macOS is a native SwiftUI menu bar host surface. It keeps the desktop
scope deliberately small: Pi detection, explicitly authorized workspaces,
the menu-bar status surface, local-network instructions, relay configuration,
remote pairing QR presentation, pairing approval, paired-device revoke,
active-session release, launch at login, diagnostics, and service status.

Choose **Add Device…** from the menu bar to open the focused pairing guide.
Pairing starts on the local network, which needs no relay. The same window can
save the `ws://` or `wss://` relay endpoint, restart the managed Host service,
and then offer a remote QR flow. Approval still happens in this app with the
same six-digit confirmation for either transport, and the relay never sees
plaintext or pairing secrets. When the Host receives a pairing request, Pix
automatically brings this window to the front for review.

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
"$PIX_CLI" status
```

The Debug Xcode target also builds and embeds the matching `target/debug/pix`
automatically, so a stale `pix` installed elsewhere on `PATH` cannot shadow
the source checkout. Set `PIX_CLI` explicitly only when you need to use a
different development binary.

2. Open `Pix.xcodeproj` in Xcode and run the `Pix` scheme on **My Mac**.
3. If macOS shows **Developer Tools Access**, type your Mac login password.
   That prompt is Xcode's debugger asking to attach. It is not a Pix crash.
4. Click the Pix menu bar icon → **Workspaces** → **Add Workspace…** to
   authorize a folder, then choose **Add Device…** and start the iOS app on a
   phone or simulator. For local pairing, keep both devices on the same LAN.

Xcode GUI launches do not inherit your shell `PATH`. Unless the explicit
development-only `PIX_CLI` override is set, the app uses the embedded `pix`
first, then the current `PATH`, the interactive login-shell `PATH`, and common
user install paths such as `~/.local/bin` and mise shims.

The release workflow builds and embeds the matching Rust CLI from the same
source commit at
`Pix.app/Contents/Resources/pix`, so an installed release does not depend on
Cargo or a pre-existing `pix` executable. The Homebrew Cask's `pix` command is
only a PATH entry for that same embedded binary; do not install a second CLI
for the App-managed host.

The App is the canonical owner of the macOS LaunchAgent. It may pass
`--adopt` when it installs the service so an explicitly launched App can move
the service back to its matching embedded CLI. A standalone CLI may inspect,
start, stop, or restart the installed service, but `service install` refuses
to replace another CLI's owner unless `--adopt` is supplied explicitly. The
owner path and Pix version are shown by `pix service status`.

The CLI stores its long-term host identity in macOS Keychain and keeps a
mode-0600 recovery copy so a background service can continue when Keychain
interaction is unavailable.

## Develop

For the normal edit/build/restart loop, run this from the repository root:

```bash
scripts/macos-dev-restart.sh
```

It builds the Debug app into `build/macos-debug`, embeds the matching
`target/debug/pix`, updates the LaunchAgent with explicit `--adopt`, restarts
the loaded host so an older DerivedData process cannot remain active, and
opens the new menu-bar app. Set `PIX_MACOS_DEV_NO_OPEN=1` when only the host
service should be refreshed.

The first migration on a machine may require one interactive identity repair:

```bash
build/macos-debug/Build/Products/Debug/Pix.app/Contents/Resources/pix \
  service repair-identity
```

For a plain App build without service replacement, use Xcode's `Run` action or
the commands below.

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
