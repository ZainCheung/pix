# Pix Host repository instructions

This repository contains the open-source Rust host, content-blind relay, and
the public macOS menu-bar client. The iOS client remains in a separate private
repository and consumes the public `pix-wire` protocol boundary.

- Pi is the only supported agent runtime.
- Pi native JSONL sessions remain the durable source of truth.
- `pix-wire` is the only implementation of encrypted framing and protocol
  validation; do not duplicate it in another language.
- Workspaces must be explicitly authorized on the host.
- Relay payloads are end-to-end encrypted and must never be logged or stored.
- Do not add accounts, cloud message history, a session database, agent
  abstractions, or arbitrary filesystem browsing.
- Never commit secrets, signing material, private keys, or local workspace
  paths.

Before changing the host contract, read `docs/ARCHITECTURE.md`,
`docs/DECISIONS.md`, and `protocol/schema/v1.md`.
