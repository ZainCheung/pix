---
title: Pix Host architecture
description: Understand Pix's runtime components, boundaries, lifecycle, and invariants.
---

Pix Host is the local control plane around one Pi runtime. It authorizes
workspaces and devices, owns secure connections, starts or attaches to Pi
processes, and maps Pi events to Pix clients. Pi's native JSONL session remains
the durable conversation source; Host does not maintain a message database.

## Runtime components

| Component | Responsibility |
| --- | --- |
| `pix-core` | Host control-plane behavior: workspaces, pairing, transports, Pi process lifecycle, session locks, and the optional TUI bridge. |
| `pix-wire` | The versioned application protocol, encrypted framing, Noise channel support, validation, and the Apple FFI boundary. |
| `pix-cli` | Setup, diagnostics, workspace and device management, session operations, and service lifecycle commands. |
| Pix Host service | The long-lived `pix serve --service` process that keeps host state and remote connections available. |
| macOS client | Public SwiftUI menu-bar UI and a local bridge to the Host service. |
| iOS client | Private client that uses the public `pix-wire` boundary to control a supported Host. |
| Pi | The only agent runtime. Pix starts or attaches to Pi; it does not replace Pi. |
| Relay | An optional content-blind transport for an encrypted host/client channel. It does not run Pi. |

The [repository map](/docs/development#repository-map) lists where each
component lives. Exact CLI syntax belongs in the [CLI reference](/docs/cli),
and service commands belong in [Service management](/docs/services).

## Process ownership

The Host service starts one `pix serve --service` process for the selected
configuration. The CLI and macOS app use the service's local control and event
sockets; they do not start a competing foreground daemon. The service manager
is a per-user LaunchAgent on macOS and a `systemd --user` unit on Linux.

Pi processes remain separate child processes owned by the Host runtime. Each
active session has a single live writer, whether that writer is a Pix RPC
process or the optional Pi TUI extension. The TUI extension is a separate
`@zaincheung/pix` package and talks to Host through a host-local Unix socket.
See [TUI bridge internals](/docs/tui-bridge-internals) for its ownership and
reconnect protocol.

## Network boundaries

```text
                         ┌─ direct TCP ───────────┐
Pix client ──────────────┤                         ▼
                         └─ WebSocket relay ── Pix Host ── Pi RPC ── Pi
```

Direct TCP and the relay carry the same length-prefixed encrypted records. The
relay authenticates channel roles and forwards opaque binary frames; it does
not terminate the Pix secure channel, parse application messages, run Pi, or
store application payloads. The local TUI socket is a separate process-local
surface and is not a second network transport for Pix clients.

The Host only accepts work in explicitly authorized workspace roots. A remote
client can request a discovered workspace or session through the Host protocol;
it cannot browse arbitrary paths on the computer.

For protocol layering, capability negotiation, and frame behavior, read
[Wire protocol](/docs/wire-protocol). For the exact request and event schema,
use the versioned [`protocol/schema/v1.md`](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md).

## Trust boundaries

The paired client authenticates to Host through the `pix-wire` secure channel.
Host remains the authority for device approval, workspace authorization, and
session ownership. Pi runs behind that boundary and receives the operations
that Host maps to its local RPC interface. A relay can route a channel and
record connection metadata, but it cannot read the encrypted application
messages. See [Security and privacy](/docs/security) for the user-facing
security boundary.

## Durable and ephemeral state

Durable state stays on the host computer:

- Pi's native JSONL files contain the authoritative conversation history.
- Pix configuration stores the host identity, display name, authorized
  workspaces, paired devices, relay settings, and selected Pi executable.
- Host identity recovery material and platform service ownership are local
  files or operating-system credential-store entries.

The Host keeps runtime state in memory or under its configuration directory:

- active Pi processes, authenticated connections, request ledgers, and TUI
  owner records;
- transient service events, logs, queues, and session snapshots; and
- content-free indexes used to page native JSONL history and derived image
  assets used for bounded transfer.

These runtime structures support reconnects and bounded reads. They do not
become a second durable conversation store. [Sessions and ownership](/docs/session-ownership)
and [Configuration](/docs/configuration) describe the persistence boundaries
in more detail.

## Lifecycle

1. The per-user service starts Host with its selected configuration path.
2. Host loads local identity, paired devices, authorized workspaces, and
   transport settings.
3. A client connects directly or through the relay. `pix-wire` authenticates
   the channel and negotiates the capabilities for that connection.
4. Host authorizes workspace and session requests, then starts or attaches to
   the matching Pi runtime. A TUI registration must satisfy the same session
   lock before it can write.
5. `pix-core` maps Pi RPC responses and events to Pix messages. `pix-wire`
   encodes and encrypts them before the transport sends them.
6. A client or relay disconnect changes reachability. Pi and its native session
   remain on the host, and a later connection can attach again when ownership
   permits it.

## Architectural invariants

Future changes must preserve these constraints:

- Pi remains the only agent runtime.
- Pi's native JSONL remains the sole durable source of conversation content;
  Pix has no cloud session database.
- Only explicitly authorized workspace roots are usable through Host.
- One live writer owns a Pi session at a time. A TUI owner and a Pix RPC owner
  use the same session lock.
- Relay transport forwards encrypted Pix frames. The relay does not decrypt,
  parse, queue, persist, or replay application messages and never runs Pi.
- `pix-wire` is the single implementation of encrypted framing and protocol
  validation used by the host and Apple clients.
- Protocol extensions are additive and capability-gated within major version
  1. The versioned schema remains the authority for exact fields and values.
- The Host service is the long-lived local owner of remote client state; the
  CLI and macOS app attach to it instead of creating competing daemons.
- Configuration mutations are written atomically so an update to one area does
  not discard unrelated trust or workspace state.

These boundaries have costs: one writer prevents simultaneous interactive
writers, relay use adds a relay-availability dependency, and remote control
requires the host computer to remain reachable.

## Where to go deeper

- [Wire protocol](/docs/wire-protocol) explains protocol layers, versioning,
  capabilities, limits, and fixtures.
- [Pi RPC coverage](/docs/pi-rpc-coverage) maps Pi commands and events to Pix
  operations.
- [TUI bridge internals](/docs/tui-bridge-internals) documents the host-local
  writer and reconnect lifecycle.
- [Repository boundary](/docs/repository) defines the public and private
  component split.
- [Testing](/docs/testing) maps changes to the suites that protect them.
