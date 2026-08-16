/**
 * Pix compatibility extension for Pi's context projection.
 *
 * Pi custom messages are persisted for the session and are included in the
 * next model request even when `display` is false. Extensions use that channel
 * for both status UI and real, hidden model context (for example, a plan-mode
 * instruction). Keep every extension loaded and preserve the latter. The
 * generic rule below removes only hidden messages that sit outside a user
 * prompt in the durable session tree, which is how idle status notifications
 * are appended. Hidden messages created during an active turn are retained.
 */
function isHiddenCustomMessage(message) {
  return (
    message?.role === "custom" &&
    message?.display === false
  );
}

function isHiddenCustomEntry(entry) {
  return (
    entry?.type === "custom_message" &&
    entry?.display === false
  );
}

function timestampValue(value) {
  if (typeof value === "number") return value;
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) return parsed;
  }
  return value;
}

function messageKey(message) {
  return JSON.stringify([
    message?.customType,
    timestampValue(message?.timestamp),
    message?.content,
    message?.details,
  ]);
}

function entryKey(entry) {
  return JSON.stringify([
    entry?.customType,
    timestampValue(entry?.timestamp),
    entry?.content,
    entry?.details,
  ]);
}

function followsUserPrompt(index, entries) {
  // Several hidden/visible custom messages can be emitted for one prompt.
  // Walk over that group and harmless metadata, stopping at the previous real
  // message. An assistant/tool result means the custom message was appended
  // while idle (a notification); a user message means it is prompt context.
  for (let cursor = index - 1; cursor >= 0; cursor -= 1) {
    const previous = entries[cursor];
    if (previous?.type === "custom_message") continue;
    if (previous?.type === "message") {
      return previous.message?.role === "user";
    }
  }
  return false;
}

function previousRealMessageRole(entries) {
  if (!Array.isArray(entries)) return undefined;
  for (let cursor = entries.length - 1; cursor >= 0; cursor -= 1) {
    const entry = entries[cursor];
    if (entry?.type === "custom_message") continue;
    if (entry?.type === "message") return entry.message?.role;
  }
  return undefined;
}

function notificationKeys(entries) {
  const keys = new Set();
  if (!Array.isArray(entries)) return keys;

  entries.forEach((entry, index) => {
    if (isHiddenCustomEntry(entry) && !followsUserPrompt(index, entries)) {
      keys.add(entryKey(entry));
    }
  });
  return keys;
}

function withoutHiddenNotifications(messages, hiddenNotificationKeys, activeContextKeys) {
  return Array.isArray(messages)
    ? messages.filter((message) => {
        if (!isHiddenCustomMessage(message)) return true;
        const key = messageKey(message);
        // An active-turn message may be a queued steering/follow-up prompt;
        // it must win over the tree-position fallback below.
        if (activeContextKeys.has(key)) return true;
        return !hiddenNotificationKeys.has(key);
      })
    : [];
}

export default function pixContextGuard(pi) {
  const activeContextKeys = new Set();
  const suppressedNotificationKeys = new Set();
  let activeTurn = false;
  let suppressNextAbortedAssistant = false;

  // `message_end` is emitted for extension messages that are part of an
  // agent turn. Idle `sendMessage()` notifications bypass this event, which
  // gives us a generic, API-level distinction without naming any extension.
  pi.on("agent_start", () => {
    activeTurn = true;
  });
  pi.on("turn_start", () => {
    activeTurn = true;
  });
  pi.on("message_end", (event) => {
    if (activeTurn && isHiddenCustomMessage(event.message)) {
      const key = messageKey(event.message);
      if (!suppressedNotificationKeys.has(key)) {
        activeContextKeys.add(key);
      }
      return;
    }
    // Aborting a synthetic notification turn makes Pi emit an empty assistant
    // error message. Normalize it to an aborted, empty turn so RPC consumers
    // do not render an error bubble or retain an error string in the session.
    if (
      suppressNextAbortedAssistant &&
      event.message?.role === "assistant" &&
      event.message?.stopReason === "error"
    ) {
      suppressNextAbortedAssistant = false;
      return {
        message: {
          ...event.message,
          content: [],
          stopReason: "aborted",
          errorMessage: undefined,
        },
      };
    }
  });
  pi.on("message_start", (event, context) => {
    if (!isHiddenCustomMessage(event.message)) return;
    // Pi turns a sendMessage() that arrives during streaming into a steering
    // turn. If it follows an assistant/tool result rather than a user prompt,
    // it is an idle status racing with turn shutdown; abort that synthetic
    // turn before it can produce a second assistant response.
    const entries = context?.sessionManager?.getBranch?.();
    if (!Array.isArray(entries)) return;
    const previousRole = previousRealMessageRole(entries);
    if (previousRole !== "user") {
      suppressedNotificationKeys.add(messageKey(event.message));
      suppressNextAbortedAssistant = true;
      context?.abort?.();
    }
  });
  pi.on("turn_end", () => {
    activeTurn = false;
  });
  pi.on("agent_end", () => {
    activeTurn = false;
    suppressNextAbortedAssistant = false;
  });

  function hiddenNotificationKeysFor(context) {
    return notificationKeys(context?.sessionManager?.getBranch?.());
  }

  pi.on("context", (event, context) => ({
    messages: withoutHiddenNotifications(
      event.messages,
      hiddenNotificationKeysFor(context),
      activeContextKeys,
    ),
  }));

  // Normal provider requests pass through `context`, but compaction and tree
  // summaries build their own message lists. Remove the same UI-only entries
  // from those transient preparations without changing the durable session.
  pi.on("session_before_compact", (event, context) => {
    const hiddenKeys = hiddenNotificationKeysFor(context);
    event.preparation.messagesToSummarize = withoutHiddenNotifications(
      event.preparation.messagesToSummarize,
      hiddenKeys,
      activeContextKeys,
    );
    event.preparation.turnPrefixMessages = withoutHiddenNotifications(
      event.preparation.turnPrefixMessages,
      hiddenKeys,
      activeContextKeys,
    );
  });

  pi.on("session_before_tree", (event, context) => {
    const entries = event.preparation.entriesToSummarize;
    const hiddenKeys = hiddenNotificationKeysFor(context);
    const filtered = entries.filter((entry) => {
      if (!isHiddenCustomEntry(entry)) return true;
      const key = entryKey(entry);
      return activeContextKeys.has(key) || !hiddenKeys.has(key);
    });
    entries.splice(0, entries.length, ...filtered);
  });
}
