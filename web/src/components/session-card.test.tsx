import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SessionCard } from "./session-card";
import type { SessionView } from "@/lib/types";

describe("SessionCard", () => {
  it("shows agent monograms and optional model text", () => {
    const html = renderToStaticMarkup(
      <>
        <SessionCard
          session={session({
            agent_kind: "codex",
            model: "gpt-5-codex",
            cwd: "/work/codex-project",
          })}
          onDelete={() => undefined}
        />
        <SessionCard
          session={session({
            agent_kind: "claude",
            cwd: "/work/claude-project",
          })}
          onDelete={() => undefined}
        />
      </>,
    );

    expect(html).toContain(">X<");
    expect(html).toContain("gpt-5-codex");
    expect(html).toContain(">C<");
    expect(html).not.toContain("undefined");
    expect(html).not.toContain("null");
  });
});

function session(overrides: Partial<SessionView> = {}): SessionView {
  return {
    session_id: crypto.randomUUID(),
    agent_kind: "claude",
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
