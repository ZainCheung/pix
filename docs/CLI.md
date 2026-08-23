# CLI reference

Run `pix --help` or `pix <command> --help` for the flags shipped by your
version. The global `--config <path>` option (or `PIX_CONFIG`) selects an
explicit configuration file for every command.

Without an override, Pix stores host configuration at
`$HOME/.config/pix/config.json` on macOS and Linux. `pix status` prints the
resolved path; host identity, service state, and logs live in the same Pix
configuration directory.

## Two modes: interactive and headless

The same binary serves humans in a terminal and scripts or agents on a
machine contract.

Interactive mode is the default in a TTY:

- A bare `pix` opens the home screen: a read-only host snapshot on top and
  an action menu below. It never creates or modifies configuration.
- A group command without an action (for example `pix device` or
  `pix workspace`) opens that group's menu. Actions that remove trust —
  revoking a device, removing a workspace — ask for confirmation first.

Headless mode is opt-in with two global flags:

```sh
pix --output json --no-input status
```

- `--output human|json` (or `PIX_OUTPUT`) selects the format. JSON mode
  never prompts and never opens a menu.
- `--no-input` makes any command that would otherwise wait for a selection
  fail with a usage error instead; pass the required ID explicitly.

Outside a TTY, a bare `pix` prints the standard help text and exits 0, so
pipelines and schedulers never block on a menu.

### The JSON envelope

Every JSON-mode command prints one object. Success goes to stdout, errors
to stderr:

```json
{"schema_version": 1, "ok": true, "command": "status", "data": { ... }}
{"schema_version": 1, "ok": false, "error": {"code": "usage", "message": "..."}}
```

Exit codes: `0` on success, `2` for usage errors (missing arguments or a
required ID), `1` for command failures. Human-readable text is not a
contract; parse only this envelope. An agent-facing skill with the full
command inventory lives in `skills/pix-cli/SKILL.md`.

## Setup and diagnostics

```sh
pix setup
pix status
pix logs --tail 50
```

`pix setup` is the product-facing first-use flow. It checks Pi, authorizes a
workspace, offers LAN or relay access, guides pairing, and installs the
per-user host service. Useful setup options include:

```sh
pix setup --workspace "$HOME/Projects/my-project"
pix setup --relay wss://relay.example.com
pix setup --no-pair --no-service --non-interactive \
  --workspace "$HOME/Projects/my-project"
pix setup --advanced
```

Interactive setup goes straight to the recommended path; `--advanced`
exposes host name, Pi selection, connectivity, workspaces, and the
service question with a review step. Pairing is optional during setup
(`pix device pair` works any time), and abandoning the wizard before it
commits leaves no config file behind.

`pix status` prints configuration, host-service state, and the resolved Pi
executable with its version. `pix logs` prints payload-free host log entries;
use `pix service logs` for the same log through the service subcommand.

`pix update` upgrades the running executable (and the macOS app bundle) from
the repository's latest GitHub release, mirroring the first-party installer.
On a configured host, `pix setup` runs a health verification directly; relay
settings live in `pix relay` (the home screen's Settings entry).

## Workspaces

Pix never browses arbitrary paths. Add a canonical folder before a client can
use it:

```sh
pix workspace add "$HOME/Projects/my-project" --name my-project
pix workspace list
pix workspace sessions <workspace-id>
pix workspace remove <workspace-id>
```

Full paths are printed only on the host. Removing a workspace revokes client
access to that root; it does not delete files.

## Devices

Pairing shows a confirmation code that a human should check against the
phone. In a terminal, `pix device pair` walks you through it; headless
callers split the flow into offer, review, and decision:

```sh
pix device pair                 # interactive: offer plus approval prompts
pix device pair --remote        # interactive: relay offer with a QR code
pix device list
pix device pending              # requests waiting for approval
pix device approve --code 123456
pix device reject --request <request-id>
pix device revoke <device-id>
```

`approve` and `reject` accept exactly one of `--request` (the stable ID
from `pix device pending`) or `--code` (the six digits shown on the
phone). Revoking a device while the host service runs also closes its
live connections.

When a relay endpoint is active, `pix device pair --remote` starts a
short-lived remote pairing channel and prints a QR code. Without a relay
the host waits for a nearby client discovered over the local network.

## Sessions

The host service owns the Pi runtimes it starts. Inspect and release them
without stopping the service:

```sh
pix session list
pix session release <session-id>
```

Releasing a runtime lets another Pi process resume that session file.

## Pi selection

Pi is discovered from the host environment by default:

```sh
pix pi show
pix pi set /absolute/path/to/pi
pix pi clear
```

Use `pix pi set` when a version manager or multiple installations make the
desired executable different from the one found on `PATH`.

## Relay

Relay configuration accepts `ws://` or `wss://` endpoints:

```sh
pix relay set wss://relay.example.com
pix relay show
pix relay disable
pix relay enable
pix relay clear
```

Setting a URL enables relay transport. `disable` keeps the endpoint but stops
using it; `clear` removes the stored endpoint.

## Services

Pix uses one persistent host service so the menu-bar app and CLI share pairing,
Bonjour, and transport state:

```sh
pix service install
pix service install --no-start
pix service start
pix service status
pix service restart
pix service stop
pix service logs --tail 100
pix service uninstall
```

The service is per-user: systemd user units on Linux and LaunchAgents on macOS.
`pix serve` remains available for a foreground host during development or
automation:

```sh
pix serve
pix serve --json-events
```

The JSON event stream is the native UI and automation bridge. It contains
payload-free lifecycle and session events, not relay secrets or Pi messages.

## Diagnostics

Create a privacy-scrubbed bundle when reporting a problem:

```sh
pix diagnostics export ./diagnostics
```

Review the archive before sharing it. Logs and diagnostic bundles redact
prompts, files, model output, credentials, private keys, pairing tokens,
workspace paths, and relay secrets.
