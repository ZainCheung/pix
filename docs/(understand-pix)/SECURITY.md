---
title: Security and privacy
description: The boundaries Pix uses for device access, workspaces, sessions, and relays.
---

Pix keeps the working environment on the host computer and requires an
explicitly paired device before accepting remote requests. The boundaries
below describe what the current implementation does.

## Device pairing

The host creates a short-lived pairing offer, shows a confirmation code after
the phone joins, and waits for explicit host approval. Approved devices are
stored by identity. Revoking a device removes that record, closes its live
connections, and rejects later reconnects until it is paired again.

## Encrypted connection

Direct LAN connections and relay connections carry the same encrypted Pix
frames. The `pix-wire` implementation owns framing, protocol validation,
encryption, and replay protection; clients do not reimplement those pieces.

## Workspace authorization

Pix exposes only workspace roots that you explicitly authorize. Removing a
workspace removes Pix access to that root; it does not delete the folder or its
files. The host checks authorization again when a device requests a workspace
or session.

## What the relay can see

The relay forwards opaque binary frames and does not decrypt, parse, queue, or
persist application payloads. It does not run Pi or receive the repository,
credentials, or session history as application content.

The relay still needs routing information to operate. Its payload-free logs
contain event names, connection roles, short channel labels, close codes, and
byte counts.

## Local credentials and sessions

Pi runs on the host and uses the credentials available in that host
environment. Repositories, native Pi session files, and conversation history
remain on the host. Pix is not an account system and does not create a cloud
message database.

Host logs and diagnostic bundles are designed to be payload-free. Review a
diagnostic archive before sharing it and remove unrelated local notes.

## Security boundaries

Pairing controls which devices may connect. Workspace authorization controls
which local roots a paired device may use. The host and Pi remain the execution
environment, so Pix cannot make an offline host or an unavailable relay
reachable.

For implementation invariants, see [Architecture](/docs/architecture). To
report a suspected vulnerability, follow the private-contact instructions in
the repository's [security policy](https://github.com/ZainCheung/pix/blob/main/SECURITY.md).

For the trust lifecycle, see [Pairing and trust](/docs/pairing-and-trust). The
availability trade-offs are listed in [Limitations](/docs/limitations).
