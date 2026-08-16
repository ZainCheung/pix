# Pix Host architecture

Pix Host is a small Rust process that owns workspace authorization, paired
device records, secure connections, Pi child processes, and Pi RPC sessions.
Pi's native JSONL session remains the only durable conversation source of
truth; Host does not maintain a message database.

## Crates

- `pix-wire` owns protocol versioning, canonical envelopes, frame limits,
  Noise XX/IK handshakes, encryption, replay protection, and the UniFFI API
  consumed by the private Apple clients.
- `pix-core` owns workspace boundaries, pairing, Bonjour/direct TCP, relay
  connections, Pi process lifecycle, RPC adaptation, and the one-writer
  invariant.
- `pix-cli` exposes `serve`, diagnostics, workspace management, pairing, and
  service operations on supported hosts.

## Transport

LAN direct TCP and the outbound WebSocket relay carry the same encrypted wire
frames. The relay is content-blind: it authenticates channel roles, forwards
opaque binary frames, applies size/rate/connection limits, and stores no
application payload.

## Apple boundary

The private iOS and macOS clients own UI, native sockets, Keychain integration,
and disposable view state. They must use the Rust `pix-wire` implementation and
must not reimplement cryptography, framing, or durable session storage.

## Security invariants

- Only explicitly authorized canonical workspace roots are usable.
- A Pi session has one writer process at a time.
- Relay loss changes reachability only; Pi continues locally.
- Logs are payload-free and never contain prompts, files, model output, keys,
  tokens, or relay secrets.
