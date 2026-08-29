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
import { sessionEntryToContextMessages } from "@earendil-works/pi-coding-agent";

const BRIDGE_PROTOCOL_VERSION = 1;
const BRIDGE_EXTENSION_VERSION = 1;
const CLAIM_TIMEOUT_MS = 300;
const MAX_OUTGOING_BYTES = 8 * 1024 * 1024;
const MAX_INCOMING_BYTES = 16 * 1024 * 1024;
const MAX_COMMAND_TEXT_BYTES = 512 * 1024;
const SAFE_COMMANDS = new Set([
	"prompt",
	"abort",
	"model.list",
	"commands.list",
	"model.set",
	"thinking.set",
	"session.rename",
]);

let activeSocket;
let activeSessionId;
let activeStreamEpoch;
let active = false;
let sequence = 0;
let outgoing = [];
let outgoingBytes = 0;
let writing = false;
let desynced = false;
let agentRunning = false;
let compacting = false;
let inflightAssistant;
let activeTools = new Map();
let commandInFlight = false;

function bridgeSocketPath() {
	const configured = process.env.PIX_CONFIG;
	const configFile = configured
		? resolve(isAbsolute(configured) ? configured : join(process.cwd(), configured))
		: join(homedir(), ".config", "pix", "config.json");
	return join(dirname(configFile), "run", "tui-bridge.sock");
}

function registerPayload(ctx, event) {
	const sessionFile = ctx.sessionManager.getSessionFile();
	const payload = {
		version: BRIDGE_PROTOCOL_VERSION,
		type: "register",
		bridgeInstanceId: randomUUID(),
		extensionVersion: BRIDGE_EXTENSION_VERSION,
		sessionId: ctx.sessionManager.getSessionId(),
		cwd: ctx.sessionManager.getCwd() || ctx.cwd,
		reason: event.reason,
		capabilities: ["ownership.v1", "events.v1", "snapshot.v1", "commands.v1"],
		...(sessionFile ? { sessionFile } : {}),
	};
	return payload;
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
	agentRunning = false;
	compacting = false;
	inflightAssistant = undefined;
	activeTools = new Map();
	commandInFlight = false;
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

function enqueueOutgoing(frame) {
	const socket = activeSocket;
	if (!socket || !active || desynced) return false;
	const frameBytes = Buffer.byteLength(frame, "utf8");
	if (frameBytes > MAX_OUTGOING_BYTES || outgoingBytes + frameBytes > MAX_OUTGOING_BYTES) {
		desynced = true;
		outgoing = [];
		outgoingBytes = 0;
		socket.destroy();
		return false;
	}
	outgoing.push(frame);
	outgoingBytes += frameBytes;
	flushOutgoing();
	return true;
}

function sendEvent(eventType, payload) {
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
	if (enqueueOutgoing(frame) !== false) sequence = nextSequence;
}

function cloneJson(value) {
	if (value === undefined) return undefined;
	try {
		return JSON.parse(JSON.stringify(value));
	} catch {
		return undefined;
	}
}

function modelSnapshot(model) {
	if (!model) return undefined;
	return cloneJson({
		id: model.id,
		name: model.name,
		api: model.api,
		provider: model.provider,
		reasoning: model.reasoning,
		input: model.input,
		cost: model.cost,
		contextWindow: model.contextWindow,
		maxTokens: model.maxTokens,
		thinkingLevelMap: model.thinkingLevelMap,
	});
}

function snapshotPayload(ctx) {
	const contextEntries = ctx.sessionManager.buildContextEntries();
	const messages = contextEntries.flatMap((entry) => sessionEntryToContextMessages(entry));
	return {
		sessionId: activeSessionId ?? ctx.sessionManager.getSessionId(),
		sessionName: ctx.sessionManager.getSessionName(),
		model: modelSnapshot(ctx.model) ?? null,
		thinkingLevel: ctx.thinkingLevel ?? "off",
		isStreaming: agentRunning || !ctx.isIdle(),
		isCompacting: compacting,
		pendingMessageCount: ctx.hasPendingMessages() ? 1 : 0,
		messages: cloneJson(messages) ?? [],
		inflightAssistant: cloneJson(inflightAssistant) ?? null,
		activeTools: cloneJson([...activeTools.values()]) ?? [],
		throughSequence: sequence,
	};
}

function sendSnapshotResponse(request, ctx) {
	let response;
	try {
		response = {
			version: BRIDGE_PROTOCOL_VERSION,
			type: "response",
			requestId: request.requestId,
			sessionId: activeSessionId,
			command: "snapshot",
			success: true,
			snapshot: snapshotPayload(ctx),
		};
	} catch {
		response = {
			version: BRIDGE_PROTOCOL_VERSION,
			type: "response",
			requestId: request.requestId,
			sessionId: activeSessionId,
			command: "snapshot",
			success: false,
			error: "snapshot_unavailable",
		};
	}
	const frame = `${JSON.stringify(response)}\n`;
	enqueueOutgoing(frame);
}

function commandText(value, field) {
	if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > MAX_COMMAND_TEXT_BYTES) {
		throw new Error(`invalid_${field}`);
	}
	return value;
}

function commandPayload(request) {
	if (!request.payload || typeof request.payload !== "object" || Array.isArray(request.payload)) {
		throw new Error("invalid_payload");
	}
	return request.payload;
}

function scopedModels(ctx) {
	return Array.isArray(ctx.scopedModels) ? ctx.scopedModels : [];
}

function modelList(ctx) {
	const scope = scopedModels(ctx);
	const models = scope.length > 0
		? scope.map((scoped) => scoped.model)
		: ctx.modelRegistry.getAvailable();
	return {
		models: models.map(modelSnapshot).filter((model) => model !== undefined),
	};
}

function commandList(pi) {
	return {
		commands: pi.getCommands().map((command) => ({
			name: command.name,
			description: command.description,
			source: command.source,
			sourceInfo: command.sourceInfo ? { scope: command.sourceInfo.scope } : undefined,
		})),
	};
}

function modelInScope(ctx, provider, modelId) {
	const scope = scopedModels(ctx);
	return scope.length === 0 || scope.some(
		(scoped) => scoped.model.provider === provider && scoped.model.id === modelId,
	);
}

function sendCommandResponse(request, success, result, error) {
	const response = {
		version: BRIDGE_PROTOCOL_VERSION,
		type: "response",
		requestId: request.requestId,
		sessionId: activeSessionId,
		command: request.command,
		success,
		...(result !== undefined ? { result: cloneJson(result) ?? null } : {}),
		...(error ? { error } : {}),
	};
	let frame;
	try {
		frame = `${JSON.stringify(response)}\n`;
	} catch {
		frame = `${JSON.stringify({
			version: BRIDGE_PROTOCOL_VERSION,
			type: "response",
			requestId: request.requestId,
			sessionId: activeSessionId,
			command: request.command,
			success: false,
			error: "command_unavailable",
		})}\n`;
	}
	enqueueOutgoing(frame);
}

async function handleCommand(request, ctx, pi) {
	if (!SAFE_COMMANDS.has(request.command)) throw new Error("unsupported_command");
	switch (request.command) {
		case "prompt": {
			if (!ctx.isIdle()) throw new Error("session_busy");
			const payload = commandPayload(request);
			const content = commandText(payload.content, "content");
			if (payload.images !== undefined) throw new Error("images_not_supported");
			// The ExtensionAPI intentionally exposes this as fire-and-forget. The
			// subsequent user/message lifecycle events are the durable acceptance
			// signal; this response only acknowledges dispatch to Pi.
			pi.sendUserMessage(content);
			return { status: "accepted" };
		}
		case "abort":
			ctx.abort();
			return { status: "accepted" };
		case "model.list":
			return modelList(ctx);
		case "commands.list":
			return commandList(pi);
		case "model.set": {
			const payload = commandPayload(request);
			const provider = commandText(payload.provider, "provider");
			const modelId = commandText(payload.modelId, "model_id");
			if (!modelInScope(ctx, provider, modelId)) throw new Error("model_not_in_scope");
			const model = ctx.modelRegistry.find(provider, modelId);
			if (!model || !(await pi.setModel(model))) throw new Error("model_unavailable");
			return { provider: model.provider, id: model.id, name: model.name };
		}
		case "thinking.set": {
			const payload = commandPayload(request);
			const level = commandText(payload.level, "thinking_level");
			if (!["off", "minimal", "low", "medium", "high", "xhigh", "max"].includes(level)) {
				throw new Error("thinking_level_invalid");
			}
			pi.setThinkingLevel(level);
			return { level: pi.getThinkingLevel() };
		}
		case "session.rename": {
			const payload = commandPayload(request);
			const name = commandText(payload.name, "name");
			pi.setSessionName(name);
			return { name: ctx.sessionManager.getSessionName() ?? null };
		}
		default:
			throw new Error("unsupported_command");
	}
}

function handleRequest(request, ctx, pi) {
	if (
		request.sessionId !== activeSessionId ||
		typeof request.requestId !== "string" ||
		!/^[-0-9a-f]{36}$/i.test(request.requestId)
	) {
		activeSocket?.destroy();
		return;
	}
	if (request.command === "snapshot") {
		sendSnapshotResponse(request, ctx);
		return;
	}
	if (commandInFlight) {
		sendCommandResponse(request, false, undefined, "command_busy");
		return;
	}
	commandInFlight = true;
	void handleCommand(request, ctx, pi)
		.then((result) => sendCommandResponse(request, true, result, undefined))
		.catch((error) => sendCommandResponse(
			request,
			false,
			undefined,
			error instanceof Error && error.message ? error.message : "command_unavailable",
		))
		.finally(() => {
			commandInFlight = false;
		});
}

function claim(pi, ctx, event) {
	const payload = registerPayload(ctx, event);

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
			if (Buffer.byteLength(buffer, "utf8") > MAX_INCOMING_BYTES) {
				socket.destroy();
				return;
			}
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
				if (response.type === "request") {
					handleRequest(response, ctx, pi);
					continue;
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
					agentRunning = false;
					compacting = false;
					inflightAssistant = undefined;
					activeTools = new Map();
					commandInFlight = false;
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
							agentRunning = false;
							compacting = false;
							inflightAssistant = undefined;
							activeTools = new Map();
							commandInFlight = false;
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

		const result = await claim(pi, ctx, event);
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

	pi.on("agent_start", () => {
		agentRunning = true;
		sendEvent("agent_start", {});
	});
	pi.on("agent_settled", () => {
		agentRunning = false;
		sendEvent("agent_settled", {});
	});
	pi.on("message_start", (event) =>
		sendEvent("message_start", { message: event.message }),
	);
	pi.on("message_update", (event) => {
		inflightAssistant = event.message;
		sendEvent("message_update", {
			message: event.message,
			assistantMessageEvent: event.assistantMessageEvent,
		});
	});
	pi.on("message_end", (event) => {
		inflightAssistant = undefined;
		sendEvent("message_end", { message: event.message });
	});
	pi.on("tool_execution_start", (event) => {
		activeTools.set(event.toolCallId, {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
		});
		sendEvent("tool_execution_start", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
		});
	});
	pi.on("tool_execution_update", (event) => {
		activeTools.set(event.toolCallId, {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
			partialResult: event.partialResult,
		});
		sendEvent("tool_execution_update", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			args: event.args,
			partialResult: event.partialResult,
		});
	});
	pi.on("tool_execution_end", (event) => {
		activeTools.delete(event.toolCallId);
		sendEvent("tool_execution_end", {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			result: event.result,
			isError: event.isError,
		});
	});
	pi.on("session_before_compact", (event) => {
		compacting = true;
		sendEvent("compaction_start", { reason: event.reason });
	});
	pi.on("session_compact", (event) => {
		compacting = false;
		sendEvent("compaction_end", {
			reason: event.reason,
			result: event.compactionEntry,
		});
	});

	pi.on("session_shutdown", () => {
		closeSocket();
	});
}
