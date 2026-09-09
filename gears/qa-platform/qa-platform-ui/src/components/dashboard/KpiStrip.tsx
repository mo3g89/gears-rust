import { Link } from 'react-router-dom';
import { Activity, AlertTriangle, CheckCircle2, Zap } from 'lucide-react';
import { cn } from '@/lib/utils';

interface KpiStripProps {
  activeRuns: number;
  failed24h: number;
  failedPrev24h: number;
  passRate24h: number | null;
  passRatePrev24h: number | null;
  flakyCount: number;
}

interface KpiCardProps {
  label: string;
  value: string;
  hint?: string;
  hintTone?: 'neutral' | 'good' | 'bad';
  icon: React.ReactNode;
  to?: string;
  onClick?: () => void;
  accent?: 'active' | 'warn' | 'bad' | 'neutral';
}

function KpiCard({ label, value, hint, hintTone = 'neutral', icon, to, onClick, accent = 'neutral' }: KpiCardProps) {
  const accentClass = {
    active: 'border-blue-200/60 dark:border-blue-900/60',
    warn: 'border-amber-200/60 dark:border-amber-900/60',
    bad: 'border-red-200/60 dark:border-red-900/60',
    neutral: '',
  }[accent];

  const hintClass = {
    neutral: 'text-muted-foreground',
    good: 'text-emerald-600 dark:text-emerald-400',
    bad: 'text-red-600 dark:text-red-400',
  }[hintTone];

  const body = (
    <div
      className={cn(
        'rounded-md border bg-card px-3 py-2 transition-colors hover:bg-accent/30',
        accentClass,
        (to || onClick) && 'cursor-pointer'
      )}
      onClick={onClick}
    >
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span className="uppercase tracking-wide">{label}</span>
        <span className="text-muted-foreground/70">{icon}</span>
      </div>
      <div className="mt-1 flex items-baseline gap-2">
        <span className="text-xl font-semibold tabular-nums text-foreground/90">{value}</span>
        {hint && <span className={cn('text-xs', hintClass)}>{hint}</span>}
      </div>
    </div>
  );

  return to ? <Link to={to}>{body}</Link> : body;
}

function formatRate(rate: number | null): string {
  if (rate == null) return '—';
  return `${Math.round(rate * 100)}%`;
}

function rateDelta(current: number | null, previous: number | null): { text: string; tone: 'neutral' | 'good' | 'bad' } {
  if (current == null || previous == null) return { text: '', tone: 'neutral' };
  const diff = (current - previous) * 100;
  if (Math.abs(diff) < 0.5) return { text: '≈ vs 24h', tone: 'neutral' };
  if (diff > 0) return { text: `↑ ${diff.toFixed(1)}pp`, tone: 'good' };
  return { text: `↓ ${Math.abs(diff).toFixed(1)}pp`, tone: 'bad' };
}

function countDelta(current: number, previous: number): { text: string; tone: 'neutral' | 'good' | 'bad' } {
  const diff = current - previous;
  if (diff === 0) return { text: '= vs 24h', tone: 'neutral' };
  if (diff > 0) return { text: `↑ ${diff} vs 24h`, tone: 'bad' };
  return { text: `↓ ${Math.abs(diff)} vs 24h`, tone: 'good' };
}

export function KpiStrip({
  activeRuns,
  failed24h,
  failedPrev24h,
  passRate24h,
  passRatePrev24h,
  flakyCount,
}: KpiStripProps) {
  const failedDelta = countDelta(failed24h, failedPrev24h);
  const rateDeltaInfo = rateDelta(passRate24h, passRatePrev24h);

  // Still no "Environments healthy/total" tile, but the reason changed: environment
  // health now HAS a source (the cluster-health design), and `EnvironmentsStrip`
  // below renders the full healthy/degraded/unhealthy/unreachable breakdown.
  // A KPI tile would restate one slice of that card a few pixels above it, so
  // it stays out on redundancy grounds rather than for want of data — see
  // REMOVED-SURFACES.md (Task 8a C3).
  return (
    <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
      <KpiCard
        label="Active"
        value={String(activeRuns)}
        icon={<Activity className="h-4 w-4" />}
        accent={activeRuns > 0 ? 'active' : 'neutral'}
        to="/runs?phase=Running"
      />
      <KpiCard
        label="Failed 24h"
        value={String(failed24h)}
        hint={failedDelta.text}
        hintTone={failedDelta.tone}
        icon={<AlertTriangle className="h-4 w-4" />}
        accent={failed24h > 0 ? 'bad' : 'neutral'}
        to="/runs?status=Failed"
      />
      <KpiCard
        label="Pass rate 24h"
        value={formatRate(passRate24h)}
        hint={rateDeltaInfo.text}
        hintTone={rateDeltaInfo.tone}
        icon={<CheckCircle2 className="h-4 w-4" />}
      />
      <KpiCard
        label="Flaky 7d"
        value={String(flakyCount)}
        icon={<Zap className="h-4 w-4" />}
        accent={flakyCount > 0 ? 'warn' : 'neutral'}
      />
    </div>
  );
}
