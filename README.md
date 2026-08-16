<h1 align="center">Pix</h1>

<p align="center">
  Securely access and control your local Pi agent from anywhere.
</p>

<p align="center">
  <a href="#installation">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#development">Development</a> ·
  <a href="docs/ARCHITECTURE.md">Architecture</a>
</p>

<p align="center">
  <a href="https://github.com/ZainCheung/pix/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/ZainCheung/pix/ci.yml?branch=main&label=CI" alt="CI status"></a>
  <a href="https://github.com/ZainCheung/pix/releases"><img src="https://img.shields.io/github/v/release/ZainCheung/pix?display_name=tag" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/ZainCheung/pix" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/rust-1.91%2B-orange?logo=rust" alt="Rust 1.91 or newer">
  <img src="https://img.shields.io/badge/platforms-Linux%20x86__64%20%7C%20ARM64-blue" alt="Linux x86_64 and ARM64">
</p>

## What is Pix?

Pix is an open-source remote host for Pi.
It runs alongside Pi on your computer and lets explicitly authorized clients
discover, connect to, and control Pi sessions without moving your workspaces
to a hosted service.

This repository contains the Pix Host, the `pix` CLI, the
`pix-wire` protocol implementation, protocol fixtures, and the
content-blind relay. The native iOS and macOS clients are maintained in a
separate private repository and use the pinned `pix-wire` release
from this repository.

## Why Pix?

- Use Pi from a phone without exposing Pi directly to the internet.
- Continue working with Pi sessions that remain on your own computer.
- Expose only workspace roots that you explicitly authorize.
- Prefer a direct LAN connection when the client and host are nearby.
- Fall back to an encrypted relay for remote access.
- Keep prompts, files, model output, and session history away from the relay.

## Features

### Host

- Run Pi sessions through the native Pi RPC interface.
- Authorize, list, and revoke workspace roots.
- Pair and revoke trusted client devices.
- Run in the foreground or as a Linux user service.

### Connectivity

- Bonjour discovery and direct TCP on the local network.
- Outbound WebSocket connections to a configurable relay.
- The same encrypted wire frames on both direct and relayed paths.
- Transport loss affects reachability, not the local Pi process.

### Security and operations

- Explicit workspace authorization and paired-device trust.
- Noise-based encrypted channels with framing and replay protection.
- Content-blind relay forwarding of opaque binary frames.
- Payload-free structured logs and privacy-scrubbed diagnostic bundles.
- Health checks, status inspection, and CLI-managed service lifecycle.

## How It Works

~~~mermaid
flowchart LR
    Client["Pix Client"]
    Relay["Pix Relay"]
    Host["Pix Host"]
    Pi["Pi Agent"]
    Workspace["Authorized Workspace"]

    Client <-->|"Direct LAN"| Host
    Client <-->|"Encrypted frames"| Relay
    Relay <-->|"Opaque frames"| Host
    Host <--> Pi
    Pi <--> Workspace
~~~

Pix prefers direct LAN connectivity when it is available. For remote access,
the host opens an outbound connection to a configured relay. The relay
authenticates channel roles and forwards opaque encrypted frames; it cannot
read application payloads. Both paths terminate at the same Pix Host and Pi
session.

The implementation boundary is:

~~~text
pix-cli  →  pix-core  →  pix-wire
~~~

See [the architecture guide](docs/ARCHITECTURE.md) for the full set of
invariants and [the protocol schema](protocol/schema/v1.md) for the versioned
wire contract.

## Requirements

For a packaged host:

- Linux x86_64 or ARM64.
- Pi installed and available on `PATH`, or an explicit executable
  path.
- A Pi version in the currently verified range `>=0.84.1, <0.85.0`.

For a source build:

- Rust 1.91 or newer.
- Pi with the RPC capabilities required by Pix (`--mode`,
  `--approve`, `--session`, and `--session-id`).
- Node.js and npm only when developing the relay.

The Rust workspace includes macOS host support for source builds, but this
repository's public host packages are currently Linux-only. The Apple clients
and their distribution artifacts are maintained separately.

## Installation

### Linux release packages

Download a release from [GitHub Releases](https://github.com/ZainCheung/pix/releases).
The release workflow publishes tarballs for Linux x86_64 and ARM64, plus
native `.deb` and `.rpm` packages when the corresponding
packaging tool is available.

Debian or Ubuntu:

~~~bash
sudo dpkg -i pix_<version>_amd64.deb   # x86_64
sudo dpkg -i pix_<version>_arm64.deb   # ARM64
~~~

Fedora, RHEL, or another RPM-based distribution:

~~~bash
sudo rpm -i pix-<version>-1.x86_64.rpm   # x86_64
sudo rpm -i pix-<version>-1.aarch64.rpm  # ARM64
~~~

Binary archive:

~~~bash
tar -xzf pix-<version>-<target>.tar.gz
mkdir -p "$HOME/.local/bin"
cp pix-<version>-<target>/bin/pix "$HOME/.local/bin/pix"
~~~

Make sure `$HOME/.local/bin` is on `PATH` before running
the commands below.

### Build from source

~~~bash
git clone https://github.com/ZainCheung/pix.git
cd pix
cargo build --release -p pix-cli --locked
mkdir -p "$HOME/.local/bin"
cp target/release/pix "$HOME/.local/bin/pix"
~~~

You can also run the CLI without installing it:

~~~bash
cargo run -p pix-cli -- doctor
~~~

## Quick Start

The following flow takes a fresh host from environment checks to a running
Pix host. Replace the workspace path with a directory you intend to expose
to Pi.

~~~bash
# 1. Check the host environment and Pi RPC compatibility.
pix doctor

# 2. Optional: pin Pi when it is not the executable you want from PATH.
pix pi set /path/to/pi

# 3. Authorize a workspace root explicitly.
pix workspace add "$HOME/Projects/my-project"
pix workspace list

# 4. Pair the first client. This starts a foreground host.
pix device pair
~~~

When a client sends a pairing request, enter `approve <request-id>`
in the `pix device pair` process. Type `quit` when pairing
is complete, then start the host normally:

~~~bash
pix serve
pix status
~~~

`pix device pair` starts its own foreground host and must not be run
while another `pix serve` process is already running. After a device
is paired, use `pix serve` directly or
[install the Linux user service](#running-as-a-service).

## Usage

Run `pix --help` or `pix <command> --help` for the
complete CLI reference.

| Task | Command |
| --- | --- |
| Check environment | `pix doctor` |
| Check host status | `pix status` |
| Start the host | `pix serve` |
| Authorize a workspace | `pix workspace add <path>` |
| List workspaces | `pix workspace list` |
| Remove a workspace | `pix workspace remove <id>` |
| Pair a device | `pix device pair` |
| List paired devices | `pix device list` |
| Revoke a device | `pix device revoke <id>` |
| Inspect Pi selection | `pix pi show` |
| Set or clear Pi | `pix pi set <path>` / `pix pi clear` |
| Inspect relay configuration | `pix relay show` |
| Configure a relay | `pix relay set <url>` |
| Enable or disable relay transport | `pix relay enable` / `pix relay disable` |
| Show recent logs | `pix logs --tail 50` |
| Export diagnostics | `pix diagnostics export <path>` |

### Workspaces

Pix does not browse arbitrary paths. Every workspace must be explicitly
authorized before a client can use it:

~~~bash
pix workspace add "$HOME/Projects/my-project" --name my-project
pix workspace list
pix workspace remove <workspace-id>
~~~

### Devices

Pairing is an explicit approval step on the host. Once paired, a device can
connect over LAN or through the configured relay until it is revoked:

~~~bash
pix device list
pix device revoke <device-id>
~~~

### Pi

Pix discovers `pi` through the host environment. Pin a known
executable when you use a version manager or multiple Pi installations:

~~~bash
pix pi set /path/to/pi
pix pi show
pix pi clear
~~~

### Relay

Configure the WebSocket endpoint supplied by your relay deployment. Setting a
URL enables relay transport; it can be disabled later without removing the
stored endpoint:

~~~bash
pix relay set wss://relay.example.com
pix relay show
pix relay disable
pix relay enable
pix relay clear
~~~

For remote pairing, run `pix device pair` (or `pix serve`)
after configuring a relay and enter `pair-remote` at its interactive
prompt. Pix prints a short-lived QR payload and join code for the client.
Treat both as secrets while they are valid.

### Running as a service

On Linux, Pix can install a user-level systemd unit. It does not require root
privileges:

~~~bash
pix service install       # enable and start pix.service
pix status
pix service stop
pix service uninstall
~~~

Use `pix service install --no-start` when you want to enable the unit
without starting it immediately. The service stores its lifecycle state under
the Pix configuration directory, so `pix status` remains the first
place to check.

### Diagnostics

Logs contain lifecycle and operational metadata, never prompts, files, model
output, keys, tokens, or relay secrets:

~~~bash
pix logs --tail 100
pix diagnostics export ./diagnostics
~~~

The export is a privacy-scrubbed `.tar.gz` bundle suitable for
sharing with a maintainer. See
[Troubleshooting](docs/TROUBLESHOOTING.md) for what to collect before reporting
a problem.

## Remote Access

Local access uses Bonjour discovery and a direct TCP connection:

~~~text
Pix Client  ───────────────  Pix Host
              Direct LAN
~~~

Remote access uses an outbound host connection and the content-blind relay:

~~~text
Pix Client  ─── encrypted ─── Pix Relay  ─── encrypted ─── Pix Host
~~~

The relay forwards authenticated opaque binary frames and does not terminate
the Noise channel. It does not parse, queue, persist, or replay application
messages. See [relay/README.md](relay/README.md) for the relay contract and
local development instructions.

## Security & Privacy

- Only explicitly authorized canonical workspace roots are usable.
- Devices must be explicitly paired before they can access the host.
- Direct and relayed transports use the `pix-wire` encrypted channel.
- The relay cannot read prompts, files, model output, or session content.
- Pi's native JSONL session remains the durable conversation source of truth;
  Pix does not maintain a second conversation database.
- Host logs are payload-free, and diagnostic exports redact sensitive paths,
  keys, relay URLs, and executable locations.

Read [SECURITY.md](SECURITY.md) before reporting a vulnerability and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the detailed security
invariants.

## Development

### Prerequisites

- Rust 1.91 or newer.
- A supported Pi installation.
- Node.js and npm for relay work.

### Clone, build, and run

~~~bash
git clone https://github.com/ZainCheung/pix.git
cd pix
cargo build --workspace
cargo run -p pix-cli -- doctor
cargo run -p pix-cli -- serve
~~~

Use a temporary configuration while experimenting:

~~~bash
cargo run -p pix-cli -- --config /tmp/pix.json doctor
cargo run -p pix-cli -- --config /tmp/pix.json serve
~~~

### Relay development

~~~bash
cd relay
npm ci
npm test
npm run typecheck
npm run dev
~~~

The relay tests consume the versioned fixtures under
`protocol/fixtures/v1`. Deploying a Worker requires the credentials
described in [the release guide](docs/RELEASE.md).

### Testing

Run the same checks required before opening a pull request:

~~~bash
cargo fmt --all -- --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
(cd relay && npm ci && npm test && npm run typecheck)
~~~

The longer development loop, packaging commands, and debugging notes live in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Project Structure

~~~text
pix/
├── crates/
│   ├── pix-cli/       # CLI and host process
│   ├── pix-core/      # Host control plane and Pi lifecycle
│   └── pix-wire/      # Protocol, crypto, framing, and UniFFI boundary
├── protocol/          # Versioned schemas and cross-language fixtures
├── relay/             # Content-blind Cloudflare Worker relay
├── packaging/         # Linux packages and Apple XCFramework helper
├── scripts/            # Release and development scripts
├── docs/              # Architecture, development, and release docs
└── .github/           # CI and release workflows
~~~

## Architecture

The host control plane is intentionally split into three public Rust crates:

- `pix-cli` owns the command-line entry point and host lifecycle
  commands.
- `pix-core` owns workspace boundaries, pairing, discovery,
  transports, Pi processes, and session ownership.
- `pix-wire` is the only implementation of protocol validation,
  framing, encryption, replay protection, and the Apple UniFFI boundary.

Do not duplicate wire or cryptographic logic in another language. For detailed
invariants, compatibility rules, and the private Apple boundary, read
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/DECISIONS.md](docs/DECISIONS.md).

## Documentation

| Document | Description |
| --- | --- |
| [Architecture](docs/ARCHITECTURE.md) | Host modules, transports, and security invariants |
| [Development](docs/DEVELOPMENT.md) | Clone, build, debug, test, relay, and package Pix |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | Common setup, pairing, relay, service, and diagnostic issues |
| [Protocol schema](protocol/schema/v1.md) | Versioned Pix wire contract |
| [Relay](relay/README.md) | Relay contract and Worker development |
| [Repository boundary](docs/REPOSITORY.md) | Public/private repository responsibilities |
| [Decisions](docs/DECISIONS.md) | Durable architecture and product decisions |
| [Release](docs/RELEASE.md) | Versioning, packaging, and deployment workflow |
| [Contributing](CONTRIBUTING.md) | Pull request and protocol change expectations |
| [Security](SECURITY.md) | Vulnerability reporting and security scope |

## Contributing

Contributions are welcome within the public host, wire protocol, and
content-blind relay boundaries.

Before opening a pull request:

1. Read [CONTRIBUTING.md](CONTRIBUTING.md).
2. Run the [quality checks](#testing).
3. Update versioned protocol schema and fixtures for wire changes.
4. Keep prompts, files, model output, credentials, and channel secrets out of
   logs, tests, and commits.

## Releases

Stable builds are published through [GitHub Releases](https://github.com/ZainCheung/pix/releases).
Pix uses the workspace version in `Cargo.toml` and matching
`vX.Y.Z` tags. Linux release artifacts currently target x86_64 and
ARM64; the workflow also builds the public `pix-wire` Apple
XCFramework consumed by the private clients.

See [docs/RELEASE.md](docs/RELEASE.md) for the maintainer workflow. Do not
deploy the relay or publish a release from a local checkout containing secrets.

## Project Status

> [!IMPORTANT]
> Pix is under active development. The host APIs and wire protocol may change
> before v1.0.

The public repository currently contains the Pix Host, encrypted LAN and relay
transports, device pairing, diagnostics, and Linux packaging. Native Apple
clients remain in a separate private repository, and no public macOS host
installer is published here.

## Troubleshooting

| Symptom | First checks |
| --- | --- |
| Pi is not found or incompatible | Run `pix doctor`; use `pix pi set <path>` or `pix doctor --pi <path>` and verify the supported Pi range. |
| A workspace cannot be opened | Run `pix workspace list`; authorize the canonical project root with `pix workspace add <path>`. |
| A device cannot discover the host | Keep `pix device pair` or `pix serve` running, verify the client and host share a LAN, then inspect `pix status`. |
| Remote pairing or relay fails | Run `pix relay show`, confirm a `ws://`/`wss://` endpoint, and inspect `pix logs --tail 100`. |
| The background service is not running | Run `pix status`; on Linux inspect `systemctl --user status pix.service` and use `pix service install` if needed. |
| You need to share diagnostics | Run `pix diagnostics export ./diagnostics` and share the resulting archive, not raw config or logs. |

For the longer checklist, see [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).

## FAQ

### Does Pix upload my Pi conversations?

No. Pi's native JSONL session stays on the host computer. Pix does not create
its own conversation database.

### Can the relay read my messages?

No. The relay forwards authenticated encrypted frames and is deliberately
content-blind. It can observe only limited transport metadata needed for
routing and rate limits.

### Does Pix work without a relay?

Yes. When the client and host are on the same network, Pix can use Bonjour and
direct TCP. A relay is only needed for the remote path.

### Can I pair more than one device?

Yes. Each device must be explicitly approved and can be listed or revoked
individually with the `pix device` commands.

### Does Pix require the Apple client?

No. The host and CLI are usable without it, and the wire protocol is documented
for other authorized clients. The native Apple clients are maintained
separately.

### Where does Pix store its configuration?

Pix uses a platform configuration directory and prints the resolved path in
`pix status` and `pix doctor`. Use the global
`--config <path>` option to select an explicit configuration file.

### Does Pix replace Pi?

No. Pix launches and supervises the installed Pi executable, adapts its RPC
interface, and keeps Pi's native session storage authoritative.

## License

Pix Host is available under the [MIT License](LICENSE). Third-party
dependencies retain their own licenses; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Acknowledgements

Pix builds on the Rust ecosystem, Pi, Noise, UniFFI, and Cloudflare Workers.
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the dependency and
license notices shipped with releases.
