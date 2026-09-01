---
title: Pair your iPhone
description: Connect an iPhone to a Pix host and approve the device safely.
---

Pairing gives one iPhone permission to connect to one Pix host. Start the flow
on the computer, then approve the phone you recognize.

## Start pairing

Use `pix setup` during first use, or run the focused command later:

```sh
pix device pair
```

For a phone on another network, configure and enable a relay first, then use:

```sh
pix device pair --remote
```

`--remote` requires an enabled relay endpoint. Setup can configure Pix's
hosted relay for you.

## Connect the phone

On the iPhone, open Pix and follow the pairing prompt:

- On the same network, choose the nearby host that Pix discovers.
- For remote pairing, scan the QR code shown by the host. The remote offer is
  short-lived; the host also prints a typable join code.

The QR offer expires after two minutes. Start a new pairing flow if it expires.

## Confirm and approve

After the phone connects, the host shows a six-digit confirmation code. Check
that it matches the code shown on the iPhone. Approve the request on the host
only when the device name and code are expected.

After approval, Pix stores the phone as a paired device. The phone can then
connect again over the local network or the configured relay without repeating
the first-use flow.

## If pairing fails

Check [Troubleshooting](/docs/troubleshooting) for discovery, relay, and
expired-offer checks. [Pairing and trust](/docs/pairing-and-trust) explains
what the host remembers and how revocation changes that trust.
