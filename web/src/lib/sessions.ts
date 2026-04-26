import type { SessionView } from "./types";

export function parseSseData(raw: string): SessionView[] {
  return JSON.parse(raw);
}

export type SummaryCounts = {
  working: number;
  waitingInput: number;
  waitingPermission: number;
};

export function summarize(sessions: SessionView[]): SummaryCounts {
  let working = 0;
  let waitingInput = 0;
  let waitingPermission = 0;

  for (const s of sessions) {
    switch (s.status.type) {
      case "working":
        working++;
        break;
      case "waiting":
        if (s.status.reason === "input") waitingInput++;
        else if (s.status.reason === "permission") waitingPermission++;
        break;
    }
  }

  return { working, waitingInput, waitingPermission };
}
