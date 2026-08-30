#!/usr/bin/env node

/**
 * Pre-implementation compatibility probe for the Pi TUI bridge.
 *
 * This is deliberately not part of the Pix runtime. It imports Pi's exported
 * SessionManager projection helpers from an installed package and compares the
 * naive getBranch() message filter with the context returned by the same
 * source-level projection used by AgentSession.messages.
 *
 * Usage:
 *   PI_CODING_AGENT_PACKAGE_ROOT=/path/to/@earendil-works/pi-coding-agent \
 *     node scripts/pi-session-snapshot-compare.mjs
 *
 * Add PI_RPC_COMPARE=1 and PI_CODING_AGENT_EXECUTABLE=/path/to/pi to run the
 * same fixtures through a real Pi RPC process and compare get_entries against
 * get_messages.
 */

import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";
import path from "node:path";

const packageRoot = process.env.PI_CODING_AGENT_PACKAGE_ROOT;
if (!packageRoot) {
  throw new Error(
    "Set PI_CODING_AGENT_PACKAGE_ROOT to an installed @earendil-works/pi-coding-agent package",
  );
}

const packageJson = JSON.parse(
  await readFile(path.join(packageRoot, "package.json"), "utf8"),
);
const sessionManagerUrl = pathToFileURL(
  path.join(packageRoot, "dist/core/session-manager.js"),
).href;
const {
  SessionManager,
  buildContextEntries,
  sessionEntryToContextMessages,
} = await import(sessionManagerUrl);

const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const message = (role, content) => ({ role, content });

function collect(name, build) {
  const manager = SessionManager.inMemory(process.cwd());
  const ids = build(manager);
  const branch = manager.getBranch();
  const contextEntries = manager.buildContextEntries();
  const naiveMessages = branch
    .filter((entry) => entry.type === "message")
    .map((entry) => entry.message);
  const contextProjection = contextEntries.flatMap(sessionEntryToContextMessages);
  const rpcMessages = manager.buildSessionContext().messages;
  const result = {
    name,
    ids,
    branchTypes: branch.map((entry) => entry.type),
    contextEntryTypes: contextEntries.map((entry) => entry.type),
    naiveEqualsRpc: equal(naiveMessages, rpcMessages),
    projectionEqualsRpc: equal(contextProjection, rpcMessages),
    naiveMessages,
    rpcMessages,
  };
  assert.equal(
    result.projectionEqualsRpc,
    true,
    `${name}: canonical context projection diverged from buildSessionContext`,
  );
  return result;
}

const fixtures = [
  collect("plain", (manager) => {
    const user = manager.appendMessage(message("user", "hello"));
    const assistant = manager.appendMessage(message("assistant", "world"));
    return { user, assistant };
  }),
  collect("current-branch", (manager) => {
    const firstUser = manager.appendMessage(message("user", "first"));
    manager.appendMessage(message("assistant", "first answer"));
    manager.branch(firstUser);
    const secondUser = manager.appendMessage(message("user", "second"));
    const secondAssistant = manager.appendMessage(message("assistant", "second answer"));
    return { firstUser, secondUser, secondAssistant };
  }),
  collect("compaction", (manager) => {
    manager.appendMessage(message("user", "old user"));
    manager.appendMessage(message("assistant", "old answer"));
    const firstKept = manager.appendMessage(message("user", "kept user"));
    manager.appendCompaction("summary of old history", firstKept, 100);
    const assistant = manager.appendMessage(message("assistant", "new answer"));
    return { firstKept, assistant };
  }),
  collect("custom-message", (manager) => {
    const user = manager.appendMessage(message("user", "hello"));
    const custom = manager.appendCustomMessageEntry("pix", "bridge note", true);
    const assistant = manager.appendMessage(message("assistant", "world"));
    return { user, custom, assistant };
  }),
  collect("branch-summary", (manager) => {
    const user = manager.appendMessage(message("user", "hello"));
    const summary = manager.branchWithSummary(user, "abandoned branch");
    const assistant = manager.appendMessage(message("assistant", "world"));
    return { user, summary, assistant };
  }),
  collect("null-content-normalization", (manager) => {
    const user = manager.appendMessage(message("user", null));
    return { user };
  }),
];

const expectedNaiveDivergence = new Set([
  "compaction",
  "custom-message",
  "branch-summary",
  "null-content-normalization",
]);
for (const fixture of fixtures) {
  assert.equal(
    fixture.naiveEqualsRpc,
    !expectedNaiveDivergence.has(fixture.name),
    `${fixture.name}: unexpected naive projection result`,
  );
}

async function runRpcFixture(name, build) {
  const root = await mkdtemp(path.join(tmpdir(), "pix-pi-rpc-"));
  const workspace = path.join(root, "workspace");
  const sessionDir = path.join(root, "sessions");
  const agentDir = path.join(root, "agent");
  const sessionFile = path.join(sessionDir, `${name}.jsonl`);
  await Promise.all([mkdir(workspace), mkdir(sessionDir), mkdir(agentDir)]);

  const manager = SessionManager.inMemory(workspace, { id: `rpc-${name}` });
  build(manager);
  const fileEntries = [manager.getHeader(), ...manager.getEntries()];
  await writeFile(
    sessionFile,
    `${fileEntries.map((entry) => JSON.stringify(entry)).join("\n")}\n`,
    { mode: 0o600 },
  );

  const executable = process.env.PI_CODING_AGENT_EXECUTABLE ?? "pi";
  const child = spawn(
    executable,
    [
      "--mode",
      "rpc",
      "--session",
      sessionFile,
      "--session-dir",
      sessionDir,
      "--offline",
      "--no-extensions",
      "--no-skills",
      "--no-prompt-templates",
      "--no-themes",
    ],
    {
      cwd: workspace,
      env: { ...process.env, PI_CODING_AGENT_DIR: agentDir },
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  const pending = new Map();
  let nextId = 1;
  let stdout = "";
  let stderr = "";
  let processClosed = false;
  const failPending = (error) => {
    for (const { reject } of pending.values()) reject(error);
    pending.clear();
  };
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
    let newline;
    while ((newline = stdout.indexOf("\n")) !== -1) {
      const line = stdout.slice(0, newline);
      stdout = stdout.slice(newline + 1);
      if (!line.trim()) continue;
      let response;
      try {
        response = JSON.parse(line);
      } catch (error) {
        failPending(new Error(`${name}: Pi emitted invalid JSON: ${error}`));
        return;
      }
      const request = response.id ? pending.get(response.id) : undefined;
      if (!request) continue;
      pending.delete(response.id);
      if (response.success === false) {
        request.reject(new Error(`${name}: Pi RPC ${response.command} failed`));
      } else {
        request.resolve(response);
      }
    }
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  child.stdin.on("error", (error) => {
    failPending(new Error(`${name}: failed to write Pi RPC request: ${error}`));
  });
  child.on("error", (error) => {
    failPending(error);
  });
  child.on("close", (code, signal) => {
    processClosed = true;
    failPending(
      new Error(
        `${name}: Pi exited before RPC response (code=${code}, signal=${signal}, stderr=${stderr.slice(0, 400)})`,
      ),
    );
  });

  const request = (command) => {
    if (processClosed) return Promise.reject(new Error(`${name}: Pi RPC process is closed`));
    const id = `pix-compare-${nextId++}`;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      child.stdin.write(`${JSON.stringify({ id, ...command })}\n`);
    });
  };
  const requestWithTimeout = async (command) => {
    let timeout;
    try {
      return await Promise.race([
        request(command),
        new Promise((_, reject) => {
          timeout = setTimeout(
            () => reject(new Error(`${name}: Pi RPC timed out`)),
            15_000,
          );
        }),
      ]);
    } finally {
      clearTimeout(timeout);
    }
  };

  try {
    const entriesResponse = await requestWithTimeout({ type: "get_entries" });
    const messagesResponse = await requestWithTimeout({ type: "get_messages" });
    const entries = entriesResponse.data?.entries;
    const messages = messagesResponse.data?.messages;
    assert.ok(Array.isArray(entries), `${name}: get_entries did not return entries`);
    assert.ok(Array.isArray(messages), `${name}: get_messages did not return messages`);
    const contextEntries = buildContextEntries(entries, entriesResponse.data?.leafId ?? null);
    const projection = contextEntries.flatMap(sessionEntryToContextMessages);
    assert.deepEqual(
      JSON.parse(JSON.stringify(projection)),
      messages,
      `${name}: buildContextEntries projection diverged from real RPC messages`,
    );
    return {
      name,
      entryTypes: entries.map((entry) => entry.type),
      messageCount: messages.length,
    };
  } finally {
    if (!processClosed) {
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("close", resolve));
    }
    await rm(root, { recursive: true, force: true });
  }
}

const rpcResults = [];
if (process.env.PI_RPC_COMPARE === "1") {
  const rpcFixtures = [
    ["plain", (manager) => {
      manager.appendMessage(message("user", "hello"));
      manager.appendMessage(message("assistant", "world"));
    }],
    ["compaction", (manager) => {
      manager.appendMessage(message("user", "old user"));
      const firstKept = manager.appendMessage(message("user", "kept user"));
      manager.appendCompaction("summary of old history", firstKept, 100);
      manager.appendMessage(message("user", "new user"));
    }],
    ["custom-message", (manager) => {
      manager.appendMessage(message("user", "hello"));
      manager.appendCustomMessageEntry("pix", "bridge note", true);
      manager.appendMessage(message("user", "world"));
    }],
    ["branch-summary", (manager) => {
      const user = manager.appendMessage(message("user", "hello"));
      manager.branchWithSummary(user, "abandoned branch");
      manager.appendMessage(message("user", "world"));
    }],
  ];
  for (const [name, build] of rpcFixtures) {
    rpcResults.push(await runRpcFixture(name, build));
  }
}

console.log(
  JSON.stringify(
    {
      piPackage: packageJson.name,
      piVersion: packageJson.version,
      canonicalProjection: "buildContextEntries + sessionEntryToContextMessages",
      rpcReference: "buildSessionContext().messages / AgentSession.messages",
      fixtures: fixtures.map(({ naiveMessages, rpcMessages, ...fixture }) => fixture),
      rpcFixtures: rpcResults,
    },
    null,
    2,
  ),
);
