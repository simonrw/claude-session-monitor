import { execFileSync } from "node:child_process";
import { mkdir, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type RegistryStatus = "busy" | "idle";

type RegistryEntry = {
  pid: number;
  sessionId: string;
  cwd: string;
  startedAt: number;
  procStart: string;
  version: "pi";
  kind: "interactive";
  entrypoint: "pi";
  name: string | null;
  status: RegistryStatus;
  updatedAt: number;
  statusUpdatedAt: number;
};

function agentDir(): string {
  const configured = process.env.PI_CODING_AGENT_DIR;
  if (!configured) return join(homedir(), ".pi", "agent");
  const expanded = configured === "~"
    ? homedir()
    : configured.startsWith("~/")
      ? join(homedir(), configured.slice(2))
      : configured;
  return resolve(expanded);
}

function formatProcStart(date: Date): string {
  const weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${weekdays[date.getUTCDay()]} ${months[date.getUTCMonth()]} ${pad(date.getUTCDate())} ${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())} ${date.getUTCFullYear()}`;
}

function processStart(): Date {
  try {
    const output = execFileSync(
      "ps",
      ["-o", "lstart=", "-p", String(process.pid)],
      { encoding: "utf8", timeout: 2_000 },
    ).trim();
    const started = new Date(output);
    if (!Number.isNaN(started.getTime())) return started;
  } catch {
    // Fall back to Node's OS-backed monotonic process uptime. Monitoring must
    // never prevent pi from starting merely because `ps` is unavailable.
  }
  return new Date(Date.now() - process.uptime() * 1_000);
}

/**
 * Maintain one Claude-Code-shaped registry claim for this pi process.
 * Copy this file to `~/.pi/agent/extensions/` to enable monitoring.
 */
export default function csmSessionMonitor(pi: ExtensionAPI) {
  const pid = process.pid;
  const started = processStart();
  const startedAt = started.getTime();
  const procStart = formatProcStart(started);
  let registryFile: string | undefined;
  let sessionId = "";
  let cwd = "";
  let name: string | null = null;
  let status: RegistryStatus = "idle";
  let statusUpdatedAt = Date.now();
  let disabled = false;
  let warned = false;
  let pending: Promise<void> = Promise.resolve();

  const warnOnce = (error: unknown) => {
    if (warned) return;
    warned = true;
    console.warn(`[csm-session-monitor] monitoring disabled: ${String(error)}`);
  };

  const entry = (): RegistryEntry => ({
    pid,
    sessionId,
    cwd,
    startedAt,
    procStart,
    version: "pi",
    kind: "interactive",
    entrypoint: "pi",
    name,
    status,
    updatedAt: Date.now(),
    statusUpdatedAt,
  });

  const enqueueWrite = () => {
    if (disabled || !registryFile) return pending;
    pending = pending.then(async () => {
      if (disabled || !registryFile) return;
      const sessionsDir = dirname(registryFile);
      const temporaryDir = join(dirname(sessionsDir), ".tmp");
      const temporary = join(temporaryDir, `${pid}.${process.hrtime.bigint()}.tmp`);
      try {
        await mkdir(sessionsDir, { recursive: true });
        await mkdir(temporaryDir, { recursive: true });
        await writeFile(temporary, `${JSON.stringify(entry())}\n`, "utf8");
        await rename(temporary, registryFile);
      } catch (error) {
        await rm(temporary, { force: true }).catch(() => undefined);
        disabled = true;
        warnOnce(error);
      }
    });
    return pending;
  };

  const setStatus = (next: RegistryStatus) => {
    if (status !== next) statusUpdatedAt = Date.now();
    status = next;
    return enqueueWrite();
  };

  const refreshSession = (ctx: ExtensionContext) => {
    sessionId = ctx.sessionManager.getSessionId();
    cwd = ctx.cwd;
    name = pi.getSessionName()?.trim() || null;
    registryFile = join(agentDir(), "csm", "sessions", `${pid}.json`);
  };

  pi.on("session_start", async (_event, ctx) => {
    refreshSession(ctx);
    status = "idle";
    statusUpdatedAt = Date.now();
    await enqueueWrite();
  });

  pi.on("session_info_changed", async (event) => {
    name = event.name?.trim() || null;
    await enqueueWrite();
  });

  pi.on("agent_start", async () => {
    await setStatus("busy");
  });
  pi.on("turn_start", async () => {
    await setStatus("busy");
  });
  pi.on("turn_end", async () => {
    await setStatus("idle");
  });
  pi.on("agent_end", async () => {
    await setStatus("idle");
  });
  pi.on("tool_execution_start", async () => {
    await setStatus("busy");
  });
  pi.on("tool_execution_update", async () => {
    await setStatus("busy");
  });

  pi.on("session_shutdown", async () => {
    await pending;
    if (!registryFile) return;
    try {
      await rm(registryFile, { force: true });
    } catch (error) {
      warnOnce(error);
    }
  });
}
