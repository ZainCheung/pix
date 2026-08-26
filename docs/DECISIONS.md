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
