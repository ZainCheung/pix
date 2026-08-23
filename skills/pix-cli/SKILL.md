---
name: pix-cli
description: Operate the Pix host (remote access for the Pi agent runtime) through its headless CLI. Use for managing Pix status, doctor checks, authorized workspaces, paired devices, pairing approvals, remote pairing offers, active Pi sessions, the relay endpoint, the Pi executable, the background host service, logs, or diagnostic exports from scripts and agents. Also use when a user mentions pix, pix://pair QR codes, or controlling a Pix host.
---

# Pix CLI for agents

Pix exposes one binary with two modes. Humans get menus; agents get a
stable, versioned JSON contract. Never parse human-oriented output.

## Headless contract

Prefix every invocation with the global mode flags:

```bash
pix --output json --no-input [--config PATH] <command> [args]
```

- `--output json` — machine-readable envelope on stdout. JSON mode never
  prompts and never opens an interactive menu.
- `--no-input` — commands that would otherwise ask for a selection fail
  with a `usage` error instead; pass the required ID explicitly.
- `--config PATH` — override the config file (env `PIX_CONFIG`). Useful for
  tests and isolated hosts. Mutating service commands refuse to operate on
  units installed from a different config path.
- Interactive-only convenience: a bare `pix` in a TTY opens a home screen.
  In headless mode it prints help and exits 0.

Envelope (success on stdout, errors on stderr):

```json
{"schema_version": 1, "ok": true,  "command": "workspace.list", "data": { ... }}
{"schema_version": 1, "ok": false, "error": {"code": "usage", "message": "..."}}
```

Exit codes: `0` success, `2` usage error (bad arguments, missing required
ID), `1` command failure (missing config, unreachable service, unknown ID).
Error codes seen in practice: `usage`, `command_failed`.

Check `schema_version` before reading `data`; treat a future version as a
reason to re-inspect shapes rather than guess.

## Command inventory

Read-only first. Most commands are safe; mutations are marked.

### Host overview

| Command | JSON `command` | Notes |
| --- | --- | --- |
| `pix status` | `status` | Config state, resolved Pi executable/version/support, service state, relay mode, device/workspace counts. Does not create config. |
| `pix logs [--tail N]` | `logs` | Recent host log entries (payload-free). |

`status.data` example:

```json
{"config_state": "ready", "host": "Zain's Mac",
 "pi": {"source": "path", "executable": "/opt/homebrew/bin/pi",
        "version": "1.2.3", "supported": true},
 "service": {"state": "running", "installed": true},
 "access": {"mode": "local", "relay_enabled": false},
 "devices": 2, "workspaces": 3}
```

`pi.version` is absent when no Pi executable resolves; treat that as a
setup gap, not an empty value.

`config_state` is one of `missing`, `ready`, or a broken state that
`pix doctor` explains.

### Devices and pairing

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix device list` | `device.list` | no |
| `pix device pending` | `device.pending` | no — needs the host service running |
| `pix device pair` | `device.pair` | yes — starts the service and a LAN offer |
| `pix device pair --remote` | `device.pair` | yes — relay offer; returns `qr_payload`, `join_code`, `expires_at` |
| `pix device approve --request ID` or `--code 123456` | `device.approve` | yes |
| `pix device reject --request ID` or `--code 123456` | `device.reject` | yes |
| `pix device revoke <ID>` | `device.revoke` | yes — closes live connections when the service runs |

Rules: approve/reject take exactly one of `--request` (UUID from
`device pending`) or `--code` (six digits shown on the phone). Device
public keys and relay channel secrets are never printed in any mode.

Remote pairing flow: `device pair --remote` presents an offer; a repeated
RPC call replays the same unused offer (idempotent) or errors with
`conflict` while a phone is mid-pairing. A deliberate new offer from an
interactive `pix serve` stdin command replaces the old channel.

### Workspaces (explicitly authorized folders)

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix workspace add <PATH> [--name NAME]` | `workspace.add` | yes |
| `pix workspace list` | `workspace.list` | no |
| `pix workspace sessions <ID>` | `workspace.sessions` | no — native Pi sessions stored in one workspace |
| `pix workspace remove <ID>` | `workspace.remove` | yes — refreshes the running service and evicts sessions in that folder |

Full paths appear only in `workspace.*` output; treat them as host-local
and never publish them.

### Sessions (active Pi runtimes)

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix session list` | `session.list` | no — needs the host service |
| `pix session release <ID>` | `session.release` | yes |

### Pi executable

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix pi show` | `pi.show` | no |
| `pix pi set <PATH>` | `pi.set` | yes |
| `pix pi clear` | `pi.clear` | yes |

### Relay (encrypted remote transport)

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix relay show` | `relay.show` | no |
| `pix relay set <wss://…>` | `relay.set` | yes |
| `pix relay clear` | `relay.clear` | yes |
| `pix relay enable` / `pix relay disable` | `relay.enable` / `relay.disable` | yes |

### Service (background host process)

| Command | JSON `command` | Mutation |
| --- | --- | --- |
| `pix service install [--no-start]` | `service.install` | yes |
| `pix service uninstall` | `service.uninstall` | yes |
| `pix service start` / `stop` / `restart` | `service.start` / `service.stop` / `service.restart` | yes |
| `pix service status` | `service.status` | no |
| `pix service logs [--tail N]` | `service.logs` | no |

`session.*`, `device.pending`, and control-dependent commands require the
host service; failures say `run pix service start first`.

### Diagnostics

`pix diagnostics export <PATH.tar.gz>` (`diagnostics.export`) writes a
privacy-scrubbed bundle. Mutation-free but writes a file.

## Canonical agent workflows

Headless first-time setup:

```bash
pix --output json --no-input status                                   # inspect
pix --output json --no-input relay set wss://relay.example.com        # optional remote access
pix --output json --no-input workspace add ~/code/project --name Project
pix --output json --no-input service install
```

Approve a phone that is pairing (user reads the six-digit code aloud or
from the phone):

```bash
pix --output json --no-input device pair --remote    # offer with QR + join code
pix --output json --no-input device pending          # requests awaiting approval
pix --output json --no-input device approve --code 123456
```

Revoke a lost phone:

```bash
pix --output json --no-input device list
pix --output json --no-input device revoke <device-id>
```

## Safety rules

- Always pass `--output json --no-input` in scripts; a bare group command
  (e.g. `pix device`) without them is a `usage` error, never a hidden menu.
- Never read business data from human-mode text; only the JSON envelope is
  a contract. Human formatting changes without notice.
- Never log or persist `qr_payload`, join codes, or anything from inside
  `data` beyond what the task needs; they are short-lived secrets.
- Prefer read commands (`status`, `list`, `show`, `pending`) before
  mutating; mutations return the resulting state in `data`.
- If `schema_version` is not `1`, stop and re-read the current CLI docs
  (`docs/CLI.md` in the pix repository) instead of guessing fields.
