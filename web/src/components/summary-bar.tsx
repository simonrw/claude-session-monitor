import type { SummaryCounts } from "@/lib/sessions";

export function SummaryBar({
  counts,
  connected,
}: {
  counts: SummaryCounts;
  connected: boolean;
}) {
  return (
    <div className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
      <div className="flex items-center justify-between px-4 py-3 sm:px-6">
        <h1 className="text-lg font-semibold">Sessions</h1>
        <div className="flex items-center gap-3 text-sm">
          <Pill color="green" count={counts.working} label="Working" />
          <Pill color="amber" count={counts.waitingInput} label="Input" />
          <Pill color="red" count={counts.waitingPermission} label="Permission" />
          <span
            className={`ml-2 inline-block h-2 w-2 rounded-full ${connected ? "bg-green-500" : "bg-red-500"}`}
          />
        </div>
      </div>
    </div>
  );
}

function Pill({
  color,
  count,
  label,
}: {
  color: "green" | "amber" | "red";
  count: number;
  label: string;
}) {
  const colors = {
    green: "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-400",
    amber: "bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-400",
    red: "bg-red-100 text-red-800 dark:bg-red-900/30 dark:text-red-400",
  };

  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 font-medium ${colors[color]}`}
    >
      <span className="font-mono">{count}</span>
      <span className="hidden sm:inline">{label}</span>
    </span>
  );
}
