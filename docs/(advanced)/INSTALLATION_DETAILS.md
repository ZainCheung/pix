---
title: Installation details
description: Package, archive, source-build, service, and uninstall details for Pix.
---

This page keeps alternative installation and service details out of the normal
user path. The recommended route is on [Installation](/docs/installation).

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

Run `pix setup` after installation to authorize a workspace, configure
connectivity, and pair a device.

## Manual archive installation

For a tarball install without a package manager:

```sh
tar -xzf pix-<version>-<target>.tar.gz
mkdir -p "$HOME/.local/bin"
cp pix-<version>-<target>/bin/pix "$HOME/.local/bin/pix"
chmod 0755 "$HOME/.local/bin/pix"
```

Make sure `$HOME/.local/bin` is on `PATH` before running `pix`.

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

Build the public macOS menu-bar client from the
[macOS development guide](https://github.com/ZainCheung/pix/blob/main/apps/macos/README.md).

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
LaunchAgent. Neither requires root. Uninstalling the service preserves the Pix
configuration, host identity, authorized workspaces, and Pi session files.

On macOS, the app's embedded CLI is the canonical owner. Homebrew's `pix`
command is a PATH entry for that same binary. A different CLI can inspect or
control the installed service, but it must use `--adopt` to replace the owner.

## Uninstall

Stop and remove the service first:

```sh
pix service uninstall
```

Then remove the installed CLI and, on macOS, `~/Applications/Pix.app`. The
service command does not delete host configuration or Pi data. Check the path
shown by `pix status` before removing the Pix configuration directory.
