---
title: Develop Pix
description: Build Pix locally, understand its boundaries, and contribute changes.
---

Build Pix locally, understand its architecture, and contribute changes without
crossing component boundaries.

Looking for help using Pix? Start with the [user documentation](/docs).

## Prerequisites

- Rust 1.91 or newer, matching the workspace toolchain.
- A Pi installation that meets the [compatibility requirements](/docs/compatibility).
- Linux or macOS for Host development. Check the released targets in
  [Platform support](/docs/platform-support).
- Node.js and npm when changing the relay, the Pi package, or website/docs
  tooling.
- Xcode when changing the macOS app.

From an installed Pix CLI, `pix status` checks the selected Pi executable and
reports whether its version and startup options are supported. Use
`pix pi set /absolute/path/to/pi` when development should use a different
executable.

## Build the CLI

From the repository root:

```bash
cargo build --workspace
cargo build --release -p pix-cli --locked
```

The release binary is `target/release/pix`. The workspace uses the same
`pix-core` and `pix-wire` crates for debug and release builds.

## Run a development host

Run the CLI in the foreground with an isolated configuration while testing
stateful changes:

```bash
cargo run -p pix-cli -- --config /tmp/pix.json status
cargo run -p pix-cli -- --config /tmp/pix.json workspace add /tmp/pix-workspace
cargo run -p pix-cli -- --config /tmp/pix.json serve
```

`pix serve` accepts `quit` or `exit` on stdin. The selected configuration path
also determines the host state, control and event sockets, logs, and temporary
Pi context guard. Keep `/tmp/pix.json` and the workspace above separate from a
normal Pix installation.

For setup and pairing behavior, run the same binary with `setup` or
`device pair`. The [user pairing flow](/docs/pairing) describes the product
behavior; the [service page](/docs/services) describes the persistent host.

## Run the macOS app

The public client is the SwiftUI menu-bar app under `apps/macos/`. Build and
test it on a Mac with the `Pix` scheme:

```bash
cd apps/macos
xcodebuild -project Pix.xcodeproj -scheme Pix \
  -destination 'platform=macOS' build
xcodebuild test -project Pix.xcodeproj -scheme Pix \
  -destination 'platform=macOS'
```

`Pix.xcodeproj` is committed, so normal build and test commands use it
directly. If you modify `project.yml`, install XcodeGen and run
`xcodegen generate` before building or testing to refresh the project.

CI disables code signing for this test. The app embeds the matching Rust CLI
for a source checkout; see the [macOS README](https://github.com/ZainCheung/pix/blob/main/apps/macos/README.md)
for the menu-bar development loop and service-owner details.

## Run the relay locally

The content-blind relay lives under `relay/`:

```bash
cd relay
npm ci
npm test
npm run typecheck
npm run dev
```

The relay forwards encrypted records and does not receive the Pix channel
secret or application payload. Relay deployment is a release operation; see
[Self-host a relay](/docs/self-host-relay) and the [release workflow](/docs/release).

## Run tests

The short pre-review loop is:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

Use [Testing](/docs/testing) to choose a focused suite, check protocol
fixtures, test the website, or run the CI path-classification test. The
[release workflow](/docs/release) owns packaging and signing checks.

## Repository map

| Area | Responsibility |
| --- | --- |
| `crates/pix-core` | Host control-plane primitives: workspaces, pairing, transports, Pi processes, sessions, and TUI ownership. |
| `crates/pix-wire` | Versioned Pix application protocol, encrypted framing, Noise channel support, validation, and wire fixtures. |
| `crates/pix-cli` | The `pix` diagnostic, setup, workspace, pairing, session, relay, and service CLI. |
| `apps/macos` | Public SwiftUI menu-bar client and its local Host service bridge. |
| `packages/pix` | Optional `@zaincheung/pix` Pi extension for the host-local TUI bridge. |
| `relay` | Cloudflare Worker that forwards one host/client encrypted channel. |
| `protocol` | Versioned schema and canonical protocol fixtures. |
| `packaging` | Linux, macOS, and Apple wire build/package scripts used by releases. |
| `website` | Fumadocs site, route generation, and documentation validation. |
| `skills` | Agent-facing Pix CLI instructions shipped with the repository. |

The iOS client is private and consumes the public `pix-wire` boundary. It is
not another source tree in this repository; see [Repository boundary](/docs/repository).

## Where facts belong

Keep one authoritative page for facts that change:

| Fact | Source of truth |
| --- | --- |
| User workflows | [Start and Use Pix](/docs) |
| Product model and trust boundaries | [Understand Pix](/docs/how-pix-works) |
| Platform support | [Platform support](/docs/platform-support) |
| Pi and protocol compatibility | [Compatibility](/docs/compatibility) |
| CLI syntax | [CLI reference](/docs/cli) |
| Configuration contract | [Configuration](/docs/configuration) |
| Service lifecycle | [Service management](/docs/services) |
| Application protocol schema | [`protocol/schema/v1.md`](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md) |
| Protocol architecture | [Wire protocol](/docs/wire-protocol) |
| Pi command mapping | [Pi RPC coverage](/docs/pi-rpc-coverage) |
| TUI ownership | [TUI bridge internals](/docs/tui-bridge-internals) |
| Test commands | [Testing](/docs/testing) |
| Release process | [Release workflow](/docs/release) |
| Repository boundaries | [Repository boundary](/docs/repository) |

Before changing a component boundary, read [Architecture](/docs/architecture).
For contribution rules, read
[CONTRIBUTING.md](https://github.com/ZainCheung/pix/blob/main/CONTRIBUTING.md).
