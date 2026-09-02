---
title: Platform support
description: The released Pix host, client, transport, and service targets.
---

This is the canonical support matrix for the current Pix release. A build
target in CI is not by itself a shipped product.

## Supported release targets

| Platform | Pix Host / CLI | macOS menu-bar app | Pi TUI bridge | Direct LAN / relay | Service | Installer and packages |
| --- | --- | --- | --- | --- | --- | --- |
| macOS Apple Silicon | Supported | Supported | Supported | Supported | LaunchAgent | First-party installer, app archive/DMG, Homebrew Cask |
| Linux x86_64 | Supported | Not shipped | Supported | Supported | `systemd --user` | First-party installer, archive, `.deb`, `.rpm` |
| Linux ARM64 | Supported | Not shipped | Supported | Supported | `systemd --user` | First-party installer, archive, `.deb`, `.rpm` |
| macOS Intel | Not shipped | Not shipped | Not shipped | Not a released workflow | No released integration | Build from source only; no release artifact |
| Windows | Not supported by the current release | Not applicable | Not supported | Not supported | Not supported | No release artifact |
| iPhone / iOS | Client only | Not applicable | Not applicable | Client connects to a supported host | Not applicable | Pix for iPhone; join the [TestFlight beta](https://testflight.apple.com/join/crTbabdp) |

The released macOS app and Homebrew Cask require macOS 14 (Sonoma) or newer
and Apple Silicon. The macOS build helper accepts x86_64 for local builds, but
the release workflow packages only arm64, so that local target is not a
supported release artifact. Linux release jobs publish x86_64 and aarch64
artifacts; the repository does not state a distro-specific minimum version.

## What “supported” means here

Supported means the repository's installer, release workflow, or platform
integration explicitly targets that combination. The host and CLI rows refer
to the machine where Pi runs. The iPhone is a client and never becomes the Pi
runtime.

The optional `@zaincheung/pix` package is a Pi extension. On supported Unix
hosts it connects the interactive TUI to the local Pix host; it does not add a
Windows host or a new agent runtime.

For the exact Pi version and RPC prerequisites, see [Compatibility](/docs/compatibility).
