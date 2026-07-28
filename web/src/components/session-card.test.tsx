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

  it("renders each of the five statuses without crashing", () => {
    const statuses: SessionView["status"][] = [
      { type: "busy", tool: "Bash" },
      { type: "shell" },
      { type: "idle" },
      { type: "waiting", detail: "Allow Bash to run rm?" },
      { type: "ended" },
    ];

    for (const status of statuses) {
      const html = renderToStaticMarkup(
        <SessionCard session={session({ status })} onDelete={() => undefined} />,
      );
      expect(html).not.toContain("undefined");
    }
  });

  it("shows the waiting detail text when present", () => {
    const html = renderToStaticMarkup(
      <SessionCard
        session={session({
          status: { type: "waiting", detail: "Allow Bash to run rm?" },
        })}
        onDelete={() => undefined}
      />,
    );
    expect(html).toContain("Allow Bash to run rm?");
  });

  it("uses the /rename name as the heading and keeps the project name visible", () => {
    const html = renderToStaticMarkup(
      <SessionCard
        session={session({ cwd: "/work/my-project", name: "captain-marvel" })}
        onDelete={() => undefined}
      />,
    );
    expect(html).toContain("captain-marvel");
    expect(html).toContain("my-project");
  });

  it("renders exactly as it does today when a session has no name", () => {
    const html = renderToStaticMarkup(
      <SessionCard
        session={session({ cwd: "/work/my-project", name: null })}
        onDelete={() => undefined}
      />,
    );
    expect(html).toContain("my-project");
    expect(html).not.toContain("undefined");
    expect(html).not.toContain("null");
  });
});

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
