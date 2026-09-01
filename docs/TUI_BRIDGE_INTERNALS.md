---
title: TUI bridge internals
description: Host-local transport, ownership, and reconnect details for the Pix Pi TUI bridge.
---

This page keeps implementation details out of [Use Pix with Pi TUI](/docs/pi-tui-bridge).
It describes the current public package and host boundary.

## Host-local transport

The bridge uses a Unix socket on the same computer as Pi and Pix Host. With the
default configuration its path is:

```text
$HOME/.config/pix/run/tui-bridge.sock
```

When `PIX_CONFIG` points to another configuration file, the host uses the
`run/` directory beside that file. The bridge is separate from `pix-wire`; it
does not add another network path.

The host obtains the socket peer's operating-system UID and PID, rechecks the
process start identity, and uses those credentials when accepting a TUI
registration. The registration payload cannot choose an owner PID.

## Ownership and source of truth

The host validates the TUI session and workspace before granting a `PiTui`
lease. TUI and Pix RPC claims use the same durable session lock. A live TUI
owner blocks a Pix RPC spawn, including while the TUI socket is temporarily
unreachable.

Pi's native JSONL file remains the durable conversation source of truth. The
host may keep an in-memory, content-free history index and an unavailable
runtime placeholder for a disconnected TUI owner; neither is a second message
store.

## Snapshot and event ordering

After registration, bounded sequenced bridge events are mapped through the Pi
compatibility adapter. A TUI snapshot includes the partial assistant state and
the sequence covered by that snapshot. Pix renders the snapshot first, then
applies later events.

The host command subset is `prompt`, `abort`, `model.list`, `model.set`,
`thinking.set`, and `session.rename`. The prompt path is text-only. Steer,
follow-up, compact, fork, and shutdown remain unsupported for TUI owners.

## Reconnect and release

After a successful attach, a lost socket triggers background reconnect attempts
after 1, 2, 5, and 10 seconds, then with a 30-second cap. A TUI that began
standalone because the host was unavailable does not claim its session later by
itself; `/reload` is the explicit retry.

If the host was reachable before Pi wrote the first JSONL session file, the
bridge gets one bounded claim retry after Pi reports the first settled agent
run. `/resume` uses a five-second preclaim for the target session. `/new`,
`/fork`, `/quit`, and signal-driven shutdown emit a `session_release` marker;
extension reload keeps the lease for same-process reconnect.

## Package boundary

The extension source and manifest live under
[`packages/pix/`](https://github.com/ZainCheung/pix/tree/main/packages/pix). Pi
loads `index.ts` through the `pi.extensions` entry in `package.json`. The
extension calls Pi's official Extension API and never replaces the `pi`
executable.

For the user installation and controls, return to [Use Pix with Pi TUI](/docs/pi-tui-bridge).
