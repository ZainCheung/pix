// Behavior tests for the content-blind Pix relay.
//
// Most tests run on their own random channel so WebSocket state never leaks
// between cases. The Rust-generated cross-language fixture pins the exact
// identifiers and join proofs pix-wire derives, so derivation drift between
// Rust, Swift, and the relay contract fails here first.

import {
  env,
  evictDurableObject,
  runDurableObjectAlarm,
  runInDurableObject,
  SELF,
} from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import fixture from "../../protocol/fixtures/v1/relay-channel.json";
import { MAX_MESSAGE_BYTES, MESSAGES_PER_SECOND, MESSAGE_BURST } from "../src/limits";

type Role = "host" | "client";

interface Channel {
  id: string;
  hostProof: string;
  clientProof: string;
}

function randomHex(bytes: number): string {
  const buffer = crypto.getRandomValues(new Uint8Array(bytes));
  return [...buffer].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function randomChannel(): Channel {
  return {
    id: randomHex(32),
    hostProof: randomHex(32),
    clientProof: randomHex(32),
  };
}

function channelUrl(channel: Channel): string {
  return `https://relay.test/v1/channel/${channel.id}`;
}

async function join(
  channel: Channel,
  role: Role,
  overrides: Record<string, string> = {},
  url: string = channelUrl(channel),
): Promise<Response> {
  return SELF.fetch(url, {
    headers: {
      Upgrade: "websocket",
      "X-Pix-Protocol": "1",
      "X-Pix-Role": role,
      "X-Pix-Join-Proof": role === "host" ? channel.hostProof : channel.clientProof,
      ...overrides,
    },
  });
}

async function joinAccepted(channel: Channel, role: Role): Promise<WebSocket> {
  const response = await join(channel, role);
  expect(response.status).toBe(101);
  const socket = response.webSocket;
  if (socket === null) {
    throw new Error("upgrade did not return a WebSocket");
  }
  socket.accept();
  // The client-side default is Blob delivery; the tests inspect raw bytes.
  socket.binaryType = "arraybuffer";
  return socket;
}

interface Received {
  messages: (string | ArrayBuffer)[];
  closes: { code: number; reason: string }[];
}

function record(socket: WebSocket): Received {
  const received: Received = { messages: [], closes: [] };
  socket.addEventListener("message", (event) => {
    received.messages.push(event.data);
  });
  socket.addEventListener("close", (event) => {
    received.closes.push({ code: event.code, reason: event.reason });
  });
  return received;
}

/** Polls until the condition holds; loopback delivery is fast but async. */
async function waitFor(condition: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) {
      throw new Error("timed out waiting for condition");
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

async function settle(turns = 20): Promise<void> {
  for (let turn = 0; turn < turns; turn += 1) {
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}

function channelStub(channel: Channel): DurableObjectStub {
  return env.RELAY_CHANNEL.get(env.RELAY_CHANNEL.idFromName(channel.id));
}

/** Waits until the Durable Object has exactly `count` live sockets. */
async function waitForSocketCount(channel: Channel, count: number): Promise<void> {
  const deadline = Date.now() + 2000;
  for (;;) {
    const sockets = await runInDurableObject(channelStub(channel), (_instance, state) =>
      state.getWebSockets().length,
    );
    if (sockets === count) {
      return;
    }
    if (Date.now() > deadline) {
      throw new Error(`expected ${count} sockets, still ${sockets}`);
    }
    await settle(5);
  }
}

function textMessages(received: Received): string[] {
  return received.messages.filter(
    (message): message is string => typeof message === "string",
  );
}

function binaryMessages(received: Received): number[][] {
  return received.messages
    .filter((message): message is ArrayBuffer => typeof message !== "string")
    .map((buffer) => [...new Uint8Array(buffer)]);
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("upgrade validation", () => {
  it("rejects plain requests without an upgrade", async () => {
    const channel = randomChannel();
    const response = await SELF.fetch(channelUrl(channel), {
      headers: {
        "X-Pix-Protocol": "1",
        "X-Pix-Role": "host",
        "X-Pix-Join-Proof": channel.hostProof,
      },
    });
    expect(response.status).toBe(426);
  });

  it("rejects unknown paths and malformed channel identifiers", async () => {
    const channel = randomChannel();
    for (const path of [
      "/",
      "/v1/channel",
      "/v1/channel/not-hex",
      `/v1/channel/${channel.id}extra`,
      `/v2/channel/${channel.id}`,
    ]) {
      const response = await join(channel, "host", {}, `https://relay.test${path}`);
      expect(response.status, path).toBe(404);
    }
  });

  it("rejects unsupported protocol versions", async () => {
    const response = await join(randomChannel(), "host", { "X-Pix-Protocol": "2" });
    expect(response.status).toBe(400);
  });

  it("rejects invalid roles", async () => {
    const response = await join(randomChannel(), "host", { "X-Pix-Role": "admin" });
    expect(response.status).toBe(400);
  });

  it("rejects malformed join proofs", async () => {
    const channel = randomChannel();
    for (const proof of ["", "short", "Z".repeat(64), channel.hostProof + "00"]) {
      const response = await join(channel, "host", { "X-Pix-Join-Proof": proof });
      expect(response.status, proof).toBe(400);
    }
  });

  it("accepts the exact identifiers and proofs pix-wire derives", async () => {
    // Cross-language contract: these values were generated by Rust from the
    // fixture channel secret and are what the host and iOS clients present.
    const channel: Channel = {
      id: fixture.channel_id,
      hostProof: fixture.host_join_proof,
      clientProof: fixture.client_join_proof,
    };
    const host = await joinAccepted(channel, "host");
    const client = await joinAccepted(channel, "client");
    host.close(1000, "done");
    client.close(1000, "done");
  });
});

describe("join proofs", () => {
  it("pins the first proof per role and rejects strangers afterwards", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    host.close(1000, "done");
    await waitForSocketCount(channel, 0);

    const stranger = await join(channel, "host", {
      "X-Pix-Join-Proof": "ab".repeat(32),
    });
    expect(stranger.status).toBe(403);

    // The legitimate proof still joins.
    const again = await join(channel, "host");
    expect(again.status).toBe(101);
    again.webSocket?.accept();
    again.webSocket?.close(1000, "done");
  });

  it("pins host and client proofs independently", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const wrongClient = await join(channel, "client", {
      "X-Pix-Join-Proof": channel.hostProof,
    });
    expect(wrongClient.status).toBe(101);
    // The first client join pinned the proof it presented, so the real
    // client proof is rejected: channel misuse burns the channel instead of
    // silently admitting a second identity.
    const realClient = await join(channel, "client");
    expect(realClient.status).toBe(403);
    host.close(1000, "done");
    wrongClient.webSocket?.accept();
    wrongClient.webSocket?.close(1000, "done");
  });
});

describe("cardinality", () => {
  it("supersedes an older connection of the same role", async () => {
    const channel = randomChannel();
    const first = await joinAccepted(channel, "client");
    const firstReceived = record(first);
    const second = await joinAccepted(channel, "client");
    await waitFor(() => firstReceived.closes.length === 1);

    expect(firstReceived.closes).toEqual([
      { code: 4008, reason: "superseded by a newer connection" },
    ]);
    second.close(1000, "done");
  });

  it("does not announce peer_left to the peer when a connection is superseded", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const hostReceived = record(host);
    const first = await joinAccepted(channel, "client");
    await waitFor(() => textMessages(hostReceived).length === 1);

    const second = await joinAccepted(channel, "client");
    const secondReceived = record(second);
    await waitForSocketCount(channel, 2);
    await settle();

    // The host must see the replacement join, never a spurious departure
    // that would tear down the fresh session.
    const seen = textMessages(hostReceived);
    expect(seen.filter((m) => m.includes("peer_left"))).toEqual([]);
    expect(seen.filter((m) => m.includes("peer_joined")).length).toBeGreaterThanOrEqual(1);

    // Frames still flow on the new connection.
    const frame = new Uint8Array([0, 0, 0, 1, 0x07]);
    host.send(frame);
    await waitFor(() => binaryMessages(secondReceived).length === 1);
    host.close(1000, "done");
    second.close(1000, "done");
  });

  it("keeps at most one host plus one client connected", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const client = await joinAccepted(channel, "client");
    const third = await joinAccepted(channel, "host");
    await waitForSocketCount(channel, 2);

    host.close(1000, "done");
    client.close(1000, "done");
    third.close(1000, "done");
  });
});

describe("forwarding", () => {
  it("announces peer_joined to both sides and peer_left on disconnect", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    const clientReceived = record(client);
    await settle();
    expect(clientReceived.messages).toEqual([]);

    const host = await joinAccepted(channel, "host");
    const hostReceived = record(host);
    await waitFor(
      () =>
        textMessages(clientReceived).length === 1 &&
        textMessages(hostReceived).length === 1,
    );
    expect(textMessages(clientReceived)).toEqual([
      JSON.stringify({ type: "peer_joined" }),
    ]);
    expect(textMessages(hostReceived)).toEqual([
      JSON.stringify({ type: "peer_joined" }),
    ]);

    host.close(1000, "done");
    await waitFor(() => textMessages(clientReceived).length === 2);
    expect(textMessages(clientReceived)[1]).toBe(JSON.stringify({ type: "peer_left" }));
    client.close(1000, "done");
  });

  it("forwards opaque binary frames byte-for-byte in both directions", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const client = await joinAccepted(channel, "client");
    const hostReceived = record(host);
    const clientReceived = record(client);

    const fromClient = new Uint8Array([0, 0, 0, 4, 0xde, 0xad, 0xbe, 0xef]);
    const fromHost = new Uint8Array([0, 0, 0, 2, 0x13, 0x37]);
    client.send(fromClient);
    host.send(fromHost);
    await waitFor(
      () =>
        binaryMessages(hostReceived).length === 1 &&
        binaryMessages(clientReceived).length === 1,
    );

    expect(binaryMessages(hostReceived)).toEqual([[...fromClient]]);
    expect(binaryMessages(clientReceived)).toEqual([[...fromHost]]);
    host.close(1000, "done");
    client.close(1000, "done");
  });

  it("drops frames sent while the peer is absent instead of queueing", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    client.send(new Uint8Array([0, 0, 0, 1, 0x42]));
    await settle();

    const host = await joinAccepted(channel, "host");
    const hostReceived = record(host);
    await settle();
    expect(binaryMessages(hostReceived)).toEqual([]);
    host.close(1000, "done");
    client.close(1000, "done");
  });
});

describe("limits", () => {
  it("closes connections that send oversized frames", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    const received = record(client);
    client.send(new Uint8Array(MAX_MESSAGE_BYTES + 1));
    await waitFor(() => received.closes.length === 1);

    expect(received.closes.map((close) => close.code)).toEqual([1009]);
  });

  it("closes connections that send text frames", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    const received = record(client);
    client.send("not allowed");
    await waitFor(() => received.closes.length === 1);

    expect(received.closes.map((close) => close.code)).toEqual([1008]);
  });

  it("rate limits message floods", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    const received = record(client);
    const frame = new Uint8Array([0, 0, 0, 1, 0x01]);
    // Enough past the burst that refill during delivery cannot absorb it.
    const flood = MESSAGE_BURST + MESSAGES_PER_SECOND * 2;
    for (let index = 0; index < flood; index += 1) {
      client.send(frame);
    }
    await waitFor(() => received.closes.length === 1, 5000);

    expect(received.closes.map((close) => close.code)).toEqual([4013]);
  });
});

describe("dead peer sockets", () => {
  it("joins successfully even when the listed peer socket is already closing", async () => {
    const channel = randomChannel();
    const client = await joinAccepted(channel, "client");
    await waitForSocketCount(channel, 1);

    // Close the server-side client socket from inside the Durable Object.
    // A hibernated socket whose TCP died silently behaves the same way at
    // join time: still listed, but send() throws. That must never 500 the
    // next join of the channel.
    await runInDurableObject(channelStub(channel), (_instance, state) => {
      for (const socket of state.getWebSockets("client")) {
        socket.close(1000, "simulated silent death");
      }
    });

    const host = await join(channel, "host");
    expect(host.status).toBe(101);
    host.webSocket?.accept();
    host.webSocket?.close(1000, "done");
    client.close(1000, "done");
  });
});

describe("hibernation", () => {
  it("keeps forwarding after the Durable Object is evicted", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const client = await joinAccepted(channel, "client");
    const hostReceived = record(host);
    await waitForSocketCount(channel, 2);

    await evictDurableObject(channelStub(channel), { webSockets: "hibernate" });

    const frame = new Uint8Array([0, 0, 0, 3, 1, 2, 3]);
    client.send(frame);
    await waitFor(() => binaryMessages(hostReceived).length === 1);
    expect(binaryMessages(hostReceived)).toEqual([[...frame]]);

    // The pinned proofs survived eviction too.
    const pinned = await runInDurableObject(channelStub(channel), (_instance, state) =>
      state.storage.get<string>("proof:host"),
    );
    expect(pinned).toBe(channel.hostProof);
    host.close(1000, "done");
    client.close(1000, "done");
  });
});

describe("expiry", () => {
  it("clears pinned proofs once an idle channel's alarm fires", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    host.close(1000, "done");
    await waitForSocketCount(channel, 0);

    const fired = await runDurableObjectAlarm(channelStub(channel));
    expect(fired).toBe(true);
    const remaining = await runInDurableObject(channelStub(channel), (_instance, state) =>
      state.storage.list(),
    );
    expect(remaining.size).toBe(0);
  });

  it("keeps proofs while a connection is still active", async () => {
    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const fired = await runDurableObjectAlarm(channelStub(channel));
    expect(fired).toBe(true);
    const pinned = await runInDurableObject(channelStub(channel), (_instance, state) =>
      state.storage.get<string>("proof:host"),
    );
    expect(pinned).toBe(channel.hostProof);
    host.close(1000, "done");
  });
});

describe("observability stays payload-free", () => {
  it("never logs forwarded frame content, proofs, or full channel identifiers", async () => {
    const logs: string[] = [];
    const spy = vi.spyOn(console, "log").mockImplementation((...args) => {
      logs.push(args.map(String).join(" "));
    });

    const channel = randomChannel();
    const host = await joinAccepted(channel, "host");
    const client = await joinAccepted(channel, "client");
    const hostReceived = record(host);

    const marker = "SECRET-PLAINTEXT-MARKER";
    const framed = new TextEncoder().encode(`\u0000\u0000\u0000\u0017${marker}`);
    client.send(framed);
    await waitFor(() => binaryMessages(hostReceived).length === 1);
    client.close(1000, "done");
    host.close(1000, "done");
    await waitForSocketCount(channel, 0);

    spy.mockRestore();
    expect(logs.length).toBeGreaterThan(0);
    for (const line of logs) {
      expect(line).not.toContain(marker);
      expect(line).not.toContain(channel.hostProof);
      expect(line).not.toContain(channel.clientProof);
      expect(line).not.toContain(channel.id);
    }
    const join = logs
      .map((line) => JSON.parse(line) as { event?: string; elapsed_ms?: number; peer_present?: number })
      .find((entry) => entry.event === "join");
    expect(join?.elapsed_ms).toBeGreaterThanOrEqual(0);
    expect([0, 1]).toContain(join?.peer_present);
  });
});
