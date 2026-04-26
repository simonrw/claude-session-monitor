export type WorkingStatus = {
  type: "working";
  tool: string | null;
};

export type WaitingStatus = {
  type: "waiting";
  reason: "permission" | "input";
  detail: string | null;
};

export type EndedStatus = {
  type: "ended";
};

export type Status = WorkingStatus | WaitingStatus | EndedStatus;

export type SessionView = {
  session_id: string;
  cwd: string;
  status: Status;
  updated_at: string;
  hostname: string | null;
  git_branch: string | null;
  git_remote: string | null;
  tmux_target: string | null;
};
