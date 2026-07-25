import type { HostStatus, SessionView } from "./types";

export function parseSseData(raw: string): SessionView[] {
  return JSON.parse(raw);
}

// How long a host can go without reporting before its watcher is treated as
// having gone silent, as opposed to genuinely reporting zero sessions right
// now - see `common::api::HOST_STALE_THRESHOLD_SECS` in
// `crates/common/src/api.rs`, which this mirrors (the two can't share code
// across the Rust/TypeScript boundary, so the constant and the reasoning are
// duplicated deliberately, in one place per language, rather than left to
// drift across every call site).
//
// Chosen against the watcher's 2s default poll interval: `last_seen_at` is
// refreshed on every successful publish, changed or not, so a healthy
// watcher advances it roughly every 2s. This client only observes that
// through its own `GET /api/hosts` poll though (`HOST_STATUS_POLL_INTERVAL_MS`
// in `hooks/use-sessions.ts`, 10s), so under fully healthy operation `now -
// last_seen_at` can already read as high as ~12s with nothing wrong at all.
// 30s sits comfortably above that ceiling - a merely slow poll or one
// dropped beat never flips this - while a watcher that has genuinely gone
// silent is still caught within one more poll cycle after the threshold
// elapses, well under a minute.
export const HOST_STALE_THRESHOLD_MS = 30_000;

export function isHostStale(host: HostStatus, now: Date): boolean {
  return now.getTime() - new Date(host.last_seen_at).getTime() >= HOST_STALE_THRESHOLD_MS;
}

// Whether the empty session list should be explained as "the watcher isn't
// reporting" rather than "genuinely no sessions right now" - see
// `isHostStale`'s doc comment, and the mirrored `watcher_appears_silent` in
// `crates/gui/src/main.rs`. True when either no host has ever reported
// (`hosts` empty) or every host that has reported has gone stale as of
// `now`: a watcher that reported once and then died leaves `hosts`
// non-empty forever with a frozen `last_seen_at`, which a plain
// `hosts.length === 0` check would miss entirely.
//
// Only meaningful once `hasReceivedHostStatus` is true - before the first
// `GET /api/hosts` poll lands, an empty `hosts` is ambiguous with "haven't
// heard back yet" rather than "watcher is silent".
export function watcherAppearsSilent(
  hosts: HostStatus[],
  hasReceivedHostStatus: boolean,
  now: Date,
): boolean {
  return (
    hasReceivedHostStatus &&
    (hosts.length === 0 || hosts.every((h) => isHostStale(h, now)))
  );
}

export type SummaryCounts = {
  busy: number;
  waiting: number;
};

// Mirrors `common::view_model::MenuBarSummary::from_sessions` (see its doc
// comment in `crates/common/src/view_model.rs`): `Shell` counts as `busy`
// alongside `Busy` - a foreground shell command is not "idle", it is a
// session actively occupied - and `Idle`/`Ended` count toward neither. There
// is no more permission/input split: the registry carries no such
// distinction, so `Waiting` is a single bucket everywhere in the stack.
export function summarize(sessions: SessionView[]): SummaryCounts {
  let busy = 0;
  let waiting = 0;

  for (const s of sessions) {
    switch (s.status.type) {
      case "busy":
      case "shell":
        busy++;
        break;
      case "waiting":
        waiting++;
        break;
    }
  }

  return { busy, waiting };
}
