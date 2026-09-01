---
title: Sessions and ownership
description: Why a Pi session has one live writer and how Pix and Pi TUI share that rule.
---

A Pi session has one live writer at a time. That prevents two processes from
appending conflicting history to the same native session file.

## The source of truth

Pi's native JSONL session is the durable conversation store. Pix Host starts or
attaches to Pi and may keep an in-memory index for reading history, but it does
not create a second message database.

## Pix ownership

When Pix opens a session, the host starts one Pi process for that session and
claims its session lock. Other Pix clients can attach to that running process,
but a second Pi writer cannot claim the same session. Releasing a host-owned
runtime returns the session so another Pi process can resume it.

## Pi TUI ownership

The optional TUI bridge uses the same session lock. A live TUI owner blocks a
new Pix RPC writer, even if its bridge socket is temporarily unreachable. The
host keeps an unavailable owner record until the TUI reconnects instead of
silently starting a second writer.

When `/resume` selects another session, the extension asks the host to validate
and reserve that target before Pi switches. The short reservation expires
after five seconds and is consumed by the matching TUI process. `/new`,
`/fork`, `/quit`, and signal-driven shutdown release the current TUI ownership;
an extension reload keeps it for same-process reconnect.

## Disconnects and reconnects

Closing a Pix client detaches that client. Pi work can continue on the host,
and another client can attach to the same native session later. An attached TUI
retries a lost bridge with bounded delays of 1, 2, 5, and 10 seconds, then a
30-second cap. A TUI that started without a host does not claim the session
later on its own; `/reload` is the explicit retry.

The single-writer rule keeps history consistent. Its cost is that two
interactive owners cannot write the same session at the same time.

For the complete lock and bridge protocol, see [Architecture](/docs/architecture)
and [TUI bridge internals](/docs/tui-bridge-internals).
