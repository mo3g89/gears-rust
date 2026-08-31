import { RunResultCounts } from '@/api/types';
import { cn } from '@/lib/utils';

/**
 * The run counts idiom this codebase already uses for a run's outcome —
 * `total/passed/in_progress/failed/skipped`, each dimmed to muted when zero,
 * with the full breakdown in a tooltip. Copied from
 * `SchedulesTable.tsx`'s "Latest" column (and the same palette
 * `RecentRunsStrip.tsx` uses for its bar segments), not invented here — Task
 * 10 asked for the run list and run detail to show `skipped` "using the
 * existing UI's own idiom", and this is it.
 */
export function RunResultSummary({
  result,
  className,
}: {
  result?: RunResultCounts;
  className?: string;
}) {
  if (!result) {
    return <span className="text-muted-foreground">—</span>;
  }
  const { total, passed, in_progress, failed, skipped } = result;
  return (
    <span
      className={cn('tabular-nums', className)}
      title={`Total ${total} · Pass ${passed} · In progress ${in_progress} · Fail ${failed} · Skip ${skipped}`}
    >
      <span className="text-muted-foreground">{total}</span>
      <span className="text-muted-foreground">/</span>
      <span className="text-emerald-600 dark:text-emerald-400">{passed}</span>
      <span className="text-muted-foreground">/</span>
      <span className={in_progress > 0 ? 'text-blue-600 dark:text-blue-400' : 'text-muted-foreground'}>
        {in_progress}
      </span>
      <span className="text-muted-foreground">/</span>
      <span className={failed > 0 ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'}>
        {failed}
      </span>
      <span className="text-muted-foreground">/</span>
      <span className={skipped > 0 ? 'text-amber-600 dark:text-amber-400' : 'text-muted-foreground'}>
        {skipped}
      </span>
    </span>
  );
}

/**
 * The marker Task 10 requires: "succeeded, 68 skipped" must not read as a
 * clean pass. `derive_terminal_state` stopped downgrading a `Succeeded`
 * outcome for a non-zero skip count (product owner decision, 2026-08-28), so
 * this badge is the only place that fact is still visible next to the
 * verdict — it renders only when the caller has already checked
 * `phase === 'succeeded' && skipped > 0`, not on its own, so a failed or
 * still-running run with skips (which is a `failed` verdict on its own
 * merits) never shows it.
 *
 * **Lowercase, and that is the contract, not a style choice.** `runFromDto`
 * sets `phase: dto.state` with no re-casing (`api/adapters.ts`, "Runs" section),
 * so `phase` carries qa-runs' own lowercase set — `created | queued |
 * dispatching | running | succeeded | failed | canceled | timed_out | expired |
 * error`. Both callers get it right (`RunsTable.tsx`, `RunDetailPage.tsx`);
 * this comment used to spell it `'Succeeded'`, which is the exact mistake that
 * produced Task 10's Critical, left standing in the doc that tells the next
 * person what to write.
 */
export function SucceededWithSkipsMarker({ skipped }: { skipped: number }) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded px-1.5 py-0 text-[10px] font-medium uppercase tracking-wide text-amber-700 bg-amber-100 dark:text-amber-300 dark:bg-amber-950/40"
      title={`Succeeded, but ${skipped} test${skipped === 1 ? '' : 's'} skipped — this run did not exercise everything it was asked to`}
    >
      {skipped} skipped
    </span>
  );
}
