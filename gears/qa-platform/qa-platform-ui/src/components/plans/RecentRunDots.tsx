import { Link } from 'react-router-dom';
import { ScheduleRunBrief } from '@/api/types';
import { cn } from '@/lib/utils';

interface RecentRunDotsProps {
  runs: ScheduleRunBrief[];
  /** How many of the most recent runs to render as dots. */
  visible?: number;
}

// `run.phase` is qa-runs' own lowercase `RunState` set (`created | queued |
// dispatching | running | succeeded | failed | canceled | timed_out | expired
// | error` — `RunState::as_str`, qa-runs-sdk/src/models.rs:271-284), passed
// through unchanged by `runFromDto` (decision X4). `'Pending'` and `'Skipped'`
// are left as literal, unmatchable placeholders: this gear's `RunState` has no
// such variant, so there is no real lowercase value to switch them to — see
// the fix report for this file.
function phaseDotClass(run: ScheduleRunBrief): { dot: string; pulse: boolean } {
  switch (run.phase) {
    case 'running':
      return { dot: 'bg-blue-500', pulse: true };
    case 'Pending':
      return { dot: 'bg-amber-500', pulse: true };
    case 'succeeded':
      // A "succeeded" workflow can still contain failed tests — reflect that.
      return { dot: run.failed > 0 ? 'bg-red-500' : 'bg-emerald-500', pulse: false };
    case 'failed':
    case 'error':
      return { dot: 'bg-red-500', pulse: false };
    case 'Skipped':
      return { dot: 'bg-yellow-500', pulse: false };
    default:
      return { dot: 'bg-muted-foreground/60', pulse: false };
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
  return `${Math.floor(h / 24)}d ago`;
}

/** A compact row of up to N dots, one per recent run, coloured by outcome.
 *  Newest run first. Used in the Test Plans list in place of Source/Product. */
export function RecentRunDots({ runs, visible = 5 }: RecentRunDotsProps) {
  if (!runs || runs.length === 0) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }

  const head = runs.slice(0, visible);

  return (
    <div className="flex items-center gap-1">
      {head.map((run, idx) => {
        const phase = phaseDotClass(run);
        const when = relativeTime(run.finished_at || run.started_at);
        const tooltip =
          `${run.phase}${when ? ' · ' + when : ''}\n` +
          `${run.passed} passed · ${run.failed} failed · ${run.skipped} skipped`;
        return (
          <Link
            key={`${run.workflow_name}-${idx}`}
            to={`/runs/${encodeURIComponent(run.workflow_name)}`}
            className="relative inline-flex h-2.5 w-2.5 items-center justify-center whitespace-pre-line"
            title={tooltip}
          >
            {phase.pulse && (
              <span className={cn('absolute inline-flex h-full w-full rounded-full opacity-70 animate-ping', phase.dot)} />
            )}
            <span className={cn('relative inline-flex h-2.5 w-2.5 rounded-full ring-1 ring-border/60', phase.dot)} />
          </Link>
        );
      })}
    </div>
  );
}
