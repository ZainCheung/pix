---
title: Compatibility
description: Pi, protocol, and client compatibility requirements for the current Pix release.
---

Compatibility is about software versions and capabilities. For operating
systems and released artifacts, see [Platform support](/docs/platform-support).

## Pi version and startup interface

The current Pix host verifies this Pi range:

```text
>=0.84.1, <0.85.0
```

During the probe Pix runs `pi --version` and checks that Pi advertises the RPC
options the adapter uses:

```text
--mode <mode>
--approve
--session <path|id>
--session-id <id>
```

`pix status` reports the detected version and whether it is supported. If more
than one Pi is installed, use `pix pi set /absolute/path/to/pi` and restart the
host service.

## Pix wire protocol

The public wire schema has protocol major version `1`. Clients and hosts
negotiate additive capabilities on each connection. An older host ignores an
unknown capability declaration and falls back to the base v1 feature set; it
does not receive fields gated by capabilities it did not negotiate.

The negotiated attachment capabilities currently distinguish the legacy
four-image ceiling (`attachments.v1`) from the nine-image ceiling
(`attachments.v2`). Each image is still bounded to 4 MiB. See the
[Pi RPC coverage matrix](/docs/pi-rpc-coverage) for operation mapping rather
than duplicating that table here.

## Pi TUI bridge

The optional `@zaincheung/pix` package uses Pi's extension API and the local
bridge socket. Its package manifest declares a peer dependency on
`@earendil-works/pi-coding-agent` and does not pin a separate Pi semver range.
Install the package with Pi's package command, then reload or restart Pi. The
host and extension must both be running on a supported Unix host.

When an exact compatibility check fails, start with `pix status`, inspect
[Diagnostics](/docs/diagnostics), and then compare the implementation details
in [Architecture](/docs/architecture).
