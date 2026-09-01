---
title: Repository boundary
description: See which Pix components are public, private, versioned, and intentionally separated.
---

`pix` is public source. The SwiftUI macOS menu-bar client is maintained under
`apps/macos/`; the SwiftUI iOS client remains in a separate private repository.

The Rust workspace lives under `crates/` (`pix-cli`, `pix-core`, and
`pix-wire`). The optional Pi TUI bridge is the public npm package under
`packages/pix/`; it connects Pi's Extension API to the host-local socket and
does not replace the Pi runtime. The protocol contract, Relay, and platform
packaging remain separate top-level boundaries.

The compatibility boundary is versioned `pix-wire` plus
`protocol/schema/v1.md` and `protocol/fixtures/v1`. The private iOS client
pins a Host tag and exact commit and builds the XCFramework from that checkout.
The public macOS target embeds a CLI built from the same source commit for
source and release builds.

The public repository contains no Apple signing material, App Store Connect
metadata, private workspace paths, or production Cloudflare credentials.
