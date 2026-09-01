---
title: Self-host a relay
description: Deploy the existing Pix relay Worker to your own Cloudflare account.
---

This page covers relay deployment for people who want to run the existing
`relay/` Worker in their own Cloudflare account. It is separate from normal
Pix remote use.

## Deploy the Worker

The repository includes a [Deploy to Cloudflare
button](https://developers.cloudflare.com/workers/platform/deploy-buttons/)
for the `relay/` subdirectory:

<p align="center">
  <a href="https://deploy.workers.cloudflare.com/?url=https://github.com/ZainCheung/pix/tree/main/relay"><img src="https://deploy.workers.cloudflare.com/button" alt="Deploy to Cloudflare"></a>
</p>

The button copies `relay/` into your GitHub account, provisions the Durable
Object binding described by `relay/wrangler.jsonc`, and deploys the Worker. It
does not change Pix's hosted relay.

## Configure Pix

Copy the Worker hostname from Cloudflare and set its WebSocket endpoint on the
host:

```sh
pix relay set wss://worker-name.subdomain.workers.dev
pix relay show
```

The relay never receives the channel secret. The host and client derive the
channel identifier and join proofs locally inside `pix-wire`.

## Local development

Run the Worker locally with:

```sh
cd relay
npm ci
npm run dev
```

For the existing relay contract, limits, and test fixtures, see the
[relay README](https://github.com/ZainCheung/pix/blob/main/relay/README.md).
