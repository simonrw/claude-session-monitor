import { describe, it, expect } from "vitest";
import {
  HOST_STALE_THRESHOLD_MS,
  isHostStale,
  parseSseData,
  summarize,
  watcherAppearsSilent,
} from "./sessions";
import type { HostStatus, SessionView } from "./types";

describe("parseSseData", () => {
  it("parses busy session", () => {
    const raw = JSON.stringify([
      {
        session_id: "s1",
        agent_kind: "codex",
        model: "gpt-5-codex",
        cwd: "/home/user/project",
        status: { type: "busy", tool: "Bash" },
        updated_at: "2026-04-26T10:00:00Z",
        hostname: "laptop",
        git_branch: "main",
        git_remote: null,
        tmux_target: null,
      },
    ]);

    const result = parseSseData(raw);
    expect(result).toHaveLength(1);
    expect(result[0].session_id).toBe("s1");
    expect(result[0].agent_kind).toBe("codex");
    expect(result[0].model).toBe("gpt-5-codex");
    expect(result[0].status).toEqual({ type: "busy", tool: "Bash" });
  });

  it("parses waiting session with detail", () => {
    const raw = JSON.stringify([
      {
        session_id: "s2",
        agent_kind: "claude",
        cwd: "/tmp",
        status: { type: "waiting", detail: "Allow Bash to run rm?" },
        updated_at: "2026-04-26T10:00:00Z",
        hostname: null,
        git_branch: null,
        git_remote: null,
        tmux_target: null,
      },
    ]);

    const result = parseSseData(raw);
    expect(result[0].status).toEqual({
      type: "waiting",
      detail: "Allow Bash to run rm?",
    });
  });

  it("parses shell and idle sessions", () => {
    const raw = JSON.stringify([
      session({ session_id: "s3", status: { type: "shell" } }),
      session({ session_id: "s4", status: { type: "idle" } }),
    ]);

    const result = parseSseData(raw);
    expect(result[0].status).toEqual({ type: "shell" });
    expect(result[1].status).toEqual({ type: "idle" });
  });

  it("parses empty array", () => {
    expect(parseSseData("[]")).toEqual([]);
  });
});

describe("summarize", () => {
  it("counts busy and shell together, waiting separately", () => {
    const sessions: SessionView[] = [
      session({ status: { type: "busy", tool: null } }),
      session({ status: { type: "busy", tool: "Bash" } }),
      session({ status: { type: "shell" } }),
      session({ status: { type: "waiting", detail: null } }),
      session({ status: { type: "waiting", detail: "rm -rf" } }),
    ];

    const counts = summarize(sessions);
    expect(counts).toEqual({
      busy: 3,
      waiting: 2,
    });
  });

  it("returns zeros for empty array", () => {
    expect(summarize([])).toEqual({
      busy: 0,
      waiting: 0,
    });
  });

  it("ignores idle and ended sessions", () => {
    const sessions: SessionView[] = [
      session({ status: { type: "ended" } }),
      session({ status: { type: "idle" } }),
      session({ status: { type: "busy", tool: null } }),
    ];

    const counts = summarize(sessions);
    expect(counts.busy).toBe(1);
    expect(counts.waiting).toBe(0);
  });
});

describe("isHostStale", () => {
  const now = new Date("2026-04-26T10:00:00Z");

  it("is not stale when just seen", () => {
    expect(isHostStale(host(now), now)).toBe(false);
  });

  it("is not stale just under the threshold", () => {
    const lastSeenAt = new Date(now.getTime() - (HOST_STALE_THRESHOLD_MS - 1));
    expect(isHostStale(host(lastSeenAt), now)).toBe(false);
  });

  it("is stale at exactly the threshold", () => {
    const lastSeenAt = new Date(now.getTime() - HOST_STALE_THRESHOLD_MS);
    expect(isHostStale(host(lastSeenAt), now)).toBe(true);
  });

  it("is stale well past the threshold", () => {
    const lastSeenAt = new Date(now.getTime() - 5 * 60_000);
    expect(isHostStale(host(lastSeenAt), now)).toBe(true);
  });
});

describe("watcherAppearsSilent", () => {
  const now = new Date("2026-04-26T10:00:00Z");

  it("is false before the first host-status poll lands", () => {
    expect(watcherAppearsSilent([], false, now)).toBe(false);
  });

  it("is true when no host has ever reported", () => {
    expect(watcherAppearsSilent([], true, now)).toBe(true);
  });

  it("is false with a freshly-seen host", () => {
    expect(watcherAppearsSilent([host(now)], true, now)).toBe(false);
  });

  it("is true once its only host goes stale", () => {
    const stale = new Date(now.getTime() - 5 * 60_000);
    expect(watcherAppearsSilent([host(stale)], true, now)).toBe(true);
  });

  it("is false if any host is still fresh", () => {
    const stale = new Date(now.getTime() - 5 * 60_000);
    const hosts = [host(stale, "dead-host"), host(now, "live-host")];
    expect(watcherAppearsSilent(hosts, true, now)).toBe(false);
  });
});

function host(lastSeenAt: Date, hostname = "mbp"): HostStatus {
  return {
    hostname,
    agent_kind: "claude",
    last_seen_at: lastSeenAt.toISOString(),
  };
}

function session(overrides: Partial<SessionView> = {}): SessionView {
  return {
    session_id: crypto.randomUUID(),
    agent_kind: "claude",
    cwd: "/tmp",
    status: { type: "busy", tool: null },
    updated_at: "2026-04-26T10:00:00Z",
    hostname: null,
    git_branch: null,
    git_remote: null,
    tmux_target: null,
    ...overrides,
  };
}
