---
title: Local-first by design
description: What Pix keeps on your computer, what it sends remotely, and the trade-off of that choice.
---

“Local-first” in Pix means the host computer remains the working environment.
The phone connects to that environment instead of moving the development
environment to a hosted service.

## What stays on your computer

- Pi and the tools it runs.
- Your repository and other files inside an authorized workspace.
- Credentials that Pi uses on the host.
- Native Pi session files and their conversation history.
- Pix host configuration and paired-device records.

Pix Host keeps host configuration, paired-device records, and the local state
needed to authorize workspaces and bridge requests to Pi. It does not maintain
a second durable copy of the Pi conversation.

## What Pix does remotely

Pix sends session requests from the paired phone to the host and returns Pi's
session events. The connection can be direct on a local network or routed
through a relay when the devices are separated. Both paths carry the same
encrypted Pix frames.

## The trade-off

The computer running Pi must be powered on and reachable for Pix to use it
remotely. Direct access needs both devices on the same network. A relay makes
different networks possible, but relay availability affects remote reachability.

The benefit is a single local environment: files, credentials, Pi tools, and
session history remain where you already use them.

See [How Pix works](/docs/how-pix-works) for the component boundaries behind
this local-first model.
