import { Link } from 'react-router-dom';
import { ScheduleRunBrief } from '@/api/types';
import { cn } from '@/lib/utils';

interface RecentRunsStripProps {
  runs: ScheduleRunBrief[];
  /** How many of the most recent runs to render. */
  visible?: number;
}

// `phase` is qa-runs' own lowercase `RunState` set (`created | queued |
// dispatching | running | succeeded | failed | canceled | timed_out | expired
// | error` — `RunState::as_str`, qa-runs-sdk/src/models.rs:271-284), passed
// through unchanged by `runFromDto` (decision X4). `'Pending'` and `'Skipped'`
// are left as literal, unmatchable placeholders: this gear's `RunState` has no
// such variant, so there is no real lowercase value to switch them to — see
// the fix report for this file.
function phaseDotClass(phase: string): { dot: string; pulse: boolean } {
  switch (phase) {
    case 'running':
      return { dot: 'bg-blue-500', pulse: true };
    case 'Pending':
      return { dot: 'bg-amber-500', pulse: true };
    case 'succeeded':
      return { dot: 'bg-emerald-500', pulse: false };
    case 'failed':
    case 'error':
      return { dot: 'bg-red-500', pulse: false };
    case 'Skipped':
      return { dot: 'bg-yellow-500', pulse: false };
    default:
      return { dot: 'bg-muted-foreground', pulse: false };
  }
}

function relativeTime(value: string | null): string {
  if (!value) return '';
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return '';
  const diff = Date.now() - d.getTime();
  const m = Math.floor(diff / 60000);
  if (m < 1) return 'just now';
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const days = Math.floor(h / 24);
  return `${days}d ago`;
}

interface RunBarProps {
  run: ScheduleRunBrief;
}

function RunBar({ run }: RunBarProps) {
  const phase = phaseDotClass(run.phase);
  const denom = run.total > 0 ? run.total : run.passed + run.failed + run.skipped;
  const safeDenom = denom > 0 ? denom : 1;
  const passedPct = (run.passed / safeDenom) * 100;
  const failedPct = (run.failed / safeDenom) * 100;
  const skippedPct = (run.skipped / safeDenom) * 100;
  const hasData = denom > 0;
  const when = relativeTime(run.finished_at || run.started_at);
  const tooltip =
    `${run.phase}${when ? ' · ' + when : ''}\n` +
    `${run.workflow_name}\n` +
    `${run.passed} passed · ${run.failed} failed · ${run.skipped} skipped`;

  return (
    <Link
      to={`/runs/${encodeURIComponent(run.workflow_name)}`}
      className="flex items-center gap-2 rounded hover:bg-accent/40 whitespace-pre-line"
      title={tooltip}
    >
      <span className="relative inline-flex h-2 w-2 shrink-0 items-center justify-center">
        {phase.pulse && (
          <span className={cn('absolute inline-flex h-full w-full rounded-full opacity-70 animate-ping', phase.dot)} />
        )}
        <span className={cn('relative inline-flex h-2 w-2 rounded-full', phase.dot)} />
      </span>
      <span className="flex h-2.5 w-[180px] shrink-0 overflow-hidden rounded-sm bg-muted ring-1 ring-border/60">
        {hasData ? (
          <>
            {run.passed > 0 && (
              <span className="block h-full bg-emerald-500" style={{ width: `${passedPct}%` }} />
            )}
            {run.skipped > 0 && (
              <span className="block h-full bg-yellow-500" style={{ width: `${skippedPct}%` }} />
            )}
            {run.failed > 0 && (
              <span className="block h-full bg-red-500" style={{ width: `${failedPct}%` }} />
            )}
          </>
        ) : (
          <span className={cn('block h-full w-full opacity-50', phase.dot)} />
        )}
      </span>
    </Link>
  );
}

export function RecentRunsStrip({ runs, visible = 2 }: RecentRunsStripProps) {
  if (runs.length === 0) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }

  const head = runs.slice(0, visible);
  const more = Math.max(0, runs.length - head.length);

  return (
    <div className="flex flex-col gap-0.5">
      {head.map((run, idx) => (
        <RunBar key={`${run.workflow_name}-${idx}`} run={run} />
      ))}
      {more > 0 && (
        <span
          className="pl-5 text-[10px] text-muted-foreground"
          title={`${more} more run${more === 1 ? '' : 's'} not shown`}
        >
          +{more} more
        </span>
      )}
    </div>
  );
}
