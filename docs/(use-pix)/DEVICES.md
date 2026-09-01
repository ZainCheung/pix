---
title: Devices
description: List, pair, and revoke the phones trusted by a Pix host.
---

Pix keeps a paired-device record on the host. Use the host CLI or the Pix
macOS settings to see which phones are trusted.

## See paired devices

```sh
pix device list
```

The list shows each device name and its pairing time. It does not print the
device's public key.

## Pair another phone

Start a new flow at any time:

```sh
pix device pair
```

Use `pix device pair --remote` when the phone is on another network and an
enabled relay is configured. Approve the new request on the host after
checking its confirmation code. See [Pair your iPhone](/docs/pairing).

## Revoke a device

Find the device ID with `pix device list`, then revoke it:

```sh
pix device revoke <device-id>
```

Revocation removes the device's host record and closes its live connections.
The device cannot reconnect until you pair it again.

If a phone is lost, revoke its record as soon as possible. You can pair a
replacement phone, or re-pair the recovered phone after confirming the new
request.

For the trust lifecycle behind these records, see [Pairing and
trust](/docs/pairing-and-trust).
