---
title: Sessions
description: Open, create, and continue native Pi sessions from Pix.
---

A Pix session is a Pi session that the host exposes to a paired phone. The
session belongs to the workspace where Pi created it, and its history stays in
Pi's native session storage on the host.

## Open an existing session

Select a workspace in Pix, then choose one of the sessions discovered there.
Pix asks the host to start Pi for that existing session and attaches the
phone. You can return to the same session later from the workspace's session
list.

## Create and switch

Choose a workspace and create a new session when you want a fresh Pi
conversation. To switch, leave the current view and attach another session
from the same or a different authorized workspace. Pix supports session
renaming and sends the new name to Pi.

## During a run

Send a prompt from the session view. If Pi is running, Pix can send an abort
request. The session view receives Pi's assistant and tool updates as they
arrive.

## If Pix disconnects

A phone disconnecting does not copy the session to a server or stop Pi's local
work. Reconnect to the host and open the same session. An idle runtime may be
released by the host; the native session file remains available to open again.

Pix does not create a separate cloud copy of a Pi session. Pi's native session
data on your computer is authoritative.

## Next

- [Workspaces](/docs/workspaces)
- [Sessions & ownership](/docs/session-ownership)
- [Troubleshooting](/docs/troubleshooting)
