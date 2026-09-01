---
title: Troubleshooting
description: Fix common Pix installation, pairing, session, workspace, and connection problems.
---

Start with these checks on the host:

```sh
pix status
pix logs --tail 100
```

`pix diagnostics export ./diagnostics` creates a scrubbed bundle for a
maintainer. Review it before sharing. Do not share QR offers, pairing codes,
prompts, session content, credentials, or private keys.

## Pix can't find my computer

**Checks**

- Confirm `pix status` reports a running host service.
- For a direct connection, put the iPhone and host on the same network and
  allow local discovery.
- For a relay connection, run `pix relay show` and check that the endpoint is
  enabled.

**Fix**

Start the service if needed:

```sh
pix service start
```

Then retry discovery in Pix. See [Remote access](/docs/remote-access) if the
phone is on another network.

## Pairing fails

**Checks**

- Start a fresh flow with `pix setup` or `pix device pair`.
- For remote pairing, confirm a relay is configured and enabled with
  `pix relay show`.
- Check that the six-digit code on the phone matches the host before approval.
- A pairing offer expires after two minutes. A remote QR offer must be scanned
  before it expires.

**Fix**

Run `pix device pair --remote` only when an enabled relay is configured. For a
local pairing, keep both devices on the same network. If a request is waiting
for approval, inspect it with:

```sh
pix device pending
```

## My session doesn't appear

**Checks**

- Select the workspace where Pi created the session.
- Confirm the workspace is still authorized:

  ```sh
  pix workspace list
  ```

- Confirm the host service can find Pi with `pix status`.

**Fix**

Add the intended folder again if it was moved or removed. Pix discovers
existing native Pi sessions from each authorized workspace.

## Pix says the host is offline

**Checks**

Run:

```sh
pix status
pix service status
```

**Fix**

Start or install the per-user service:

```sh
pix service install
pix service start
```

If the host service starts and stops again, run `pix status` and inspect the
last entries from `pix logs --tail 100`.

## Pix works on my local network but not remotely

**Checks**

```sh
pix relay show
```

The endpoint must be a valid `ws://` or `wss://` URL and relay transport must
be enabled. A running service may need a restart after the setting changes.

**Fix**

```sh
pix relay set wss://relay.example.com
pix relay enable
pix service restart
```

Relay loss affects remote reachability. It does not stop Pi on the host.

## My Pi TUI session doesn't appear in Pix

**Checks**

- Install `@zaincheung/pix` with `pi install npm:@zaincheung/pix`.
- Restart Pi or run `/reload` in the interactive TUI.
- Confirm the Pix host service is running with `pix status`.

**Fix**

Start the host, then run `/reload`. Pi remains usable as a standalone TUI
when the host is unavailable. The [Pi TUI guide](/docs/pi-tui-bridge) lists the
controls and current limits.

## Messages stopped updating

**Checks**

- Check `pix status` and the connection path you are using.
- Reopen the same session after reconnecting the phone.
- For a TUI session, check that Pi still shows the Pix status after `/reload`.

**Fix**

If Pi is still running on the host, reconnecting to the same session resumes
the view. If the host is unavailable, restore it first and then attach again.
Use `pix logs --tail 200` for payload-free connection events.

## The `pix` command isn't found

The installer puts the executable in `~/.local/bin`. Add that directory to your
shell `PATH`, then open a new shell or reload its configuration:

```sh
export PATH="$HOME/.local/bin:$PATH"
pix status
```

## Pix stopped working after an update

**Checks**

```sh
pix status
pix service status
```

Run `pix status` to check Pi compatibility. If several Pi installations exist,
select the executable Pix should use:

```sh
pix pi set /absolute/path/to/pi
pix service restart
```

## A workspace is inaccessible

Pix only uses folders that are explicitly authorized and still available on the
host. If the folder was moved, remove the old authorization and add the new
path:

```sh
pix workspace remove <workspace-id>
pix workspace add /absolute/path/to/project
```

## Deeper diagnostics

Still not working? See [Diagnostics](/docs/diagnostics) for status, logs, and
a shareable bundle. The [CLI reference](/docs/cli) has command details, and
[Architecture](/docs/architecture) explains host and transport boundaries.
