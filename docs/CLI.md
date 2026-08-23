# CLI reference

Run `pix --help` or `pix <command> --help` for the flags shipped by your
version. The global `--config <path>` option (or `PIX_CONFIG`) selects an
explicit configuration file for every command.

Without an override, Pix stores host configuration at
`$HOME/.config/pix/config.json` on macOS and Linux. `pix status` prints the
resolved path; host identity, service state, and logs live in the same Pix
configuration directory.

## Setup and diagnostics

```sh
pix setup
pix doctor
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
```

`pix doctor` checks the local configuration, Pi executable, version, and RPC
capabilities. Use `--pi /path/to/pi` for a one-off probe or `pix pi set` to
persist an explicit executable. Add `--verbose` when support needs local
paths and environment details.

`pix status` prints configuration and host-service state. `pix logs` prints
payload-free host log entries; use `pix service logs` for the same log through
the service subcommand.

## Workspaces

Pix never browses arbitrary paths. Add a canonical folder before a client can
use it:

```sh
pix workspace add "$HOME/Projects/my-project" --name my-project
pix workspace list
pix workspace remove <workspace-id>
```

Full paths are printed only on the host. Removing a workspace revokes client
access to that root; it does not delete files.

## Devices

Pairing requires an interactive terminal because the host asks you to approve
the device's confirmation code:

```sh
pix device pair
pix device list
pix device revoke <device-id>
```

When a relay endpoint is active, `pix device pair` starts a short-lived remote
pairing channel and prints a QR code. Without a relay it waits for a nearby
client discovered over the local network.

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
