---
title: Installation
description: Install the Pix host and optional macOS client on a supported machine.
---

The [homepage](/) covers the shortest install and first-use flow. This page
lists the other supported installation paths.

## First-party installer

The installer resolves the latest GitHub release at install time and does not
need root privileges:

```sh
curl -fsSL https://pix.deepoke.com/install.sh | sh
```

It installs the `pix` executable into `~/.local/bin`. On Apple Silicon macOS it
also installs `Pix.app` into `~/Applications`. Add `~/.local/bin` to `PATH` if
the installer tells you to do so, then run:

```sh
pix doctor
pix setup
```

The installer currently supports macOS Apple Silicon and Linux x86_64 or
ARM64. It does not install the private iOS client.

## macOS

The current release publishes a `pix-<version>-macos-arm64.zip` archive with
the menu-bar app and its matching CLI. Open the archive, move `Pix.app` to
Applications, and use the bundled CLI at
`Pix.app/Contents/Resources/pix`, or use the first-party installer above.

The repository also contains a first-party Homebrew Cask:

```sh
brew tap ZainCheung/pix https://github.com/ZainCheung/pix.git
brew install --cask ZainCheung/pix/pix
```

The fully qualified Cask name limits trust to this requested Cask when
Homebrew asks for trust on a non-official tap. The current published build is
Apple Silicon and requires macOS Sonoma or newer.

## Linux packages

Download the matching asset from the
[latest release](https://github.com/ZainCheung/pix/releases/latest).

Debian or Ubuntu:

```sh
sudo dpkg -i pix_<version>_amd64.deb   # x86_64
sudo dpkg -i pix_<version>_arm64.deb   # ARM64
```

Fedora, RHEL, or another RPM-based distribution:

```sh
sudo rpm -i pix-<version>-1.x86_64.rpm   # x86_64
sudo rpm -i pix-<version>-1.aarch64.rpm  # ARM64
```

The archive and package formats install the `pix` CLI. Run `pix setup` to
authorize a workspace, pair a client, and install the per-user service.

## Manual archive installation

For a tarball install without a package manager:

```sh
tar -xzf pix-<version>-<target>.tar.gz
mkdir -p "$HOME/.local/bin"
cp pix-<version>-<target>/bin/pix "$HOME/.local/bin/pix"
chmod 0755 "$HOME/.local/bin/pix"
```

Make sure `$HOME/.local/bin` is on `PATH` before running `pix`.

## Pi TUI bridge package

Install the optional Pix TUI bridge through Pi's package manager after `pix
setup` has installed the host service:

```sh
pi install npm:@zaincheung/pix
```

The extension mirrors the interactive Pi TUI session in Pix App through a
host-local Unix socket. It forwards session snapshots and live agent/tool
events, and accepts text prompts and session controls from Pix App. Restart Pi,
or run `/reload` in an existing Pi session, after installing it. If the Pix Host
is unavailable, Pi continues to work normally without a bridge status
indicator. See the [Pi TUI bridge guide](/docs/pi-tui-bridge) for ownership and
reconnect behavior.

## Build from source

Source builds need Rust 1.91 or newer and a supported Pi installation:

```sh
git clone https://github.com/ZainCheung/pix.git
cd pix
cargo build --release -p pix-cli --locked
mkdir -p "$HOME/.local/bin"
cp target/release/pix "$HOME/.local/bin/pix"
```

Run the CLI without copying it into `PATH` with:

```sh
cargo run -p pix-cli -- doctor
```

Build the public macOS menu-bar client from
[its development guide](https://github.com/ZainCheung/pix/blob/main/apps/macos/README.md).

## Background service

`pix setup` installs and starts a per-user service on supported macOS and Linux
hosts. To manage it directly:

```sh
pix service install
pix service install --adopt  # only when intentionally changing the CLI owner
pix service status
pix service restart
pix service uninstall
```

The Linux service is a systemd user unit. The macOS service is a per-user
LaunchAgent. Neither requires root, and uninstalling the service preserves
the Pix configuration, host identity, authorized workspaces, and Pi session
files. On macOS, the App's embedded CLI is the canonical owner; Homebrew's
`pix` command is only a PATH entry for that same binary. A different CLI can
inspect or control the installed service, but must use `--adopt` to replace
the owner.

## Uninstall

Stop and remove the service first:

```sh
pix service uninstall
```

Then remove the installed CLI and, on macOS, `~/Applications/Pix.app`. The
service command does not delete host configuration or Pi data. Keep or remove
the Pix configuration directory only after checking the path shown by
`pix status`.
