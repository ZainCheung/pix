import { DurableObject } from "cloudflare:workers";
import {
  BYTES_PER_SECOND,
  BYTE_BURST,
  CLOSE_MESSAGE_TOO_LARGE,
  CLOSE_PROTOCOL_VIOLATION,
  CLOSE_RATE_LIMITED,
  CLOSE_SUPERSEDED,
  IDLE_CHANNEL_TTL_MS,
  MAX_MESSAGE_BYTES,
  MESSAGES_PER_SECOND,
  MESSAGE_BURST,
} from "./limits";

type Role = "host" | "client";

interface Attachment {
  role: Role;
  // First eight hex characters of the channel identifier, for payload-free
  // logs only. Never sufficient to address or join the channel.
  label: string;
}

interface TokenBuckets {
  messages: number;
  bytes: number;
  refilledAt: number;
}

interface Counters {
  rxMessages: number;
  rxBytes: number;
  firstForwardMs: number;
}

/**
 * One rendezvous channel between exactly one host and one client.
 *
 * The channel is content-blind: binary frames are forwarded to the opposite
 * role without parsing, nothing is queued or persisted, and the only durable
 * state is the pair of pinned join proofs used to keep strangers out of an
 * established channel. Application security never depends on this object;
 * frames remain end-to-end encrypted by the Pix Noise transport.
 */
export class RelayChannel extends DurableObject {
  /** In-memory rate/observability state; reset on hibernation is acceptable. */
  private buckets = new Map<WebSocket, TokenBuckets>();
  private counters = new Map<WebSocket, Counters>();

  override async fetch(request: Request): Promise<Response> {
    const joinedAt = Date.now();
    const url = new URL(request.url);
    const channelId = url.pathname.split("/").pop() ?? "";
    const label = channelId.slice(0, 8);
    const role = request.headers.get("X-Pix-Role") as Role;
    const proof = request.headers.get("X-Pix-Join-Proof") ?? "";

    const pinnedKey = `proof:${role}`;
    const pinned = await this.ctx.storage.get<string>(pinnedKey);
    if (pinned === undefined) {
      await this.ctx.storage.put(pinnedKey, proof);
    } else if (!timingSafeEqual(pinned, proof)) {
      this.log({ event: "join_rejected", channel: label, role });
      return new Response("join proof mismatch", { status: 403 });
    }

    // Connection cardinality is one per role. A newer join with a valid
    // proof supersedes the previous connection of the same role, which is
    // what a phone moving from Wi-Fi to cellular needs; anything beyond
    // host+client can never be connected simultaneously.
    for (const existing of this.ctx.getWebSockets(role)) {
      try {
        existing.close(CLOSE_SUPERSEDED, "superseded by a newer connection");
      } catch {
        // Already dead; hibernated sockets can outlive their TCP connection.
      }
    }

    const pair = new WebSocketPair();
    const server = pair[1];
    this.ctx.acceptWebSocket(server, [role]);
    server.serializeAttachment({ role, label } satisfies Attachment);

    // A hibernated peer socket may be dead without a close event having
    // fired (silent TCP loss). Sending must never throw out of fetch —
    // that would 500 every future join of this channel.
    const joined = JSON.stringify({ type: "peer_joined" });
    let peerPresent = false;
    for (const peer of this.ctx.getWebSockets(otherRole(role))) {
      if (this.trySend(peer, joined)) {
        peerPresent = true;
      }
    }
    if (peerPresent) {
      server.send(joined);
    }

    await this.ctx.storage.setAlarm(Date.now() + IDLE_CHANNEL_TTL_MS);
    this.log({
      event: "join",
      channel: label,
      role,
      stage: "join",
      peer_present: peerPresent ? 1 : 0,
      elapsed_ms: Date.now() - joinedAt,
    });
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  override webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): void {
    const attachment = ws.deserializeAttachment() as Attachment;
    if (typeof message === "string") {
      // Endpoints never send text; control messages flow relay -> endpoint.
      this.log({
        event: "close",
        channel: attachment.label,
        role: attachment.role,
        reason: "text_message",
      });
      ws.close(CLOSE_PROTOCOL_VIOLATION, "text frames are not allowed");
      return;
    }
    if (message.byteLength > MAX_MESSAGE_BYTES) {
      this.log({
        event: "close",
        channel: attachment.label,
        role: attachment.role,
        reason: "oversized_frame",
        bytes: message.byteLength,
      });
      ws.close(CLOSE_MESSAGE_TOO_LARGE, "frame exceeds the 1 MiB limit");
      return;
    }
    if (!this.admit(ws, message.byteLength)) {
      this.log({
        event: "close",
        channel: attachment.label,
        role: attachment.role,
        reason: "rate_limited",
      });
      ws.close(CLOSE_RATE_LIMITED, "rate limit exceeded");
      return;
    }

    const counters = this.countersFor(ws);
    counters.rxMessages += 1;
    counters.rxBytes += message.byteLength;

    // Forward to the opposite role. Without a peer the frame is dropped:
    // the relay never queues, and clients resynchronize with a snapshot.
    const forwardStarted = Date.now();
    let forwarded = 0;
    for (const peer of this.ctx.getWebSockets(otherRole(attachment.role))) {
      if (this.trySend(peer, message)) {
        forwarded += 1;
      }
    }
    if (counters.rxMessages === 1) {
      counters.firstForwardMs = Date.now() - forwardStarted;
    }
    if (forwarded === 0 && counters.rxMessages === 1) {
      this.log({
        event: "forward",
        channel: attachment.label,
        role: attachment.role,
        stage: "forward",
        peer_present: 0,
        bytes: message.byteLength,
        elapsed_ms: Date.now() - forwardStarted,
      });
    }
  }

  override webSocketClose(ws: WebSocket, code: number): void {
    const attachment = ws.deserializeAttachment() as Attachment;
    const counters = this.countersFor(ws);
    this.buckets.delete(ws);
    this.counters.delete(ws);
    // A superseded connection closes while its replacement is already
    // active. The peer must only learn `peer_left` when the role really
    // emptied; otherwise the notification arrives after the replacement's
    // `peer_joined` and tears down the fresh session.
    const remaining = this.ctx
      .getWebSockets(attachment.role)
      .filter((socket) => socket !== ws).length;
    if (remaining === 0) {
      const left = JSON.stringify({ type: "peer_left" });
      for (const peer of this.ctx.getWebSockets(otherRole(attachment.role))) {
        this.trySend(peer, left);
      }
    }
    this.log({
      event: "close",
      channel: attachment.label,
      role: attachment.role,
      stage: "close",
      code,
      rx_messages: counters.rxMessages,
      rx_bytes: counters.rxBytes,
      first_forward_ms: counters.firstForwardMs,
    });
  }

  override webSocketError(ws: WebSocket): void {
    const attachment = ws.deserializeAttachment() as Attachment;
    this.buckets.delete(ws);
    this.counters.delete(ws);
    this.log({
      event: "socket_error",
      channel: attachment.label,
      role: attachment.role,
    });
  }

  override async alarm(): Promise<void> {
    if (this.ctx.getWebSockets().length === 0) {
      // Idle channel: forget pinned proofs so abandoned channels leave no
      // durable residue. Legitimate endpoints simply re-pin on next join.
      await this.ctx.storage.deleteAll();
      this.log({ event: "expired" });
      return;
    }
    await this.ctx.storage.setAlarm(Date.now() + IDLE_CHANNEL_TTL_MS);
  }

  /**
   * Sends if the socket is still live; reaps it and reports false if not.
   * Hibernatable sockets can be dead (silent TCP loss, no close event yet)
   * while still listed by `getWebSockets`, and `send()` then throws.
   */
  private trySend(ws: WebSocket, data: string | ArrayBuffer): boolean {
    try {
      ws.send(data);
      return true;
    } catch {
      try {
        ws.close(1011, "peer connection lost");
      } catch {
        // Close can throw on the same dead socket; nothing left to do.
      }
      return false;
    }
  }

  /** Token-bucket admission for one inbound frame. */
  private admit(ws: WebSocket, bytes: number): boolean {
    const now = Date.now();
    let bucket = this.buckets.get(ws);
    if (bucket === undefined) {
      bucket = { messages: MESSAGE_BURST, bytes: BYTE_BURST, refilledAt: now };
      this.buckets.set(ws, bucket);
    }
    const elapsedSeconds = Math.max(0, (now - bucket.refilledAt) / 1000);
    bucket.messages = Math.min(
      MESSAGE_BURST,
      bucket.messages + elapsedSeconds * MESSAGES_PER_SECOND,
    );
    bucket.bytes = Math.min(BYTE_BURST, bucket.bytes + elapsedSeconds * BYTES_PER_SECOND);
    bucket.refilledAt = now;
    if (bucket.messages < 1 || bucket.bytes < bytes) {
      return false;
    }
    bucket.messages -= 1;
    bucket.bytes -= bytes;
    return true;
  }

  private countersFor(ws: WebSocket): Counters {
    let counters = this.counters.get(ws);
    if (counters === undefined) {
      counters = { rxMessages: 0, rxBytes: 0, firstForwardMs: 0 };
      this.counters.set(ws, counters);
    }
    return counters;
  }

  /** Structured, payload-free log line. Values never include frame content. */
  private log(fields: Record<string, string | number>): void {
    console.log(JSON.stringify({ component: "relay_channel", ...fields }));
  }
}

function otherRole(role: Role): Role {
  return role === "host" ? "client" : "host";
}

/** Constant-time comparison of two equal-length hex strings. */
function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) {
    return false;
  }
  let difference = 0;
  for (let index = 0; index < a.length; index += 1) {
    difference |= a.charCodeAt(index) ^ b.charCodeAt(index);
  }
  return difference === 0;
}
