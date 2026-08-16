# Pix troubleshooting

Start with the four commands below. They inspect the resolved configuration,
Pi compatibility, host liveness, and payload-free operational history:

~~~bash
pix doctor
pix status
pix logs --tail 100
pix diagnostics export ./diagnostics
~~~

Use an explicit configuration path when the problem is isolated to a test or
service instance:

~~~bash
pix --config /path/to/config.json doctor
pix --config /path/to/config.json status
~~~

Do not attach raw configuration, host identity files, pairing tokens, or
unredacted logs to an issue.

## Pi is missing or incompatible

`pix doctor` probes the Pi executable and checks the RPC flags and
version range that Pix currently supports:

~~~bash
pix doctor
pix doctor --pi /absolute/path/to/pi
pix pi set /absolute/path/to/pi
~~~

The verified range is `>=0.84.1, <0.85.0`. If Pi is installed
through a version manager, use `pix pi set` to pin the executable
that the host should launch. A successful probe must advertise
`--mode`, `--approve`, `--session`, and
`--session-id`.

If the probe works in a terminal but not from a background service, inspect
the resolved environment printed by `pix doctor`. GUI and systemd
launches may have a different `PATH` from an interactive shell.

## Workspace is inaccessible

Pix only exposes explicitly authorized canonical workspace roots. Check the
current registry and authorize the intended project directory:

~~~bash
pix workspace list
pix workspace add /absolute/path/to/project
~~~

Use the root directory that contains the files the Pi session should access.
If a path was moved, renamed, or replaced by a symlink, remove the old entry
and add the current path again:

~~~bash
pix workspace remove <workspace-id>
pix workspace add /absolute/path/to/project
~~~

Never work around an authorization error by exposing a broad parent directory
that contains unrelated files.

## A client cannot discover the host

For LAN pairing and access:

1. Start `pix setup` for first-use pairing, or `pix serve` for an already
   paired device.
2. Keep the host process running while the client searches.
3. Confirm the client and host are on the same network and that local
   discovery is allowed.
4. Confirm the six-digit code shown by `pix setup` on the phone, then accept
   the pairing prompt.
5. Check `pix status` for a live service and paired-device count.

The focused `pix device pair` command also starts its own foreground host and
refuses to run while another Pix host owns the same configuration. If a
service is already running, stop it before a new interactive pairing flow:

~~~bash
pix service stop
pix device pair
~~~

After pairing, `pix setup` installs and starts the Linux user service unless
`--no-service` was supplied. `pix serve --json-events` remains the foreground
diagnostic/native-UI bridge.

## Relay or remote pairing fails

Inspect the stored endpoint and active flag:

~~~bash
pix relay show
pix status
~~~

Configure a WebSocket endpoint with the scheme that the deployment supports:

~~~bash
pix relay set wss://relay.example.com
pix relay enable
~~~

For a local Worker, use the URL printed by your Wrangler dev server. The relay
does not receive the channel secret, so a successful `pix relay show`
does not prove that the endpoint is reachable.

For remote pairing, run `pix setup` or `pix device pair`. Pix starts the
short-lived pairing channel and renders a QR automatically. If the code
expires, start a new pairing flow. Treat the QR and join code as credentials
and do not paste them into issues or logs.

Inspect only payload-free relay lifecycle entries:

~~~bash
pix logs --tail 200
~~~

If the relay is unavailable, a direct LAN connection can still work. Relay
loss changes remote reachability only; it does not stop Pi's local process.

## The background service is not running

The built-in service manager is a Linux systemd user unit:

~~~bash
pix status
pix service install
systemctl --user status pix.service
~~~

To enable the unit without starting it immediately:

~~~bash
pix service install --no-start
systemctl --user start pix.service
~~~

To stop or remove it:

~~~bash
pix service stop
pix service uninstall
~~~

No root privileges are required. If `systemctl` is unavailable, run
`pix serve` under the process manager provided by your platform.
There is no public macOS Pix host installer in this repository.

## A service starts but the client cannot connect

Check all of the following:

- `pix status` reports a live service, not a stale status file.
- At least one workspace is listed by `pix workspace list`.
- The intended Pi executable is shown by `pix pi show`.
- The client device is still listed by `pix device list`.
- Relay transport is enabled only when its endpoint is valid.
- The host and client clocks are not so far apart that pairing offers expire.

A service restart does not grant a new device access. Revoke and pair again if
the device identity is no longer trusted.

## Logs and diagnostic bundles

The host log location is printed by `pix logs` and is derived from
the Pix configuration directory. Logs are payload-free and contain no prompts,
files, model output, private keys, pairing tokens, or relay channel secrets.

Create a scrubbed bundle for a maintainer:

~~~bash
pix diagnostics export ./diagnostics
~~~

The command refuses to overwrite an existing archive. Review the archive before
sharing it and remove any unrelated local notes from the destination directory.

## Configuration confusion

Every command accepts the global `--config <path>` override. The
resolved path is printed by `pix status` and `pix doctor`:

~~~bash
pix --config /tmp/pix.json status
pix --config /tmp/pix.json logs
~~~

If a service was installed with a custom configuration path, use that same
path for status, logs, and service commands. The Linux systemd unit stores the
absolute path it was installed with.

## Reporting a problem

Before opening an issue:

1. Reproduce with the smallest authorized workspace possible.
2. Run `pix doctor`, `pix status`, and
   `pix diagnostics export ./diagnostics`.
3. Record the Pix version, operating system, architecture, and whether the
   failure is LAN-only, relay-only, or both.
4. Remove paths, prompts, session content, credentials, keys, tokens, and
   relay secrets from any report.

For suspected vulnerabilities, follow [SECURITY.md](../SECURITY.md) instead of
filing a public issue.
