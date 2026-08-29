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
  connections, Pi process lifecycle, RPC adaptation, the one-writer invariant,
  and the optional host-local TUI ownership harness.
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

## CLI ownership on macOS

The release App bundle is the canonical Pix CLI distribution. The Homebrew
Cask exposes `Pix.app/Contents/Resources/pix` in `PATH`; it does not install a
second implementation. The macOS app resolves that embedded binary by
default, while `PIX_CLI` is an explicit development override.

The per-user LaunchAgent records its CLI owner in the configuration's
`service-owner.json`. `service start`, `stop`, `restart`, and `status` operate
on the installed owner without silently replacing it. `service install` only
replaces a different owner when `--adopt` is supplied; the App uses that flag
when it is explicitly launched so the service returns to its matching embedded
CLI. The service manager exposes one per-user Pix service identity; a
standalone CLI may control that existing service, but an independent CLI-only
daemon must use a separate service identity rather than competing with the
App-managed host.

## Transport

LAN direct TCP and the outbound WebSocket relay carry the same encrypted wire
frames. The relay is content-blind: it authenticates channel roles, forwards
opaque binary frames, applies size/rate/connection limits, and stores no
application payload.

The optional Pi TUI bridge is a separate host-local NDJSON surface. Its Unix
socket adapter obtains peer UID/PID from the operating system, rechecks the
process start identity, and passes only those credentials into the ownership
registry; REGISTER payloads never declare an owner PID. TUI owner records share
the same session lock as Pix RPC, survive a Host disconnect, and appear to the
runtime manager as an unavailable placeholder until a later event/snapshot
adapter is installed. The bridge is not part of `pix-wire` and does not yet
forward conversation events to remote clients.

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
- A live `PiTui` owner blocks a Pix RPC spawn even when its bridge socket is
  temporarily unreachable; TUI owners do not consume the Pix RPC process
  capacity budget.
- Relay loss changes reachability only; Pi continues locally.
- Logs are payload-free and never contain prompts, files, model output, keys,
  tokens, or relay secrets.
- Wire extensions are capability-gated per connection: a host never emits a
  gated event or field to a client that did not declare it
  (`protocol/schema/v1.md`).
- Image attachments are staged in bounded connection memory, then persisted
  atomically below the Pix configuration directory as a session-scoped
  `attachments/v1/<session>/<attachment-key>/` asset (client attachment ID
  for new uploads, vision hash for recovered history). The source, agent-compatible,
  and vision paths are explicit; decodable images get a <=2000×2000 vision
  derivative and malformed/unsupported bytes remain a byte-for-byte fallback.
  Pi still receives the vision bytes in `images[]`, while the agent-compatible
  paths are appended to the prompt for filesystem-aware workflows. History
  clients that declare `image_refs.v1` receive `imageRef` entries and fetch
  bounded chunks lazily; raw Pi `ImageContent` remains the durable source of
  truth.
- Session snapshots return Pi state and messages first. Clients declaring
  `session_metadata.v1` receive commands, usage, and thinking-level choices in
  a later unsolicited `session.metadata` event. Legacy clients keep the
  fields in the snapshot, but each optional probe has a short best-effort
  deadline so it cannot hold the connection indefinitely.

## Pi RPC coverage

`docs/PI_RPC_COVERAGE.md` tracks which Pi RPC commands and events are
exposed, capability-gated, or intentionally omitted. Pi-specific field names
stop at `pix-core/src/pi_bridge.rs`.
