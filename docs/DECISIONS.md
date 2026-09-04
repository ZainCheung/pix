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

## Optional TUI integration boundary

The TUI integration uses Pi's official Extension API and a separate host-local
Unix-socket protocol. The Pi-side extension is an independent `@zaincheung/pix`
package installed by Pi; Pix must not shadow, wrap, patch, or replace the
user's `pi` executable. The host-local bridge is not a `pix-wire` version and
never becomes a second conversation store.

One session has one live writer. `PixRpc` and `PiTui` claims use the same
durable session lock; a disconnected TUI retains its owner record and is
represented as an unavailable RuntimeManager placeholder, so a reconnect or
Host restart cannot silently fall back to a second RPC writer. Kernel-derived
peer credentials and PID start identity are required before a TUI REGISTER is
accepted.

`/resume` uses a short host-local preclaim: the active extension asks Host to
validate and reserve the discovered destination session before Pi tears down
the current runtime. The reservation is represented by the same `PiTui`
`SessionLease`, is consumed by a matching same-process REGISTER, and expires
after five seconds. A target already owned by Pix or another TUI rejects the
switch; a missing/unreachable bridge does not make Pi unusable as a standalone
TUI. Session replacement and quit send an explicit `session_release` marker,
whereas extension reload retains the lease for same-process reconnect.

After a successful TUI claim, an unexpected bridge disconnect is retried in
the background with bounded delays (1s, 2s, 5s, 10s, then a 30s cap). A TUI
that starts standalone because Host is reachable but its first session has not
been persisted yet gets one bounded retry after `agent_settled`, when Pi has
written the first JSONL entry. A TUI that started standalone because Host was
unavailable never performs this late claim automatically; `/reload` is the
explicit opt-in retry.

## Relay privacy

The relay forwards only authenticated encrypted frames. It does not decrypt,
parse, queue, persist, or replay application messages.

## Licensing

Pix Host source is licensed under GPL-3.0-only. Third-party code keeps its
original license; release artifacts must ship the corresponding dependency
notices.

### Image history and optional session metadata

Pi JSONL remains the authoritative conversation history, including the
original `ImageContent` base64. Pix derives host-local image assets with
content-addressed SHA-256 IDs and atomically writes `source`, `agent`, `vision`,
and `metadata.json` (including original pixel dimensions) under the
configuration directory. A client that opts into
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

The host may keep an ephemeral, content-free `SessionHistoryIndex` for each
discovered JSONL file. It contains only message ordinals, byte anchors, sparse
checkpoints, a committed complete-record fence, and an epoch/fingerprint. It
is rebuilt or discarded on file rewrite/truncation and is never persisted, so
Pi JSONL remains the sole durable source of truth.

The structured `history_items.v1` representation keeps every selected source
index visible: oversized or otherwise unrenderable records become bounded
semantic placeholders instead of hiding the final exchange. The additive
`history_presentation.v1` envelope carries the final user/terminal-assistant
anchors and Turn state so settled process records can be collapsed while an
active Turn remains inspectable. Predictive client-side upward prefetch is a
presentation concern; it does not change the Host's authoritative history.

Model summaries carry Pi's optional `input` modalities. Clients use the
advertised `image` value to gate image composition; the host does not infer
model capability from the existence of an attachment upload.

### Workspace Files boundary

`workspace_files_v1` is a Host-owned, read-only capability for browsing files
inside an already authorized workspace. It is deliberately separate from the
`workspace.list` catalog operation and does not depend on a Pi session. The
wire contract accepts only workspace-relative paths and bounded byte ranges;
the Host revalidates authorization and traverses directory descriptors with
no-follow semantics. Symlink entries may be listed for context, but symlink
traversal and opening are denied. Opaque revisions prevent a client from
concatenating ranges from different file versions. Arbitrary host filesystem
browsing and file mutation remain out of scope.
