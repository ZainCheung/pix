import guard from "./pi-context-guard.mjs";

const handlers = new Map();
const pi = {
  on(name, handler) {
    const list = handlers.get(name) ?? [];
    list.push(handler);
    handlers.set(name, list);
  },
};
guard(pi);

const dispatch = (name, ...args) =>
  (handlers.get(name) ?? []).map((handler) => handler(...args));

const iso = (timestamp) => new Date(timestamp).toISOString();
const messageEntry = (id, timestamp, role) => ({
  type: "message",
  id,
  parentId: null,
  timestamp: iso(timestamp),
  message: { role, content: role === "user" ? "hello" : "done" },
});
const hiddenEntry = (id, timestamp, customType, content) => ({
  type: "custom_message",
  id,
  parentId: null,
  timestamp: iso(timestamp),
  customType,
  content,
  display: false,
});

const statusEntry = hiddenEntry(
  "status",
  2000,
  "arbitrary-package:status",
  "connected",
);
const branch = [messageEntry("assistant", 1000, "assistant"), statusEntry];
const context = {
  sessionManager: { getBranch: () => branch },
  abortCalled: false,
  abort() {
    this.abortCalled = true;
  },
};
const statusMessage = {
  role: "custom",
  customType: statusEntry.customType,
  content: statusEntry.content,
  display: false,
  timestamp: 2000,
};

dispatch("turn_start");
dispatch("message_start", { message: statusMessage }, context);
if (!context.abortCalled) throw new Error("queued status turn was not aborted");

dispatch("message_end", { message: statusMessage }, context);
const replacement = dispatch(
  "message_end",
  {
    message: {
      role: "assistant",
      content: [],
      stopReason: "error",
      errorMessage: "This operation was aborted",
    },
  },
  context,
).at(-1);
if (replacement?.message?.stopReason !== "aborted") {
  throw new Error("aborted notification turn was not normalized");
}
const statusProjection = dispatch(
  "context",
  { messages: [statusMessage] },
  context,
).at(-1);
if (statusProjection?.messages.length !== 0) {
  throw new Error("queued notification remained in model context");
}

const hiddenPromptEntry = hiddenEntry(
  "prompt-context",
  3000,
  "arbitrary-package:hidden-context",
  "keep this instruction",
);
const promptContext = {
  sessionManager: {
    getBranch: () => [messageEntry("user", 2500, "user"), hiddenPromptEntry],
  },
  abortCalled: false,
  abort() {
    this.abortCalled = true;
  },
};
const hiddenPrompt = {
  role: "custom",
  customType: hiddenPromptEntry.customType,
  content: hiddenPromptEntry.content,
  display: false,
  timestamp: 3000,
};
dispatch("message_start", { message: hiddenPrompt }, promptContext);
if (promptContext.abortCalled) {
  throw new Error("active-turn hidden context was aborted");
}
dispatch("message_end", { message: hiddenPrompt }, promptContext);
const promptProjection = dispatch(
  "context",
  { messages: [hiddenPrompt] },
  promptContext,
).at(-1);
if (promptProjection?.messages.length !== 1) {
  throw new Error("active-turn hidden context was removed");
}

console.log("pi-context-guard regression: ok");
