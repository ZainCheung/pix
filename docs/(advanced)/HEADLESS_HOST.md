---
title: Headless hosts
description: Set up a Pix host over SSH on a Linux machine without a desktop GUI.
---

Pix's CLI and host service can be prepared from an SSH terminal. This workflow
focuses on a Linux host you normally reach over SSH; it does not add Windows
support or replace the optional macOS app.

## Prerequisites

You need:

- SSH access to the Linux user account that will run Pi and Pix;
- Pi installed and available to that account;
- an existing workspace directory on the host;
- a terminal with a TTY for the pairing step; and
- a relay endpoint if the phone will be outside the host's local network.

## Prepare the host

SSH into the account and install Pix using the normal first-party installer:

```sh
ssh user@example-host
curl -fsSL https://pix.deepoke.com/install.sh | sh
```

If `~/.local/bin` is not already on `PATH`, add it before continuing. The
non-interactive setup path authorizes one existing workspace, checks Pi, and
installs the per-user service without waiting for a phone:

```sh
pix setup --non-interactive --no-pair \
  --workspace "$HOME/Projects/my-project"
```

`--workspace` must point to a directory that already exists. The command writes
the same host configuration used by an interactive setup.

## Configure access away from home

Set the hosted relay, or substitute an endpoint you operate:

```sh
pix relay set wss://pix-relay.deepoke.com
pix relay show
pix service restart
```

The relay changes how the phone reaches the host; Pi and the workspace remain
on this machine. For a local-network-only host, omit the relay commands and
pair while the phone and host share a network.

## Pair from the SSH terminal

With a relay configured, start remote pairing in the SSH TTY:

```sh
pix device pair --remote
```

The terminal prints a QR offer and a short join code. Scan the QR code in Pix,
compare the six-digit confirmation code shown by both devices, and approve the
request when the CLI asks. The offer is short-lived; rerun the command if it
expires. `pix device pending` shows requests waiting for approval.

For a same-network pairing, run `pix device pair` instead and keep local
discovery available between the phone and host.

## Check restart persistence

The service is per-user and is enabled in that user's service manager. Pix does
not change the host's login or session policy, so verify the service after a
reboot on a machine that does not keep the user manager running:

```sh
pix service status
pix service restart
pix status
```

On Linux the service is a `systemd --user` unit. If it is not installed, run
`pix service install`; do not use `sudo`. After a host restart, reconnect the
phone and check `pix status` before opening a session.

If a step fails, collect [Diagnostics](/docs/diagnostics) and use the
symptom-oriented [Troubleshooting](/docs/troubleshooting) page. Service
ownership and removal are covered in [Service management](/docs/services).
