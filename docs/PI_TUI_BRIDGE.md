---
title: Pi TUI bridge
description: Mirror an interactive Pi TUI session in Pix App with the optional @zaincheung/pix extension.
---

The `@zaincheung/pix` package is an optional Pi extension that connects an
interactive Pi TUI session to Pix App on the same computer. Pi remains the
agent and its native JSONL session remains the durable source of truth. The
extension gives Pix App a live view of the TUI session and lets the app send a
bounded set of controls back to Pi.

## Install

Install Pix first, then run `pix setup` so the host service is available:

```sh
pi install npm:@zaincheung/pix
```

Restart Pi, or run `/reload` in an existing Pi session, after installing. The
extension only attaches to Pi's interactive TUI mode. If the Pix host is not
running, Pi continues as a standalone TUI and shows no Pix status.

If an older Pix release installed a copy at
`~/.pi/agent/extensions/pix-bridge`, remove that legacy copy before enabling
the package. Pi loads both locations when both are present.

## Connection and status

The extension connects to a host-local Unix socket. With the default
configuration, the socket is:

```text
$HOME/.config/pix/run/tui-bridge.sock
```

When `PIX_CONFIG` points to another configuration file, Pix uses the `run/`
directory beside that file. The Pix host validates the session and workspace
before granting the bridge lease. A successful attachment adds `Pix running`
to the Pi TUI footer.

This bridge stays on the computer and is separate from `pix-wire`. It does not
create another network path or another session database.

## What syncs

Pi sends Pix App:

- the current session snapshot, including messages, model, thinking level, and
  active tools;
- assistant and tool execution updates while an agent run is active;
- session, compaction, and agent lifecycle events.

Pix App can send Pi:

- text prompts and abort requests;
- model-list requests, model selection, and thinking-level changes;
- command-list requests and a session rename.

Prompts sent through a TUI owner are text-only. Image attachments are not
supported on this bridge yet.

## Session ownership

Each Pi session has one live writer. If Pix or another Pi TUI already owns a
session, the extension warns the TUI and closes it instead of allowing two
processes to write the same JSONL session.

When you use `/resume`, the extension asks the host to check the target session
before Pi switches. `/new`, `/fork`, `/quit`, and signal-driven shutdown release
the current bridge lease. A temporary socket loss triggers bounded reconnect
attempts. If the host is unavailable, the TUI stays usable on its own; run
`/reload` after starting the host when you want to attach it.

## Development

The package source and manifest live under [`packages/pix/`](https://github.com/ZainCheung/pix/tree/main/packages/pix).
Pi loads [`index.ts`](https://github.com/ZainCheung/pix/blob/main/packages/pix/index.ts)
through the `pi.extensions` entry in `package.json`. The extension connects to
the host socket and never replaces the `pi` executable.
