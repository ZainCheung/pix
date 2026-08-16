# Pix Host

Pix Host is the open-source Rust host for Pix. It runs the installed Pi
executable for explicitly authorized workspaces, exposes the native Pi session
through an authenticated protocol, and provides the `pix` CLI for host
operation.

The Apple clients are maintained in a separate private repository. They build
against a pinned release of this repository's `pix-wire` crate and protocol
fixtures. The host remains useful without those clients through the CLI and
the documented wire protocol.

## Repository layout

```text
crates/pix-wire/     Shared protocol, Noise channel, and UniFFI boundary
crates/pix-core/     Rust host control plane and Pi RPC lifecycle
crates/pix-cli/      `pix` command-line host entry point
protocol/            Versioned schema and cross-language fixtures
relay/               Content-blind Cloudflare Worker relay
packaging/linux/     Linux service and package scripts
packaging/apple/     Public pix-wire XCFramework build helper
docs/                Host architecture and compatibility contract
```

## Development

```bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cd relay && npm ci && npm test && npm run typecheck
```

Release automation and local packaging conventions are documented in
[docs/RELEASE.md](docs/RELEASE.md).

The Apple repository can point `PIX_HOST_CHECKOUT` at a local checkout while
developing the client against an unreleased host commit.

## License

Pix Host is available under the MIT License. See [LICENSE](LICENSE). Third-party
dependencies retain their own licenses; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
