# Pix Host architecture

Pix Host is a small Rust process that owns workspace authorization, paired
device records, secure connections, Pi child processes, and Pi RPC sessions.
Pi's native JSONL session remains the only durable conversation source of
truth; Host does not maintain a message database.

## Crates

- `pix-wire` owns protocol versioning, canonical envelopes, frame limits,
  Noise XX/IK handshakes, encryption, replay protection, and the UniFFI API
  consumed by the private iOS client.
- `pix-core` owns workspace boundaries, pairing, Bonjour/direct TCP, relay
  connections, Pi process lifecycle, RPC adaptation, and the one-writer
  invariant.
- `pix-cli` exposes `serve`, diagnostics, workspace management, pairing, and
  service operations on supported hosts.

## Host service lifecycle

`pix-cli/src/service/` is the platform boundary for the persistent host:

```text
service/
├── mod.rs       shared CLI contract and lifecycle dispatch
├── linux.rs     systemd --user unit
└── macos.rs     per-user launchd LaunchAgent
```

Windows service integration is intentionally not included yet; the shared
contract leaves room for a future `windows.rs` adapter.

Both managers launch the same `pix serve --service` process. The CLI and the
macOS menu app never start a competing foreground daemon; they attach through
the mode-0600 sockets derived from the selected configuration path:

- `run/host-service.sock` accepts one-line commands (`approve`, `reject`,
  `devices`, `sessions`, `refresh`, `pair-remote`, and lifecycle commands).
- `run/host-events.sock` streams transient JSONL service events and retains no
  history.
- `run/host-service.json` is a liveness record containing only process and
  listener supervision fields.

This shared service instance keeps Bonjour ownership, pairing state, and
encrypted transport stable while `pix device pair` or the menu app performs
approval.

## Transport

LAN direct TCP and the outbound WebSocket relay carry the same encrypted wire
frames. The relay is content-blind: it authenticates channel roles, forwards
opaque binary frames, applies size/rate/connection limits, and stores no
application payload.

## Apple boundary

The public macOS client owns menu-bar/settings UI, native folder pickers,
Keychain integration, and the local Host service bridge. The private iOS client
owns its SwiftUI presentation, native sockets, Keychain integration, and
disposable view state. Both clients must use the Rust `pix-wire`
implementation and must not reimplement cryptography, framing, or durable
session storage.

## Security invariants

- Only explicitly authorized canonical workspace roots are usable.
- A Pi session has one writer process at a time.
- Relay loss changes reachability only; Pi continues locally.
- Logs are payload-free and never contain prompts, files, model output, keys,
  tokens, or relay secrets.
- Wire extensions are capability-gated per connection: a host never emits a
  gated event or field to a client that did not declare it
  (`protocol/schema/v1.md`).
- Image attachments assemble in host memory only, bounded per connection, and
  are consumed by the prompt that references them; nothing is persisted.

## Pi RPC coverage

`docs/PI_RPC_COVERAGE.md` tracks which Pi RPC commands and events are
exposed, capability-gated, or intentionally omitted. Pi-specific field names
stop at `pix-core/src/pi_bridge.rs`.
