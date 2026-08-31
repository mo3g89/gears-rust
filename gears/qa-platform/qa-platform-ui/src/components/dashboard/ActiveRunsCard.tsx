import { Link } from 'react-router-dom';
import { WorkflowRun } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { RunPhaseBadge } from '@/components/runs/RunPhaseBadge';
import { Activity } from 'lucide-react';

interface ActiveRunsCardProps {
  runs: WorkflowRun[];
}

function elapsed(startedAt: string | null): string {
  if (!startedAt) return '—';
  const d = new Date(startedAt);
  if (Number.isNaN(d.getTime())) return '—';
  const diff = Math.max(0, Date.now() - d.getTime());
  const h = Math.floor(diff / 3600000);
  const m = Math.floor((diff % 3600000) / 60000);
  const s = Math.floor((diff % 60000) / 1000);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export function ActiveRunsCard({ runs }: ActiveRunsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Activity className="h-4 w-4 text-blue-500" />
          Active Runs
          <Badge variant="secondary" className="ml-1 px-1.5 py-0 text-xs tabular-nums">{runs.length}</Badge>
        </CardTitle>
        <CardDescription>Workflows in Running or Pending state</CardDescription>
      </CardHeader>
      <CardContent>
        {runs.length === 0 ? (
          <p className="py-4 text-sm text-muted-foreground">All quiet — no runs in progress.</p>
        ) : (
          <ul className="divide-y">
            {runs.map((run) => (
              <li key={run.name} className="flex items-center justify-between gap-3 py-2">
                <div className="min-w-0 flex-1">
                  <Link
                    to={`/runs/${run.name}`}
                    className="block truncate text-sm text-foreground/80 hover:text-foreground hover:underline"
                    title={run.name}
                  >
                    {run.name}
                  </Link>
                  <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
                    <span title={`Plan: ${run.plan_id}`}>{run.plan_id}</span>
                    {run.platform && <span>· {run.platform}</span>}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-3">
                  <RunPhaseBadge phase={run.phase} />
                  <span className="font-mono text-xs text-muted-foreground tabular-nums">
                    {elapsed(run.started_at)}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
