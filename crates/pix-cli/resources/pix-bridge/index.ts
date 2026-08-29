// Pix TUI Bridge ownership extension.
//
// This first revision intentionally implements only the ownership handshake.
// Pi remains fully usable when the host or socket is unavailable; realtime
// event forwarding is added by a later bridge protocol revision.

import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";

const BRIDGE_PROTOCOL_VERSION = 1;
const BRIDGE_EXTENSION_VERSION = 1;
const CLAIM_TIMEOUT_MS = 300;

let activeSocket;
let activeSessionId;
let active = false;

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
		capabilities: ["ownership.v1"],
	};
}

function closeSocket() {
	const socket = activeSocket;
	activeSocket = undefined;
	activeSessionId = undefined;
	active = false;
	if (socket) socket.end();
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
					active = true;
					socket.on("close", () => {
						if (activeSocket === socket) {
							activeSocket = undefined;
							activeSessionId = undefined;
							active = false;
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

	pi.on("session_shutdown", () => {
		closeSocket();
	});
}

