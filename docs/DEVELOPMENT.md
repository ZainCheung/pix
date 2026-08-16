# Pix development

This guide covers the local loop for the public Pix Host repository:

~~~text
clone → build → run → inspect → test → package
~~~

For the design rationale, read [ARCHITECTURE.md](ARCHITECTURE.md) first. For
contribution rules, see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Prerequisites

- Rust 1.91 or newer.
- A Pi installation in the verified range `>=0.84.1, <0.85.0`.
- Linux or macOS for host development. Published host packages are Linux-only.
- Node.js and npm when changing the relay.
- A running local relay only when exercising remote transport end to end.

Check the Pi executable and its RPC flags before starting:

~~~bash
pix doctor
~~~

When the executable is not the one found on `PATH`, pin it for the
host:

~~~bash
pix pi set /absolute/path/to/pi
~~~

## Clone and build

~~~bash
git clone https://github.com/ZainCheung/pix.git
cd pix
cargo build --workspace
~~~

A release-mode CLI build is:

~~~bash
cargo build --release -p pix-cli --locked
~~~

The resulting executable is `target/release/pix`.

## Run a local host

Run the CLI directly during development:

~~~bash
cargo run -p pix-cli -- doctor
cargo run -p pix-cli -- serve
~~~

`pix serve` runs in the foreground and accepts `quit` or
`exit` on stdin. Use `--json-events` when a local UI bridge
needs machine-readable JSONL events.

Keep test configuration separate from your normal host state:

~~~bash
cargo run -p pix-cli -- --config /tmp/pix.json doctor
cargo run -p pix-cli -- --config /tmp/pix.json workspace add /tmp/pix-workspace
cargo run -p pix-cli -- --config /tmp/pix.json serve
~~~

The host configuration, status file, control socket, logs, and temporary Pi
context guard are all derived from the selected configuration path.

### Pairing during development

For the product-facing flow, use `pix setup`; it starts a short-lived JSON
event host internally, renders a QR when relay transport is configured, and
maps the confirmation prompt to the pairing request ID internally:

~~~bash
cargo run -p pix-cli -- --config /tmp/pix.json setup
~~~

The focused pairing command follows the same flow:

~~~bash
cargo run -p pix-cli -- --config /tmp/pix.json device pair
~~~

Use `pix serve --json-events` when testing a native UI bridge. Its JSONL
events retain request IDs for automation; the normal `pix serve` presenter
never prints Rust debug structs or raw relay payloads.

## Relay development

The relay is a Cloudflare Worker under `relay/`. Install dependencies
and run its checks from that directory:

~~~bash
cd relay
npm ci
npm test
npm run typecheck
npm run dev
~~~

`npm run deploy` targets a configured Cloudflare account and should
only be used with the credentials and environment described in
[RELEASE.md](RELEASE.md). The relay never receives the channel secret or
application payload.

Relay tests consume `protocol/fixtures/v1/relay-channel.json`. If a
derivation or protocol fixture changes, regenerate the fixture from the Rust
implementation and review the resulting diff:

~~~bash
cargo run -p pix-wire --example generate_fixtures
~~~

## Tests and quality checks

Run the complete local checks before opening a pull request:

~~~bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
(cd relay && npm ci && npm test && npm run typecheck)
~~~

For a focused Rust test, use the package or test name:

~~~bash
cargo test -p pix-wire
cargo test -p pix-core pairing
cargo test -p pix-cli --test e2e_lan
~~~

Tests that need a real Pi are explicitly ignored unless the executable is
available. Do not put real workspace paths, prompts, credentials, private
keys, pairing tokens, or relay secrets in fixtures.

## Debugging and diagnostics

Start with the built-in checks:

~~~bash
pix doctor
pix status
pix logs --tail 100
pix diagnostics export ./diagnostics
~~~

Use an isolated configuration when reproducing a stateful issue. Rust failures
can be made more verbose with `RUST_BACKTRACE=1`:

~~~bash
RUST_BACKTRACE=1 cargo run -p pix-cli -- --config /tmp/pix.json doctor
~~~

Diagnostic bundles redact workspace paths, device public keys, relay URLs,
channel secrets, and Pi executable paths. Review the archive contents before
sharing it.

## Packaging

The release scripts support Linux x86_64 and ARM64. Install the target
toolchains first, then run the reproducible all-in-one helper:

~~~bash
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
SOURCE_DATE_EPOCH=0 packaging/linux/release.sh
~~~

The output is written to `target/release-pkg` by default. To run the
CI-style steps separately:

~~~bash
packaging/linux/build-release.sh x86_64-unknown-linux-gnu dist
packaging/linux/package.sh x86_64-unknown-linux-gnu dist
SOURCE_DATE_EPOCH=0 packaging/release/finalize.sh dist
~~~

The release process, artifact names, version rules, and relay deployment
workflow are documented in [RELEASE.md](RELEASE.md).

## Protocol and Apple boundary

`pix-wire` is the only implementation of encrypted framing and
protocol validation. Keep Rust, protocol schemas, fixtures, and the private
Apple client boundary aligned:

- Update `protocol/schema/v1.md` for a protocol change.
- Regenerate or add fixtures under `protocol/fixtures/v1`.
- Run the Rust and relay tests.
- Do not reimplement crypto or framing in Swift, TypeScript, or another
  language.
- Do not commit signing material or private client files to this repository.

The public Apple build helper is
`packaging/apple/build-pix-wire-xcframework.sh`; the native Apple
clients that consume its output live elsewhere.

## Pull request checklist

Before requesting review:

1. Keep the change scoped to the public host, wire protocol, or content-blind
   relay.
2. Run the formatting, Rust, Clippy, and relay checks above.
3. Update documentation and fixtures when behavior or compatibility changes.
4. Confirm logs and diagnostic output remain payload-free.
5. Read [CONTRIBUTING.md](../CONTRIBUTING.md) and
   [SECURITY.md](../SECURITY.md).
