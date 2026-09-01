---
title: Service management
description: Install, inspect, restart, repair, and remove the Pix host service.
---

Pix can run as a per-user background service. Use this page when you need to
control that service directly; the normal [Installation](/docs/installation)
flow installs it for you.

## The service on each host

- Linux uses a `systemd --user` unit at
  `$XDG_CONFIG_HOME/systemd/user/pix.service`, or
  `~/.config/systemd/user/pix.service` when `XDG_CONFIG_HOME` is unset.
- macOS uses a per-user LaunchAgent in `~/Library/LaunchAgents`.
- Neither service command requires root. The service runs as the user who
  installed it and uses that user's Pix configuration.

The service runs `pix serve --service`. It is the long-lived host process that
the CLI and the macOS app connect to.

## Install and inspect

```sh
pix service install
pix service status
```

`install` enables the user service and starts it unless `--no-start` is
provided. Use `--no-start` when you want to inspect the installed definition
before starting it:

```sh
pix service install --no-start
pix service start
```

`status` reports the service manager, whether the unit is installed and active,
the owning executable, and the host process when it is running.

## Start, stop, and restart

```sh
pix service start
pix service stop
pix service restart
```

Restart after changing relay or Pi executable settings while the host is
running. `stop` leaves the service installed so it can be started again.

## Service ownership

Pix records which executable installed the service. This prevents a Homebrew,
app-bundled, or source-built CLI from silently replacing another installation.
If you intentionally want the current CLI to take ownership, say so with:

```sh
pix service install --adopt
```

`--adopt` is an explicit ownership transfer. It is not needed when the same
executable already owns the service, and it does not copy or delete your Pi
sessions.

## Repair the host identity

If a service cannot authorize the host identity after an account or keychain
change, run this in a terminal:

```sh
pix service repair-identity
```

The command authorizes the existing host identity and refreshes its protected
local recovery copy. It is intentionally human-facing and does not support
JSON output.

## Logs and removal

Inspect recent service logs without stopping the host:

```sh
pix service logs --tail 100
```

Remove the service and disable it at login/boot with:

```sh
pix service uninstall
```

Uninstall removes the platform service definition and Pix's service-owner
record. It preserves the Pix configuration, host identity, authorized
workspaces, repository files, and native Pi session files. Remove those data
separately only when you intend to delete the host state.

For alternate package, archive, source-build, and removal paths, see
[Installation details](/docs/installation-details). For the complete command
syntax, see the [CLI reference](/docs/cli).
