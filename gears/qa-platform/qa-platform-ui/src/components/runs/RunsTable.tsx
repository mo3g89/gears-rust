import { Link } from 'react-router-dom';
import { WorkflowRun, isActiveRun } from '@/api/types';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { RunPhaseBadge } from '@/components/runs/RunPhaseBadge';
import { RunResultSummary, SucceededWithSkipsMarker } from '@/components/runs/RunResultSummary';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { Loader2, Square } from 'lucide-react';

interface RunsTableProps {
  runs: WorkflowRun[];
  platformBuildByName?: Map<string, string>;
  onStop?: (name: string) => void;
  stoppingRunName?: string | null;
}

function formatStartedValue(value: string | null): { primary: string; secondary?: string } {
  if (!value) {
    return { primary: '-' };
  }

  const started = new Date(value);
  if (Number.isNaN(started.getTime())) {
    return { primary: value };
  }

  const diffMs = Date.now() - started.getTime();
  const absMs = Math.abs(diffMs);
  const minute = 60 * 1000;
  const hour = 60 * minute;
  const day = 24 * hour;

  let relative = '';
  if (absMs < minute) {
    relative = diffMs >= 0 ? 'just now' : 'in moments';
  } else if (absMs < hour) {
    const mins = Math.round(absMs / minute);
    relative = diffMs >= 0 ? `${mins}m ago` : `in ${mins}m`;
  } else if (absMs < day) {
    const hours = Math.round(absMs / hour);
    relative = diffMs >= 0 ? `${hours}h ago` : `in ${hours}h`;
  } else {
    const days = Math.round(absMs / day);
    relative = diffMs >= 0 ? `${days}d ago` : `in ${days}d`;
  }

  return {
    primary: relative,
    secondary: started.toLocaleString(),
  };
}

export function RunsTable({
  runs,
  platformBuildByName,
  onStop,
  stoppingRunName,
}: RunsTableProps) {
  return (
    <div>
      {runs.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          No test runs found
        </div>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Platform</TableHead>
              <TableHead>Version</TableHead>
              <TableHead>Started</TableHead>
              <TableHead>Duration</TableHead>
              <TableHead className="text-right">Results</TableHead>
              <TableHead className="w-[120px]"></TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {runs.map((run) => (
              <TableRow
                key={run.name}
                className={run.is_validation ? 'bg-amber-50/40 dark:bg-amber-950/10' : undefined}
              >
                <TableCell>
                  <div className="flex items-center gap-2 flex-wrap">
                    <Link
                      to={`/runs/${run.name}`}
                      className="text-foreground/80 hover:text-foreground hover:underline"
                    >
                      {run.name}
                    </Link>
                    {(run.run_source || 'manual') === 'scheduled' && (
                      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">scheduled</span>
                    )}
                    {run.is_validation && (
                      <Badge variant="default" className="text-[10px] px-1.5 py-0 uppercase tracking-wide">
                        Validation
                      </Badge>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1.5">
                    <RunPhaseBadge phase={run.phase} />
                    {/* `run.phase` is `WorkflowRun.phase`, which `runFromDto` sets to
                        `dto.state` "without re-casing" (adapters.ts:378-383, decision X4) -
                        qa-runs' lowercase state set, not Title Case. `RunPhaseBadge` itself
                        switches on Title Case (a separate, pre-existing defect elsewhere,
                        not this task's to fix), but this marker's own condition must compare
                        against the real value or it never renders. */}
                    {run.phase === 'succeeded' && !!run.result?.skipped && (
                      <SucceededWithSkipsMarker skipped={run.result.skipped} />
                    )}
                  </div>
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {run.platform || 'Default'}
                </TableCell>
                <TableCell className="tabular-nums">
                  {(() => {
                    const appVersion = run.app_version?.trim() || '';
                    const testVersion = run.test_version?.trim() || '';
                    const snapshotBuild = run.app_build?.trim() || '';
                    const fallbackBuild =
                      (run.platform ? platformBuildByName?.get(run.platform)?.trim() : '') || '';
                    const effectiveBuild = snapshotBuild || fallbackBuild;
                    // Display format matches vpadm: "<version>.<build>" (the
                    // Platform table renders the same way) instead of the
                    // legacy hyphenated "version-build".
                    const appVersionWithBuild =
                      appVersion && effectiveBuild ? `${appVersion}.${effectiveBuild}` : appVersion;
                    const versionsMatch = !!appVersion && !!testVersion && appVersion === testVersion;

                    if (!appVersion && !testVersion) {
                      return <span className="text-muted-foreground">—</span>;
                    }

                    if (versionsMatch) {
                      return <span>{appVersionWithBuild}</span>;
                    }

                    return (
                      <span title={`App: ${appVersionWithBuild || '-'}\nTests: ${testVersion || '-'}`}>
                        {appVersionWithBuild || '-'}
                        {testVersion && (
                          <span className="text-muted-foreground"> / {testVersion}</span>
                        )}
                      </span>
                    );
                  })()}
                </TableCell>
                <TableCell>
                  {(() => {
                    const formatted = formatStartedValue(run.started_at);
                    if (!formatted.secondary) {
                      return <span className="text-sm text-muted-foreground">{formatted.primary}</span>;
                    }
                    return (
                      <div className="flex flex-col leading-tight">
                        <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{formatted.primary}</span>
                        <span className="text-xs text-muted-foreground">{formatted.secondary}</span>
                      </div>
                    );
                  })()}
                </TableCell>
                <TableCell className="text-sm text-muted-foreground">
                  {run.duration || '-'}
                </TableCell>
                <TableCell className="text-right">
                  <RunResultSummary result={run.result} />
                </TableCell>
                <TableCell className="text-right">
                  {onStop && isActiveRun(run) && (
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7 text-destructive hover:text-destructive"
                      onClick={() => onStop(run.name)}
                      disabled={!!stoppingRunName}
                      title="Stop run"
                      aria-label="Stop run"
                    >
                      {stoppingRunName === run.name ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : (
                        <Square className="h-3.5 w-3.5 fill-current" />
                      )}
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  );
}
