import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import csmSessionMonitor from "./csm-session-monitor.ts";

function harness() {
  const handlers = new Map();
  let name;
  let id = "pi-session-1";
  const pi = {
    on(event, handler) {
      const registered = handlers.get(event) ?? [];
      registered.push(handler);
      handlers.set(event, registered);
    },
    getSessionName() {
      return name;
    },
  };
  csmSessionMonitor(pi);
  const ctx = {
    cwd: "/tmp/pi-project",
    sessionManager: { getSessionId: () => id },
  };
  return {
    set id(value) { id = value; },
    set name(value) { name = value; },
    async emit(event, payload = {}) {
      for (const handler of handlers.get(event) ?? []) {
        await handler(payload, ctx);
      }
    },
  };
}

test("extension maintains the registry contract across lifecycle events", async () => {
  const root = await mkdtemp(join(tmpdir(), "csm-pi-extension-"));
  const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
  process.env.PI_CODING_AGENT_DIR = root;
  const registry = join(root, "csm", "sessions", `${process.pid}.json`);
  try {
    const extension = harness();
    await extension.emit("session_start", { reason: "startup" });

    const started = JSON.parse(await readFile(registry, "utf8"));
    assert.deepEqual(Object.keys(started).sort(), [
      "cwd", "entrypoint", "kind", "name", "pid", "procStart", "sessionId",
      "startedAt", "status", "statusUpdatedAt", "updatedAt", "version",
    ].sort());
    assert.equal(started.pid, process.pid);
    assert.equal(started.sessionId, "pi-session-1");
    assert.equal(started.cwd, "/tmp/pi-project");
    assert.equal(started.kind, "interactive");
    assert.equal(started.status, "idle");
    assert.equal(started.name, null);
    assert.equal(started.version, "pi");
    assert.match(started.procStart, /^[A-Z][a-z]{2} [A-Z][a-z]{2} \d{2} \d{2}:\d{2}:\d{2} \d{4}$/);

    await extension.emit("turn_start");
    assert.equal(JSON.parse(await readFile(registry, "utf8")).status, "busy");
    await extension.emit("tool_execution_update");
    assert.equal(JSON.parse(await readFile(registry, "utf8")).status, "busy");
    await extension.emit("turn_end");
    assert.equal(JSON.parse(await readFile(registry, "utf8")).status, "idle");

    await extension.emit("session_info_changed", { name: "  Pi monitor  " });
    assert.equal(JSON.parse(await readFile(registry, "utf8")).name, "Pi monitor");

    extension.id = "pi-session-resumed";
    extension.name = "Resumed";
    await extension.emit("session_start", { reason: "resume" });
    const resumed = JSON.parse(await readFile(registry, "utf8"));
    assert.equal(resumed.sessionId, "pi-session-resumed");
    assert.equal(resumed.name, "Resumed");

    await extension.emit("session_shutdown", { reason: "quit" });
    await assert.rejects(readFile(registry, "utf8"), { code: "ENOENT" });
  } finally {
    if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
    else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
    await rm(root, { recursive: true, force: true });
  }
});

test("an unwritable registry disables itself after one warning without throwing", async () => {
  const root = await mkdtemp(join(tmpdir(), "csm-pi-extension-failure-"));
  const notDirectory = join(root, "not-a-directory");
  await writeFile(notDirectory, "file");
  const oldAgentDir = process.env.PI_CODING_AGENT_DIR;
  const oldWarn = console.warn;
  const warnings = [];
  process.env.PI_CODING_AGENT_DIR = notDirectory;
  console.warn = (message) => warnings.push(message);
  try {
    const extension = harness();
    await extension.emit("session_start");
    await extension.emit("turn_start");
    await extension.emit("tool_execution_update");
    await extension.emit("session_start", { reason: "resume" });
    await extension.emit("session_shutdown");
    assert.equal(warnings.length, 1);
  } finally {
    console.warn = oldWarn;
    if (oldAgentDir === undefined) delete process.env.PI_CODING_AGENT_DIR;
    else process.env.PI_CODING_AGENT_DIR = oldAgentDir;
    await rm(root, { recursive: true, force: true });
  }
});
