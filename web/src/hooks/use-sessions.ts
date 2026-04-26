import { useEffect, useRef, useState } from "react";
import type { SessionView } from "@/lib/types";
import { parseSseData } from "@/lib/sessions";

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
