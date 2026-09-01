---
title: Use Pix with Pi TUI
description: Connect an interactive Pi terminal session to Pix with the optional npm extension.
---

I already use `pi` in the terminal. Can Pix follow the same session?

Yes. The optional `@zaincheung/pix` extension connects an interactive Pi TUI
session to the Pix host on the same computer. Pi remains the agent and Pix
shows the same session on the phone.

## Install the extension

Install the package through Pi after Pix Host is installed and set up:

```sh
pi install npm:@zaincheung/pix
```

Restart Pi after installing, or run `/reload` in an existing interactive
session.

## What appears in Pix

When the host is available, Pix can show the TUI session's current snapshot,
including messages, model, thinking level, and active tools. Assistant and tool
updates appear while Pi is running, along with session and compaction status.

Pix can send the TUI:

- text prompts and abort requests;
- model-list requests, model selection, and thinking-level changes;
- the available command list and a session rename.

## If the host is unavailable

Pi continues as a normal standalone TUI when Pix Host is not running. Start the
host, then run `/reload` when you want that session to attach to Pix.

## Current limits

The TUI bridge accepts text prompts only. Image attachments are not supported
there, and the bridge does not expose every native Pi control. Steer,
follow-up, compact, fork, and shutdown commands remain unavailable for TUI
owners.

If the session does not appear, check [Troubleshooting](/docs/troubleshooting).
For the ownership and reconnect rules behind this feature, see [TUI bridge
internals](/docs/tui-bridge-internals).
