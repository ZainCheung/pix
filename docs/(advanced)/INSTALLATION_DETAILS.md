---
title: Installation details
description: Alternative package, archive, source-build, update, and uninstall details for Pix.
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
cargo run -p pix-cli -- status
```

Build the public macOS menu-bar client from the
[macOS development guide](https://github.com/ZainCheung/pix/blob/main/apps/macos/README.md).

## Updating

For a released installer or app installation, update in place with:

```sh
pix update
```

The command downloads the matching latest release asset and replaces the
running CLI. On macOS it also updates `~/Applications/Pix.app` when the app
bundle is available. If a host service is running, restart it after the update
so the service uses the new executable. A source build should be rebuilt with
Cargo instead.

## Background service

`pix setup` installs and starts a per-user service on supported macOS and Linux
hosts. For service commands, ownership checks, and uninstall behavior, see
[Service management](/docs/services).

## Uninstalling Pix

Stop and remove the service first:

```sh
pix service uninstall
```

Then remove the installed CLI and, on macOS, `~/Applications/Pix.app`. The
service command does not delete host configuration or Pi data. Check the path
shown by `pix status` before removing the Pix configuration directory.
