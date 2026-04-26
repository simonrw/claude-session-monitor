import { useSessions, deleteSession } from "@/hooks/use-sessions";
import { summarize } from "@/lib/sessions";
import { SummaryBar } from "@/components/summary-bar";
import { SessionCard } from "@/components/session-card";

function App() {
  const { sessions, connected } = useSessions();
  const counts = summarize(sessions);

  const handleDelete = async (sessionId: string) => {
    await deleteSession(sessionId);
  };

  return (
    <div className="min-h-screen bg-background">
      <SummaryBar counts={counts} connected={connected} />
      <main className="p-4 sm:p-6">
        {sessions.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-muted-foreground">
            <p className="text-lg">No active sessions</p>
            <p className="text-sm">Sessions will appear here when Claude Code is running</p>
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
