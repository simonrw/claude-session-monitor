import { describe, it, expect } from "vitest";
import { parseSseData, summarize } from "./sessions";
import type { SessionView } from "./types";

describe("parseSseData", () => {
  it("parses working session", () => {
    const raw = JSON.stringify([
      {
        session_id: "s1",
        cwd: "/home/user/project",
        status: { type: "working", tool: "Bash" },
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
    expect(result[0].status).toEqual({ type: "working", tool: "Bash" });
  });

  it("parses waiting session with permission reason", () => {
    const raw = JSON.stringify([
      {
        session_id: "s2",
        cwd: "/tmp",
        status: { type: "waiting", reason: "permission", detail: null },
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
      reason: "permission",
      detail: null,
    });
  });

  it("parses empty array", () => {
    expect(parseSseData("[]")).toEqual([]);
  });
});

describe("summarize", () => {
  it("counts working, waiting input, waiting permission", () => {
    const sessions: SessionView[] = [
      session({ status: { type: "working", tool: null } }),
      session({ status: { type: "working", tool: "Bash" } }),
      session({ status: { type: "waiting", reason: "input", detail: null } }),
      session({
        status: { type: "waiting", reason: "permission", detail: null },
      }),
      session({
        status: { type: "waiting", reason: "permission", detail: "rm -rf" },
      }),
    ];

    const counts = summarize(sessions);
    expect(counts).toEqual({
      working: 2,
      waitingInput: 1,
      waitingPermission: 2,
    });
  });

  it("returns zeros for empty array", () => {
    expect(summarize([])).toEqual({
      working: 0,
      waitingInput: 0,
      waitingPermission: 0,
    });
  });

  it("ignores ended sessions", () => {
    const sessions: SessionView[] = [
      session({ status: { type: "ended" } }),
      session({ status: { type: "working", tool: null } }),
    ];

    const counts = summarize(sessions);
    expect(counts.working).toBe(1);
    expect(counts.waitingInput).toBe(0);
    expect(counts.waitingPermission).toBe(0);
  });
});

function session(overrides: Partial<SessionView> = {}): SessionView {
  return {
    session_id: crypto.randomUUID(),
    cwd: "/tmp",
    status: { type: "working", tool: null },
    updated_at: "2026-04-26T10:00:00Z",
    hostname: null,
    git_branch: null,
    git_remote: null,
    tmux_target: null,
    ...overrides,
  };
}
