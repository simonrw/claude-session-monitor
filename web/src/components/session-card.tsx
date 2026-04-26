import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { SessionView } from "@/lib/types";
import { relativeTime } from "@/lib/time";

export function SessionCard({
  session,
  onDelete,
}: {
  session: SessionView;
  onDelete: (id: string) => void;
}) {
  const projectName = session.cwd.split("/").pop() || session.cwd;

  return (
    <Card className={`border-l-4 ${borderColor(session)}`}>
      <CardHeader className="flex flex-row items-start justify-between gap-2 space-y-0 pb-2">
        <div className="min-w-0">
          <h3 className="truncate font-semibold leading-tight">{projectName}</h3>
          {session.hostname && (
            <p className="truncate text-xs text-muted-foreground">
              {session.hostname}
            </p>
          )}
        </div>
        <StatusBadge session={session} />
      </CardHeader>
      <CardContent className="space-y-1.5 text-sm">
        <StatusDetail session={session} />
        {session.git_branch && (
          <div className="flex items-center gap-1.5 text-muted-foreground">
            <GitBranchIcon />
            <span className="truncate">{session.git_branch}</span>
          </div>
        )}
        <div className="flex items-center justify-between pt-1">
          <span className="text-xs text-muted-foreground">
            {relativeTime(session.updated_at)}
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 text-xs text-destructive hover:text-destructive"
            onClick={() => onDelete(session.session_id)}
          >
            Remove
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

function StatusBadge({ session }: { session: SessionView }) {
  switch (session.status.type) {
    case "working":
      return <Badge className="bg-green-100 text-green-800 hover:bg-green-100 dark:bg-green-900/30 dark:text-green-400">Working</Badge>;
    case "waiting":
      if (session.status.reason === "permission") {
        return <Badge className="bg-red-100 text-red-800 hover:bg-red-100 dark:bg-red-900/30 dark:text-red-400">Permission</Badge>;
      }
      return <Badge className="bg-amber-100 text-amber-800 hover:bg-amber-100 dark:bg-amber-900/30 dark:text-amber-400">Input</Badge>;
    case "ended":
      return <Badge variant="secondary">Ended</Badge>;
  }
}

function StatusDetail({ session }: { session: SessionView }) {
  if (session.status.type === "working" && session.status.tool) {
    return (
      <p className="text-muted-foreground">
        Using <span className="font-mono text-foreground">{session.status.tool}</span>
      </p>
    );
  }
  if (session.status.type === "waiting") {
    const label = session.status.reason === "permission" ? "Waiting for permission" : "Waiting for input";
    return (
      <div>
        <p className="text-muted-foreground">{label}</p>
        {session.status.detail && (
          <p className="truncate text-xs text-muted-foreground/70">{session.status.detail}</p>
        )}
      </div>
    );
  }
  return null;
}

function borderColor(session: SessionView): string {
  switch (session.status.type) {
    case "working":
      return "border-l-green-500";
    case "waiting":
      return session.status.reason === "permission" ? "border-l-red-500" : "border-l-amber-500";
    case "ended":
      return "border-l-muted";
  }
}

function GitBranchIcon() {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <line x1="6" y1="3" x2="6" y2="15" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </svg>
  );
}
