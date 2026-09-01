---
title: Pairing and trust
description: How Pix decides which phones may connect to a host and how that trust changes.
---

Pairing turns one phone into a device the host recognizes. The host asks you
to confirm the phone before it stores that trust.

## The pairing record

During pairing, the phone presents a device identity and a display name. The
host keeps a paired-device record with that identity, name, and pairing time.
Later connections must prove the same device identity before Pix accepts
requests.

## QR offer and confirmation

For remote pairing, `pix device pair --remote` creates a short-lived QR offer
through the configured relay. The offer and its typable join code expire after
two minutes. Local pairing uses the nearby host discovered on the local
network.

After the phone joins the pairing flow, both sides show a six-digit
confirmation code. Compare the codes, then approve the request on the host.
The host does not persist trust until that approval succeeds.

## What survives pairing

An approved device can reconnect later over the local network or the configured
relay using its stored identity. The two-minute expiry applies to an in-flight
pairing offer, not to an approved device record.

## Revocation and re-pairing

Use `pix device revoke <device-id>` to remove a device's record. Pix closes its
live connections and rejects later reconnects for that identity. Pair the
phone again to create a new approved record.

Pairing controls access to the Pix host. Workspace authorization still limits
which local folders a paired device can use.

For the user flow, see [Pair your iPhone](/docs/pairing). For transport and
relay boundaries, see [Security and privacy](/docs/security).
