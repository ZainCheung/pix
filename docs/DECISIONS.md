# Pix Host decisions

## Public boundary

The Rust host, `pix-wire`, protocol fixtures, content-blind relay, and the
SwiftUI macOS menu-bar client are open source. The SwiftUI iOS client remains
in a separate private repository.

## macOS CLI ownership

The macOS App bundle is the canonical Pix CLI distribution. Homebrew may expose
the embedded CLI through a PATH launcher, but a different CLI binary must not
silently replace the App-managed service. Service installation records the
owner and requires an explicit `--adopt` to switch it; ordinary lifecycle
commands remain safe to run from another CLI.

## Protocol source of truth

`pix-wire` is the only secure-channel implementation. Client repositories use a
pinned Host release and do not copy Rust protocol code.

## Runtime source of truth

Pi is the only agent runtime and its native JSONL session is the authoritative
conversation store. Pix does not create a second message database.

## Relay privacy

The relay forwards only authenticated encrypted frames. It does not decrypt,
parse, queue, persist, or replay application messages.

## Licensing

Pix Host source is MIT licensed. Third-party code keeps its original license;
release artifacts must ship the corresponding dependency notices.

### Image history and optional session metadata

Pi JSONL remains the authoritative conversation history, including the
original `ImageContent` base64. Pix derives host-local image assets with
content-addressed SHA-256 IDs and atomically writes `source`, `agent`, `vision`,
and `metadata.json` under the configuration directory. A client that opts into
`image_refs.v1` receives lightweight `imageRef` content and retrieves the
vision bytes with bounded `image.get`/`image.chunk` requests. This avoids
shipping every historical image during session attach while preserving
graceful recovery if the derived asset cache is lost.

The base `session.snapshot` path is intentionally independent from optional
Pi probes. `commands.v1`, `usage.v1`, and `thinking_levels.v1` are scheduled
after the snapshot and delivered through `session.metadata` for clients that
declare `session_metadata.v1`; older clients use a short, best-effort inline
fallback for compatibility.

### Bounded session history windows

`session_history.v1` keeps the wire frame ceiling separate from logical
session size. A negotiated attach returns the newest history window (at most
50 messages and a 512 KiB encoded-content target); older windows are requested
with opaque cursors and a fixed revision boundary. The host scans Pi's native
JSONL incrementally instead of calling Pi's unbounded `get_messages` for the
initial view. History pages are independent of live events, and iOS prepends
them while preserving the current scroll anchor. Image entries are
externalized before byte selection when `image_refs.v1` is active.

Model summaries carry Pi's optional `input` modalities. Clients use the
advertised `image` value to gate image composition; the host does not infer
model capability from the existence of an attachment upload.
