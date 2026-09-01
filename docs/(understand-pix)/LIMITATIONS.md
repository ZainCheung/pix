---
title: Limitations
description: Current Pix boundaries and unsupported paths, stated plainly.
---

These are current product limits, not a roadmap.

## Host and Pi requirements

- The documented installer targets Apple Silicon macOS and Linux x86_64 or
  ARM64. Windows service integration is not included.
- Pix checks Pi compatibility with the range listed in
  [Installation](/docs/installation) and requires Pi to expose the RPC options
  used by the host.
- The computer running Pi must be powered on and reachable for remote access.
- Direct access requires the iPhone and host to share a local network. Remote
  access through different networks depends on a configured relay.

## Relay limits

- A relay outage removes remote reachability. Pi and local sessions continue on
  the host.
- Pix setup supports the hosted relay or a configured `ws://`/`wss://`
  endpoint. Custom direct endpoints are not available in the setup flow.

## Pi TUI bridge

The optional `@zaincheung/pix` extension attaches interactive Pi TUI sessions
only. Its command surface is smaller than native Pi:

- prompts are text-only; image attachments are not supported;
- Pix can request models and commands, change the model or thinking level,
  abort a run, and rename a session;
- steer, follow-up, compact, fork, and shutdown controls are not available for
  TUI owners.

Pi remains usable as a standalone TUI when the host is unavailable.

## Attachments and RPC surface

Native Pix image attachments accept PNG, JPEG, WebP, and GIF data in bounded
uploads. Current clients can attach up to nine images to one request. Each
image can be up to 4 MiB; older clients are limited to four images.

Pix does not expose every Pi RPC operation. Terminal `bash` controls, session
fork/tree operations, HTML export, and Pi retry or compaction policy controls
remain outside the Pix surface. See the [Pi RPC coverage matrix](/docs/pi-rpc-coverage)
for the current mapping.

## Data and availability

Pix does not provide a cloud copy of Pi history or an alternative agent runtime.
A paired phone cannot use the host's resources while the host is offline.

For the product model behind these limits, see [Pix and Pi](/docs/pix-and-pi)
and [Local-first by design](/docs/local-first).
