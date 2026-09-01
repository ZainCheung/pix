---
title: Pix and Pi
description: See what Pix adds around Pi and which responsibilities stay with Pi.
---

Pi is the agent. Pix is a remote client and host bridge for that agent. Pix
does not replace the `pi` executable or move Pi's work to a server.

| Capability | Pi | Pix |
| --- | --- | --- |
| Runs the coding agent | Yes | No |
| Runs local tools in the repository | Yes | No |
| Owns native Pi session history | Yes, in Pi's native session files | No second message database |
| Mobile remote client | No | Yes |
| Device pairing and revocation | No | Yes |
| Direct and relay transport | No | Yes |
| Workspace authorization for remote access | No | Yes, in Pix Host |

## What Pix adds

Pix Host runs on the same computer as Pi. It authorizes workspace roots,
accepts paired-device connections, and forwards session actions to Pi. The
phone can list workspaces and native sessions, open or create a session, send
prompts, and receive Pi's updates.

## What stays Pi's

Pi reads and changes your repository, runs its tools, talks to the model
provider configured for Pi, and writes the native session history. The Pix
relay never becomes a Pi runtime, and Pix does not shadow that history in a
cloud store.

## Optional TUI integration

The `@zaincheung/pix` Pi extension connects an interactive terminal TUI to the
same Pix host. It follows Pi's Extension API and leaves the `pi` executable in
charge. See [Use Pix with Pi TUI](/docs/pi-tui-bridge) for the user flow.

The boundary matters when something is unavailable: Pix cannot use a host that
is offline, and Pi continues locally when Pix or its relay is unavailable.

For the host components behind this boundary, see [Architecture](/docs/architecture).
