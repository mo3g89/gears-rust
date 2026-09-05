import { cn } from '@/lib/utils';

interface RunPhaseBadgeProps {
  phase: string;
  className?: string;
}

interface PhaseStyle {
  label: string;
  dot: string;
  text: string;
  ring: string;
  pulse: boolean;
}

// `phase` is qa-runs' own lowercase `RunState` set — `created | queued |
// dispatching | running | succeeded | failed | canceled | timed_out | expired
// | error` (`RunState::as_str`, qa-runs-sdk/src/models.rs:271-284) — passed
// through by `runFromDto` "without re-casing" (`adapters.ts`, decision X4).
// These cases used to compare against the Title-Case Argo phases legacy sent,
// so every real run fell through to `default` and never showed its intended
// colour. `'Pending'` and `'Skipped'` are left as-is below: neither is a value
// this gear's `RunState` can ever emit for `run.phase` (there is no `Pending`
// or `Skipped` variant), so there is no real lowercase spelling to switch them
// to — see the fix report for this file.
function styleFor(phase: string): PhaseStyle {
  switch (phase) {
    case 'running':
      return {
        label: 'Running',
        dot: 'bg-blue-500',
        text: 'text-blue-700 dark:text-blue-300',
        ring: 'ring-blue-500/30',
        pulse: true,
      };
    case 'Pending':
      return {
        label: 'Pending',
        dot: 'bg-amber-500',
        text: 'text-amber-700 dark:text-amber-300',
        ring: 'ring-amber-500/30',
        pulse: true,
      };
    case 'succeeded':
      return {
        label: 'Succeeded',
        dot: 'bg-emerald-500',
        text: 'text-emerald-700 dark:text-emerald-300',
        ring: 'ring-emerald-500/20',
        pulse: false,
      };
    case 'failed':
      return {
        label: 'Failed',
        dot: 'bg-red-500',
        text: 'text-red-700 dark:text-red-300',
        ring: 'ring-red-500/30',
        pulse: false,
      };
    case 'error':
      return {
        label: 'Error',
        dot: 'bg-red-500',
        text: 'text-red-700 dark:text-red-300',
        ring: 'ring-red-500/30',
        pulse: false,
      };
    case 'Skipped':
      return {
        label: 'Skipped',
        dot: 'bg-yellow-500',
        text: 'text-yellow-700 dark:text-yellow-300',
        ring: 'ring-yellow-500/20',
        pulse: false,
      };
    default:
      return {
        label: phase || 'Unknown',
        dot: 'bg-muted-foreground',
        text: 'text-muted-foreground',
        ring: 'ring-muted-foreground/20',
        pulse: false,
      };
  }
}

export function RunPhaseBadge({ phase, className }: RunPhaseBadgeProps) {
  const style = styleFor(phase);

  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 text-xs font-medium tabular-nums',
        style.text,
        className
      )}
    >
      <span className="relative inline-flex h-2 w-2 shrink-0 items-center justify-center">
        {style.pulse && (
          <span
            className={cn(
              'absolute inline-flex h-full w-full rounded-full opacity-70 animate-ping',
              style.dot
            )}
          />
        )}
        <span
          className={cn(
            'relative inline-flex h-2 w-2 rounded-full ring-1 ring-offset-0',
            style.dot,
            style.ring
          )}
        />
      </span>
      {style.label}
    </span>
  );
}
