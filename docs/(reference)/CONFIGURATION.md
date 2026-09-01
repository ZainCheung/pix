---
title: Configuration
description: Where Pix stores host state and which settings the CLI manages.
---

Pix keeps host state in one per-user JSON configuration. Use the CLI commands
below to change it; Pix validates and writes the file atomically.

## Configuration file

The default file is:

```text
$HOME/.config/pix/config.json
```

The same `.config/pix` layout is used on macOS and Linux. Select another file
for one command, or for the host service, with the global option:

```sh
pix --config /absolute/path/to/pix.json status
```

`PIX_CONFIG` is the equivalent environment variable. A service installed with
one config path is not silently controlled through another path.
When both are supplied, the explicit `--config` option selects the file for
that invocation.

Pix creates the file when setup or another state-changing command first needs
it. A missing file is reported by read-only commands such as `pix status`.

## What the file contains

| Area | Stored information | Supported command |
| --- | --- | --- |
| Host | Host UUID and display name | `pix setup --advanced` |
| Workspaces | Explicitly authorized folder roots and names | `pix workspace add`, `list`, `remove` |
| Devices | Approved device identities and pairing metadata | `pix device pair`, `list`, `revoke` |
| Relay | WebSocket endpoint and enabled/disabled state | `pix relay set`, `show`, `enable`, `disable`, `clear` |
| Pi | Optional selected executable path | `pix pi show`, `set`, `clear` |
| Runtime preferences | Idle timeout and active/concurrent session limits | No dedicated setting command in the current CLI; defaults are persisted in the file |

Workspace paths are stored as absolute, canonical directories. Removing a
workspace removes its Pix authorization; it does not delete the directory or
its files.

## Related host files

The host identity key is kept beside the configuration as a protected local
recovery file. Runtime status, control sockets, event sockets, and host logs
are under the same directory's `run/` and `logs/` subdirectories. These are
host-local implementation state, not additional cloud storage.

The platform service definition embeds the selected config path and starts
`pix serve --service`; installing a service for another path is a separate host
state, not a second view of the current one.

On macOS Pix prefers the Keychain for the identity when it is available; Linux
may use the desktop Secret Service. Pix also keeps the protected local recovery
copy needed by a background service.

## Editing and precedence

The CLI is the supported way to change configuration. It applies one change to
the latest file and restarts or refreshes a running host when necessary. The
current CLI does not expose a setting command for runtime limits, so do not
assume that editing those values or adding arbitrary JSON keys is supported.
Use `--config`/`PIX_CONFIG` to select a separate host state instead of copying
parts of the file.

For environment variables, see [Environment variables](/docs/environment).
For command syntax, see the [CLI reference](/docs/cli).
