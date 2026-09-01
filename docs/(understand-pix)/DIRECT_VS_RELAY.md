---
title: Direct connection vs relay
description: Understand what changes when Pix uses your local network or a relay.
---

Pix keeps the same secure application channel on both transport paths. The
difference is how the phone reaches the host.

| | Direct connection | Relay connection |
| --- | --- | --- |
| Where it works | iPhone and host on the same local network | iPhone and host on different networks |
| How the host is reached | Bonjour discovery, then direct TCP | Host opens an outbound WebSocket to the configured relay |
| What runs Pi | The host computer | The host computer |
| What the relay handles | Nothing | Channel rendezvous and forwarding of encrypted frames |
| If the path fails | The phone cannot reach the host on that LAN | Remote reachability stops; Pi keeps running locally |

## The secure channel stays the same

Pix uses the `pix-wire` encrypted framing after either path is established. A
relay does not terminate that application channel, decrypt it, or turn into a
Pi runtime.

The relay does handle routing metadata needed to operate the channel, such as
the connection role and payload-free operational counters. It forwards opaque
binary frames, does not queue them when the peer is absent, and does not store
application payloads.

## What remains on the host

Pi and the native session remain on the host in both modes. Switching from
direct to relay changes reachability and availability dependencies, not session
ownership.

Use [Remote access](/docs/remote-access) for configuration steps and [How Pix
works](/docs/how-pix-works) for the overall model.
