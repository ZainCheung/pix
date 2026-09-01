---
title: Quickstart
description: Get from an installed Pix host to your first Pi session on an iPhone.
---

Pix needs two things: Pi on your Mac or Linux computer and Pix on your
iPhone. Pi, your files, and your session data stay on the computer.

## Prerequisites

- Pi is installed and works in a terminal. Pix currently verifies Pi
  `>=0.84.1, <0.85.0`.
- The host is an Apple Silicon Mac or a Linux x86_64/ARM64 computer.
- Pix is installed on the iPhone you want to pair.

## Step 1: Install the host

Run the first-party installer on the computer where Pi is installed:

```sh
curl -fsSL https://pix.deepoke.com/install.sh | sh
```

Then start the setup flow:

```sh
pix setup
```

Setup checks Pi, asks which local folder Pix may use, offers local-network or
relay connectivity, and installs the per-user host service.

## Step 2: Pair your iPhone

Keep the phone and computer on the same network for a direct connection. Open
Pix on the iPhone and choose the nearby host shown by the pairing flow.

If setup selected relay access, scan the QR code printed by the host instead.
Compare the six-digit confirmation code on both devices, then approve the
request on the host.

## Step 3: Open a session

In Pix, select the workspace you authorized during setup. Open an existing Pi
session or create a new one. Send a prompt and wait for Pi's response.

## Success

The host is online, the workspace appears on the phone, and Pi responds inside
the selected session. Your repository and the native Pi session remain on the
computer.

## Next

- [Use Pix remotely](/docs/remote-access)
- [Continue existing sessions](/docs/sessions)
- [How Pix works](/docs/how-pix-works)
