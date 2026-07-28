import { useEffect, useRef, useState } from "react";
import type { HostStatus, SessionView } from "@/lib/types";
import { parseSseData } from "@/lib/sessions";

const HOST_STATUS_POLL_INTERVAL_MS = 10_000;

// Polls `GET /api/hosts` (see `common::api::HostStatus` and the server route
// in `crates/server/src/lib.rs`) so the UI can distinguish "no watcher has
// ever reported" from "genuinely zero sessions right now" - the same
// PRO-211/PRO-214 distinction wired into the mac client's
// `PopoverView.emptyStateText` and iOS's `SessionListView.content`.
// `hasReceivedHostStatus` flips true after the first successful poll, even
// if that poll returns an empty list, so callers can tell "haven't heard
// back yet" from "heard back, and there are zero hosts."
export function useHostStatus() {
  const [hosts, setHosts] = useState<HostStatus[]>([]);
  const [hasReceivedHostStatus, setHasReceivedHostStatus] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const poll = async () => {
      try {
        const resp = await fetch("/api/hosts");
        if (!resp.ok) return;
        const data: HostStatus[] = await resp.json();
        if (cancelled) return;
        setHosts(data);
        setHasReceivedHostStatus(true);
      } catch {
        // Network error - leave state as-is and try again on the next tick.
      }
    };

    poll();
    const interval = setInterval(poll, HOST_STATUS_POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  return { hosts, hasReceivedHostStatus };
}

export function useSessions() {
  const [sessions, setSessions] = useState<SessionView[]>([]);
  const [connected, setConnected] = useState(false);
  const eventSourceRef = useRef<EventSource | null>(null);

  useEffect(() => {
    const es = new EventSource("/api/events");
    eventSourceRef.current = es;

    es.onopen = () => setConnected(true);

    es.onmessage = (event) => {
      setSessions(parseSseData(event.data));
    };

    es.onerror = () => {
      setConnected(false);
    };

    return () => {
      es.close();
      eventSourceRef.current = null;
    };
  }, []);

  return { sessions, connected };
}

export async function deleteSession(sessionId: string): Promise<boolean> {
  const resp = await fetch(`/api/sessions/${sessionId}`, { method: "DELETE" });
  return resp.status === 204;
}
