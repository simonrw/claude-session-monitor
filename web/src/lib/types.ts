export type BusyStatus = {
  type: "busy";
  tool: string | null;
};

export type ShellStatus = {
  type: "shell";
};

export type IdleStatus = {
  type: "idle";
};

export type WaitingStatus = {
  type: "waiting";
  detail: string | null;
};

export type EndedStatus = {
  type: "ended";
};

export type Status =
  | BusyStatus
  | ShellStatus
  | IdleStatus
  | WaitingStatus
  | EndedStatus;

export type AgentKind = "claude" | "codex";

export type SessionView = {
  session_id: string;
  agent_kind: AgentKind;
  model?: string | null;
  cwd: string;
  status: Status;
  updated_at: string;
  hostname: string | null;
  git_branch: string | null;
  git_remote: string | null;
  tmux_target: string | null;
};

// Mirrors `common::api::HostStatus`. Lets the client distinguish "no host
// has ever reported" from "genuinely zero sessions right now" - see
// `useHostStatus` in `hooks/use-sessions.ts`.
export type HostStatus = {
  hostname: string;
  agent_kind: AgentKind;
  last_seen_at: string;
};
