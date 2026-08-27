# Remote access

Pix supports two paths between a client and the host. Both paths use the same
encrypted Pix channel after the connection is established.

## Local network

When the phone and computer are nearby, Pix advertises the host with Bonjour
and the client connects directly over TCP:

```text
Pix Client ───────────────► Pix Host
              Direct LAN
```

The host does not need a relay for this path. Keep both devices on the same
network, start `pix setup` or `pix device pair`, and choose the host from the
client's nearby-host list. Pairing still requires comparing the six-digit
confirmation code and approving the request on the host.

## Relay access

When the devices are on different networks, the host opens an outbound WebSocket
connection to the configured relay:

```text
Pix Client ── encrypted ──► Pix Relay ── encrypted ──► Pix Host
```

The relay only authenticates channel roles and forwards opaque encrypted
frames. It does not run Pi, receive a workspace, terminate the Pix secure
channel, queue messages, or persist application payloads. Relay loss changes
reachability; Pi and its local session continue on the host.

The interactive setup wizard offers Pix's hosted relay at
`wss://pix-relay.deepoke.com`. To use another endpoint:

```sh
pix relay set wss://relay.example.com
pix relay show
pix relay enable
```

Use `pix relay disable` to keep the endpoint but return to LAN-only transport,
or `pix relay clear` to remove it.

## Pair over a relay

With an active relay endpoint, `pix setup` and `pix device pair` create a
single-use pairing channel:

1. Start `pix setup` or `pix device pair` on the host.
2. Open Pix on the iPhone and scan the QR code printed by the host.
3. Compare the six-digit confirmation code shown on both devices.
4. Approve the request on the host.

The QR offer and its short pairing channel expire after two minutes. The host
stores the paired device and can revoke it later with `pix device revoke`.

## Deploy a self-hosted Relay

The public `relay/` directory is a Cloudflare Worker with a Durable Object
binding. Cloudflare provides an official [Deploy to Cloudflare
button](https://developers.cloudflare.com/workers/platform/deploy-buttons/) for
public Workers repositories, including isolated subdirectories in a monorepo:

<p align="center">
  <a href="https://deploy.workers.cloudflare.com/?url=https://github.com/ZainCheung/pix/tree/main/relay"><img src="https://deploy.workers.cloudflare.com/button" alt="Deploy to Cloudflare"></a>
</p>

The button creates a copy of `relay/` in your GitHub account, provisions the
Durable Object described by `relay/wrangler.jsonc`, and deploys the Worker to
your Cloudflare account. It does not change Pix's hosted relay. Cloudflare
treats `relay/` as the project root, so keep runtime dependencies and Wrangler
configuration inside that directory.

After deployment, copy the Worker hostname from Cloudflare and configure Pix
with a WebSocket URL:

```sh
pix relay set wss://worker-name.subdomain.workers.dev
pix relay show
```

The relay never receives the channel secret. The host and client derive the
channel identifier and join proofs from that secret inside `pix-wire`.

For a local Worker during development:

```sh
cd relay
npm ci
npm run dev
```

For the contract, limits, and test fixtures, see [relay/README.md](../relay/README.md).

## Security notes

- Use `wss://` for a deployed relay. Use `ws://` only for a trusted local endpoint.
- Pair only devices you recognize and revoke devices you no longer use.
- Authorize workspace roots explicitly with `pix workspace add`.
- Share diagnostic bundles only after checking that they contain no local details.
