// Pix relay Worker: validates join requests and routes each rendezvous
// channel to its own Durable Object. The Worker never parses, stores, or
// logs application payloads; everything beyond the upgrade request is an
// opaque end-to-end encrypted frame.

export { RelayChannel } from "./channel";

export interface Env {
  RELAY_CHANNEL: DurableObjectNamespace;
}

const CHANNEL_PATH = /^\/v1\/channel\/([0-9a-f]{64})$/;
const PROOF_PATTERN = /^[0-9a-f]{64}$/;
const SUPPORTED_PROTOCOL = "1";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const match = CHANNEL_PATH.exec(url.pathname);
    if (match === null) {
      return new Response("not found", { status: 404 });
    }
    if (request.method !== "GET") {
      return new Response("method not allowed", { status: 405 });
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected a WebSocket upgrade", {
        status: 426,
        headers: { Upgrade: "websocket" },
      });
    }
    if (request.headers.get("X-Pix-Protocol") !== SUPPORTED_PROTOCOL) {
      return new Response("unsupported protocol version", { status: 400 });
    }
    const role = request.headers.get("X-Pix-Role");
    if (role !== "host" && role !== "client") {
      return new Response("invalid role", { status: 400 });
    }
    const proof = request.headers.get("X-Pix-Join-Proof") ?? "";
    if (!PROOF_PATTERN.test(proof)) {
      return new Response("invalid join proof", { status: 400 });
    }

    const channelId = match[1] ?? "";
    const id = env.RELAY_CHANNEL.idFromName(channelId);
    return env.RELAY_CHANNEL.get(id).fetch(request);
  },
} satisfies ExportedHandler<Env>;
