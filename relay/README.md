# Pix relay

A content-blind rendezvous relay: one Cloudflare Worker routing each channel
to its own Durable Object using the Hibernatable WebSocket API.

The relay forwards opaque, end-to-end encrypted binary frames between exactly
one `host` and one `client` connection per channel. It never parses, queues,
persists, or logs application payloads. Losing the relay affects
reachability only; the Pix secure channel (Noise, implemented in `pix-wire`)
never terminates here.

## Deploy your own Relay

<p align="center">
  <a href="https://deploy.workers.cloudflare.com/?url=https://github.com/ZainCheung/pix/tree/main/relay"><img src="https://deploy.workers.cloudflare.com/button" alt="Deploy to Cloudflare"></a>
</p>

The button creates a copy of this isolated `relay/` Worker in your GitHub and
Cloudflare accounts, including its Durable Object binding. After deployment,
configure Pix with the returned WebSocket endpoint:

```sh
pix relay set wss://worker-name.subdomain.workers.dev
```

This deploys to your account; it does not modify Pix's hosted relay. See
[`docs/REMOTE_ACCESS.md`](../docs/REMOTE_ACCESS.md) for the pairing flow and
self-hosting notes.

## Contract

Join request:

```text
GET /v1/channel/{channel_id}
Upgrade: websocket
X-Pix-Protocol: 1
X-Pix-Role: host | client
X-Pix-Join-Proof: 64 lowercase hex characters
```

- `channel_id` and the per-role join proofs are derived from a 32-byte
  channel secret by `pix-wire` (`relay_channel_id`, `relay_join_proof`). The
  secret itself never reaches the relay.
- The first proof seen for each role of a channel is pinned in Durable
  Object storage; later joins must present the same proof (403 otherwise).
- One connection per role. A newer valid join supersedes the older
  connection of the same role (close code 4008), which is what a phone
  switching from Wi-Fi to cellular needs.
- Binary messages are single length-prefixed encrypted records, at most
  1 MiB + 4 bytes, forwarded verbatim to the opposite role and dropped if
  the peer is absent.
- Text messages from endpoints are a protocol violation (close 1008). The
  relay itself sends `{"type":"peer_joined"}` and `{"type":"peer_left"}`.
- Per-connection token buckets bound message and byte rates (close 4013).
- Channels idle for 24 hours lose their pinned proofs via a Durable Object
  alarm; endpoints simply re-pin on the next join.

Structured logs contain only event names, roles, truncated channel labels,
close codes, and byte counts.

## Develop

```bash
npm install
npm test          # vitest + @cloudflare/vitest-pool-workers
npm run typecheck
npm run dev       # local wrangler dev server
npm run deploy    # requires Cloudflare credentials (Release 2 accounts)
```

The test suite consumes `protocol/fixtures/v1/relay-channel.json`, generated
by `cargo run -p pix-wire --example generate_fixtures`, so relay expectations
and Rust/Swift derivations cannot drift silently.
