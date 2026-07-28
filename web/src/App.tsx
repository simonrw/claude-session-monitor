import { useHostStatus, useSessions, deleteSession } from "@/hooks/use-sessions";
import { summarize, watcherAppearsSilent } from "@/lib/sessions";
import { SummaryBar } from "@/components/summary-bar";
import { SessionCard } from "@/components/session-card";

function App() {
  const { sessions, connected } = useSessions();
  const { hosts, hasReceivedHostStatus } = useHostStatus();
  const counts = summarize(sessions);

  const handleDelete = async (sessionId: string) => {
    await deleteSession(sessionId);
  };

  // Mirrors the mac (`PopoverView.emptyStateText`) and iOS
  // (`SessionListView.content`) empty-state split: until the first host-status
  // poll lands, we don't yet know whether "no sessions" means "genuinely
  // none" or "the watcher hasn't reported in" - see PRO-211/PRO-214.
  //
  // `watcherAppearsSilent` also catches the case a plain `hosts.length === 0`
  // check misses: a watcher that reported at least once and then died still
  // leaves `hosts` non-empty forever, with a `last_seen_at` that stops
  // advancing - see its doc comment in `lib/sessions.ts`.
  const noHostsReported = watcherAppearsSilent(hosts, hasReceivedHostStatus, new Date());

  return (
    <div className="min-h-screen bg-background">
      <SummaryBar counts={counts} connected={connected} />
      <main className="p-4 sm:p-6">
        {sessions.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-muted-foreground">
            <p className="text-lg">
              {noHostsReported ? "No watcher has reported in yet" : "No active sessions"}
            </p>
            <p className="text-sm">
              {noHostsReported
                ? "Start a session with the watcher running and it will appear here."
                : "Sessions will appear here when Claude Code is running"}
            </p>
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {sessions.map((session) => (
              <SessionCard
                key={session.session_id}
                session={session}
                onDelete={handleDelete}
              />
            ))}
          </div>
        )}
      </main>
    </div>
  );
}

export default App;
