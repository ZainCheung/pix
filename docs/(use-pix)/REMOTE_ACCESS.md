---
title: Remote access
description: Use Pix on the same network or reach your host through a relay when you are away.
---

Pix has two connection paths. Pi keeps running on the host in both cases.

## Same network: direct connection

When the iPhone and host are on the same local network, Pix discovers the host
with Bonjour and connects directly:

```text
iPhone / Pix App  ─────────  Pix Host
                         direct LAN
```

No relay is needed. Keep the host service running and allow local discovery on
the network. Pair with `pix setup` or `pix device pair`.

## Different networks: relay connection

When the phone is away from the host's network, the host opens an outbound
connection to the configured relay:

```text
iPhone / Pix App  ──  Relay  ──  Pix Host  ──  Pi
```

The relay forwards encrypted Pix frames between the phone and host. It does
not run Pi or store application messages.

Pix setup offers the hosted relay at `wss://pix-relay.deepoke.com`. To use a
different endpoint on the host:

```sh
pix relay set wss://relay.example.com
pix relay enable
pix service restart
```

Use `pix relay show` to inspect the saved endpoint. `pix relay disable` keeps
the endpoint but returns the host to LAN-only transport; `pix relay clear`
removes it.

## If the relay is unavailable

Remote reachability stops while the relay connection is down. Pi's local
process and session continue on the host. A direct LAN connection can still
work when both devices are on the same network.

## Self-hosting

Running your own relay is a separate infrastructure task. See [Self-host a
relay](/docs/self-host-relay) for the deployment instructions.

Pair a device with [Pair your iPhone](/docs/pairing). For the reason behind the
two connection paths, see [Direct connection vs relay](/docs/direct-vs-relay).
