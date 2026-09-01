---
title: Installation
description: Install the Pix host on a supported Mac or Linux computer.
---

Pix runs next to Pi on your computer. Install the host there first, then run
`pix setup` to choose a workspace, configure connectivity, and pair a phone.

## Install

The first-party installer is the recommended path. It installs without root
privileges:

```sh
curl -fsSL https://pix.deepoke.com/install.sh | sh
```

The installer puts `pix` in `~/.local/bin`. On Apple Silicon macOS it also
installs `Pix.app` in `~/Applications`. If the installer asks, add
`~/.local/bin` to `PATH`.

The installer supports Apple Silicon macOS and Linux x86_64 or ARM64. Pix
checks that a compatible Pi installation is available when setup runs. See
[Platform support](/docs/platform-support) for the release matrix and
[Compatibility](/docs/compatibility) for the current Pi range.

## macOS

You can use the app and CLI installed above. The release page also publishes a
macOS arm64 archive.

### Homebrew

The repository provides a first-party Homebrew Cask:

```sh
brew tap ZainCheung/pix https://github.com/ZainCheung/pix.git
brew install --cask ZainCheung/pix/pix
```

The current published macOS build is Apple Silicon and requires macOS Sonoma
or newer. The Homebrew `pix` command points to the CLI inside `Pix.app`.

## Linux

The installer selects the Linux x86_64 or ARM64 release for the machine it is
running on. If you need a package or archive instead, choose the matching
asset from the [latest release](https://github.com/ZainCheung/pix/releases/latest)
and then run `pix setup`.

## Verify

Check the Pi executable and host before pairing:

```sh
pix status
pix pi show
```

`pix status` probes the installed Pi and shows the host-service state. `pix pi
show` shows the executable Pix will use; use `pix pi set` when PATH discovery
selects the wrong installation.

For alternative install methods, background-service commands, and uninstall
details, see [Installation details](/docs/installation-details).

## Next

[Pair your iPhone](/docs/pairing) to finish the first-use setup. If you have
not chosen a workspace yet, [Quickstart](/docs/quickstart) keeps the complete
path in one place.
