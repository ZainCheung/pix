---
title: Wire protocol
description: Understand pix-wire layers, compatibility, frame limits, and protocol fixtures.
---

`pix-wire` is the shared Rust boundary for Pix's versioned application
messages and authenticated encrypted channel. The host and Apple clients use
this implementation for encoding, validation, framing, and cryptography.

The exact message schema remains in
[`protocol/schema/v1.md`](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md).
This page explains the boundary around that schema.

## What pix-wire owns

`crates/pix-wire` owns:

- protocol v1 envelopes and validation;
- length-prefixed encrypted frame encoding and decoding;
- Noise XX pairing and Noise IK reconnect handshakes, followed by ordered
  authenticated transport records;
- capability vocabulary and wire representations used by negotiated feature
  boundaries; and
- the Rust/UniFFI surface consumed by the Apple clients.

`pix-core` owns host behavior and Pi command adaptation. The relay only
forwards encrypted records. Neither layer should reimplement framing or
cryptography.

## Protocol layers

```text
Pix application messages
            │
            ▼
pix-wire encoding and validation
            │
            ▼
authenticated + encrypted secure channel
            │
            ▼
direct TCP or WebSocket relay transport
```

Application messages are encoded before the secure channel encrypts them. A
direct connection and a relay connection carry the same encrypted records. The
relay terminates WebSockets, but it does not terminate the Pix secure channel
or see plaintext application messages.

## Versioning and capabilities

Pix currently uses protocol major version `1`. The major stays stable while
new behavior is added as per-connection capabilities:

1. A client declares the capabilities it understands in `host.snapshot`.
2. The host reports the capabilities it can honor.
3. The usable set is the intersection. The host omits fields and events gated
   by capabilities the client did not declare.

Unknown capability strings are ignored. An older host can therefore accept a
new client's request and return the base v1 response without the optional
`capabilities` field; the client falls back instead of requiring a major
protocol change. Keeping major v1 stable avoids coordinated upgrades, but each
new field or event must remain gated and have a safe base-v1 behavior.

The current capability list and exact request/event fields are maintained in
the [v1 schema](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md).
The [Pi RPC coverage matrix](/docs/pi-rpc-coverage) maps those protocol
operations to Pi's interface.

## Frame limits and bounded payloads

Pix keeps each encrypted application frame bounded. The `pix-wire` constant
`MAX_ENCRYPTED_FRAME_BYTES` is 1 MiB. Text fields and pending requests have
separate limits. Larger logical values use bounded strategies rather than
raising the frame ceiling:

- image uploads use `attachment.begin`, bounded chunks, and `attachment.finish`;
- long Pi histories use recent windows and opaque-cursor pages;
- oversized or unrenderable history records can use semantic placeholders; and
- historical images can use lazy references and bounded `image.get` ranges.

These strategies let live events continue while a client reads older data.
Keep exact field shapes and current numeric limits in the
[versioned schema](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md)
and the constants in [`crates/pix-wire/src/lib.rs`](https://github.com/ZainCheung/pix/blob/main/crates/pix-wire/src/lib.rs).

## Fixtures and compatibility checks

`protocol/fixtures/v1/` contains canonical Rust-generated envelopes, pairing
artifacts, relay derivations, and frame-limit cases. The Rust fixture tests
decode and re-encode every request and event fixture, reject invalid examples,
and check the pairing and frame helpers. Relay tests consume the relay-channel
fixture as well.

When a protocol serializer, derivation, or schema changes, regenerate the
fixtures from the Rust implementation and review the diff:

```bash
cargo run -p pix-wire --example generate_fixtures
cargo test -p pix-wire
```

Do not regenerate fixtures for a documentation-only change. Run the broader
suite described in [Testing](/docs/testing) when a protocol change also affects
the host, relay, or Apple wire artifact.

## Exact specification

Use [`protocol/schema/v1.md`](https://github.com/ZainCheung/pix/blob/main/protocol/schema/v1.md)
for envelope fields, request and event inventories, capability names, history
cursor rules, attachment behavior, relay headers, and resource limits. Keep
this page at the architectural level so the schema remains the single place
where wire-level details can change.
