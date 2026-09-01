---
title: Diagnostics
description: Inspect Pix status and logs, then export a bundle you can review before sharing.
---

Use these commands when a symptom in [Troubleshooting](/docs/troubleshooting)
needs more evidence. They describe the host's current state; they do not
upload anything.

## `pix status`

```sh
pix status
```

Status reads the selected configuration, probes the Pi executable and its
version, and checks the host-service state. It also reports the active
connection mode and counts of configured workspaces and devices. Use
`--output json` when a script needs the versioned machine-readable envelope.

## `pix service status`

```sh
pix service status
```

This adds service-manager details: whether the per-user service is installed
and active, which executable owns it, and the host process details when it is
running. On supported Unix hosts it may show the local control and event socket
paths; those paths are local diagnostics, not network endpoints.

## `pix logs`

```sh
pix logs --tail 100
```

The host log is a JSONL file in the Pix configuration directory. The normal
log view contains lifecycle and connection metadata such as event type, state,
byte counts, and close information. It is designed not to record Pi prompts,
model output, repository contents, credentials, or relay payloads. Metadata
can still identify timing and connection activity, so treat it as private.

`pix service logs --tail N` reads the same host log through the service command.

## `pix diagnostics export`

Create a bundle for a maintainer:

```sh
pix diagnostics export ./diagnostics
```

Pix writes a new `pix-diagnostics-<timestamp>.tar.gz` file in that directory.
You can also provide a destination ending in `.tar.gz` or `.tgz`; existing files
are never overwritten. The archive contains:

- `summary.txt` with Pix version, operating system, configuration presence,
  workspace/device counts, and relay-enabled state;
- `config.redacted.json`;
- `service-status.txt`;
- `pi-status.txt` with Pi version/support status but not its executable path;
- a sanitized `logs/host.jsonl` file.

The exporter replaces workspace paths, device keys, relay URLs and channels,
Pi executable paths, credentials, tokens, and private-key fields with
`[redacted]`. The host private-key file is not included. Log entries are
allow-listed and pairing QR payloads and join codes are removed.

## Review before sharing

The bundle is scrubbed by Pix, but it is still a file from your computer. Open
the archive, check every entry, and remove unrelated notes or files before
sharing it. If the archive contains no useful evidence, share the relevant
command output instead and keep paths, codes, prompts, and session content
private.

For the full command syntax, see the [CLI reference](/docs/cli).
