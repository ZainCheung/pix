// Pix TUI Bridge ownership extension.
//
// The extension owns only the host-local bridge connection. This ownership handshake
// remains optional: Pi stays usable when the host or socket is
// unavailable; event delivery is bounded and fail-closed so it can never
// stall the interactive TUI.

import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";

const BRIDGE_PROTOCOL_VERSION = 1;
const BRIDGE_EXTENSION_VERSION = 1;
const CLAIM_TIMEOUT_MS = 300;
const MAX_OUTGOING_BYTES = 8 * 1024 * 1024;

let activeSocket;
let activeSessionId;
let activeStreamEpoch;
let active = false;
let sequence = 0;
let outgoing = [];
let outgoingBytes = 0;
let writing = false;
let desynced = false;

function bridgeSocketPath() {
	const configured = process.env.PIX_CONFIG;
	const configFile = configured
		? resolve(isAbsolute(configured) ? configured : join(process.cwd(), configured))
		: join(homedir(), ".config", "pix", "config.json");
	return join(dirname(configFile), "run", "tui-bridge.sock");
}

function registerPayload(ctx, event) {
	const sessionFile = ctx.sessionManager.getSessionFile();
	if (!sessionFile) return undefined;
	return {
		version: BRIDGE_PROTOCOL_VERSION,
		type: "register",
		bridgeInstanceId: randomUUID(),
		extensionVersion: BRIDGE_EXTENSION_VERSION,
		sessionId: ctx.sessionManager.getSessionId(),
		cwd: ctx.sessionManager.getCwd() || ctx.cwd,
		sessionFile,
		reason: event.reason,
		capabilities: ["ownership.v1", "events.v1", "snapshot.v1"],
	};
}

function closeSocket() {
	const socket = activeSocket;
	activeSocket = undefined;
	activeSessionId = undefined;
	activeStreamEpoch = undefined;
	active = false;
	sequence = 0;
	outgoing = [];
	outgoingBytes = 0;
	writing = false;
	desynced = false;
	if (socket) socket.end();
}

function flushOutgoing() {
	const socket = activeSocket;
	if (!socket || writing || outgoing.length === 0) return;
	const frame = outgoing.shift();
	outgoingBytes -= Buffer.byteLength(frame, "utf8");
	writing = true;
	socket.write(frame, () => {
		writing = false;
		flushOutgoing();
	});
}

function sendEvent(eventType, payload) {
	const socket = activeSocket;
	if (!socket || !active || desynced) return;
	const nextSequence = sequence + 1;
	const frame = `${JSON.stringify({
		version: BRIDGE_PROTOCOL_VERSION,
		type: "event",
		sessionId: activeSessionId,
		streamEpoch: activeStreamEpoch,
		sequence: nextSequence,
		eventType,
		payload,
	})}\n`;
	const frameBytes = Buffer.byteLength(frame, "utf8");
	if (frameBytes > MAX_OUTGOING_BYTES || outgoingBytes + frameBytes > MAX_OUTGOING_BYTES) {
		desynced = true;
		outgoing = [];
		outgoingBytes = 0;
		socket.destroy();
		return;
	}
	sequence = nextSequence;
	outgoing.push(frame);
	outgoingBytes += frameBytes;
	flushOutgoing();
}

function claim(ctx, event) {
	const payload = registerPayload(ctx, event);
	if (!payload) return Promise.resolve({ kind: "standalone" });

	return new Promise((resolveClaim) => {
		const socket = createConnection({ path: bridgeSocketPath() });
		let buffer = "";
		let settled = false;
		let timer;
		const finish = (result) => {
			if (settled) return;
			settled = true;
			if (timer) clearTimeout(timer);
			if (result.kind !== "attached") socket.destroy();
			resolveClaim(result);
		};

		timer = setTimeout(() => finish({ kind: "standalone" }), CLAIM_TIMEOUT_MS);
		socket.setEncoding("utf8");
		socket.on("connect", () => {
			socket.write(`${JSON.stringify(payload)}\n`);
		});
		socket.on("data", (chunk) => {
			buffer += chunk;
			while (true) {
				const newline = buffer.indexOf("\n");
				if (newline < 0) return;
				const line = buffer.slice(0, newline);
				buffer = buffer.slice(newline + 1);
				if (!line.trim()) continue;
				let response;
				try {
					response = JSON.parse(line);
				} catch {
					finish({ kind: "standalone" });
					return;
				}
				if (response.type !== "register_result") continue;
				if (response.granted === true) {
					activeSocket = socket;
					activeSessionId = payload.sessionId;
					activeStreamEpoch = response.bridgeInstanceId;
					active = true;
					sequence = 0;
					outgoing = [];
					outgoingBytes = 0;
					writing = false;
					desynced = false;
					socket.on("close", () => {
						if (activeSocket === socket) {
							activeSocket = undefined;
							activeSessionId = undefined;
							activeStreamEpoch = undefined;
							active = false;
							outgoing = [];
							outgoingBytes = 0;
							writing = false;
							sequence = 0;
							desynced = false;
						}
					});
					finish({ kind: "attached", response });
				} else {
					finish({ kind: "conflict", response });
				}
				return;
			}
		});
		socket.on("error", () => finish({ kind: "standalone" }));
		socket.on("close", () => finish({ kind: "standalone" }));
	});
}

export default function pixTuiBridge(pi) {
	pi.on("session_start", async (event, ctx) => {
		if (ctx.mode !== "tui") return;

		const result = await claim(ctx, event);
		if (result.kind === "attached") {
			ctx.ui.setStatus("pix-bridge", "attached");
			return;
		}
		if (result.kind === "conflict") {
			ctx.ui.notify(
				"This session is currently active in Pix. Release it before opening it in Pi TUI.",
				"warning",
			);
			ctx.shutdown();
			return;
		}
	ctx.ui.setStatus("pix-bridge", "standalone");
	});

	pi.on("agent_start", () => sendEvent("agent_start", {}));
	pi.on("agent_settled", () => sendEvent("agent_settled", {}));
	pi.on("message_start", (event) =>
		sendEvent("message_start", { message: event.message }),
	);
	pi.on("message_update", (event) =>
		sendEvent("message_update", {
			message: event.message,
			assistantMessageEvent: event.assistantMessageEvent,
		}),
	);
	pi.on("message_end", (event) => sendEvent("message_end", { message: event.message }));
	pi.on("tool_execution_start", (event) =>
		sendEvent("tool_execution_start", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
		}),
	);
	pi.on("tool_execution_update", (event) =>
		sendEvent("tool_execution_update", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
			partialResult: event.partialResult,
		}),
	);
	pi.on("tool_execution_end", (event) =>
		sendEvent("tool_execution_end", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			result: event.result,
			isError: event.isError,
		}),
	);
	pi.on("session_before_compact", (event) =>
		sendEvent("compaction_start", { reason: event.reason }),
	);
	pi.on("session_compact", (event) =>
		sendEvent("compaction_end", {
			reason: event.reason,
			result: event.compactionEntry,
		}),
	);

	pi.on("session_shutdown", () => {
		closeSocket();
	});
}
