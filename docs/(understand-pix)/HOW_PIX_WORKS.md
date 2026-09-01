---
title: How Pix works
description: See where Pix, Pi, your files, and your session data run.
---

Pix is a remote interface to Pi on your own computer. The phone provides the
client view; the host computer remains the place where Pi does the work.

## The mental model

```text
iPhone / Pix App
        │  encrypted Pix connection
        │
Pix Host on your Mac or Linux computer
        │  local interaction
        │
Pi + tools + repository + native sessions
```

When the phone is away from the host's network, a relay sits between the two
endpoints:

```text
iPhone ── Relay ── Pix Host ── Pi
```

The relay changes the route, not the place where Pi runs.

## What runs where

### iPhone

The Pix app shows authorized workspaces and Pi sessions, sends session actions,
and receives the host's session updates.

### Host computer

Pix Host owns the host identity, paired-device records, workspace
authorization, network listeners, and the Pi processes it starts. It launches
Pi in the selected workspace and bridges the phone to Pi.

### Pi

Pi remains the agent. It reads the repository, runs its tools, talks to the
configured model provider, and writes its native session history.

## What stays on the host

Repositories, local credentials, Pi processes, and native Pi sessions remain
on the host computer. Pix does not create a second cloud message database.

With a relay, the relay forwards the encrypted Pix frames needed to connect the
phone and host. It does not run Pi or receive the workspace and session data as
application content.

For transport and security boundaries, continue to [Architecture](/docs/architecture).

## Next

- [Local-first by design](/docs/local-first)
- [Direct connection vs relay](/docs/direct-vs-relay)
- [Pix and Pi](/docs/pix-and-pi)
