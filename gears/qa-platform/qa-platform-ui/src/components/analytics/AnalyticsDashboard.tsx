import { useEffect, useMemo, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import ReactApexChart from 'react-apexcharts';
import type { ApexOptions } from 'apexcharts';
import { chartThemeOptions, useChartTheme, type ChartTheme } from '@/lib/chart-theme';
import {
  useAnalyticsOverview,
  useProducts,
  useObservedProductVersions,
  useObservedProductBranches,
  useRun,
  useJiraConfig,
  useCollectCases,
} from '@/api/hooks';
import { toast } from 'sonner';
import { getStatusColor } from '@/api/types';
import type {
  AnalyticsGroupBy,
  AnalyticsListItem,
  AnalyticsOverviewQuery,
  AnalyticsScope,
  TestFileInfo,
} from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Combobox } from '@/components/ui/combobox';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { RunTestDialog } from '@/components/tests/RunTestDialog';
import { UnavailableNotice } from '@/components/ui/unavailable';
import { cn } from '@/lib/utils';
import { ChevronRight, Loader2, Play, RefreshCcw } from 'lucide-react';

interface AnalyticsDashboardProps {
  scope: AnalyticsScope;
  planId?: string;
}

const runFinishedAtFormatter = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: '2-digit',
  year: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
});

function formatRunFinishedAt(value: string): { primary: string; secondary?: string } {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return { primary: value };
  }

  const diffMs = Date.now() - date.getTime();
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
    secondary: runFinishedAtFormatter.format(date),
  };
}

type ResultColumnTone = 'passed' | 'failed' | 'skipped' | 'xfail' | 'xpass' | 'not_run';

// Single per-tone metadata table (label, dot/chip colors, donut hex, and
// filter-button styling) consumed by every render site below — mosaic dots,
// stat chips, donut charts, and the status filter buttons — so a new tone or
// a label/color rename only has to happen in one place.
//
// Filter-button styling: `idle` is a light tint of the status's own color
// (rather than a flat grey) so each button stays recognizable at rest;
// `active` steps up to a more saturated fill plus a ring so the current
// selection is unambiguous even with several tones toggled on at once.
const TONE_META: Record<
  ResultColumnTone,
  { label: string; icon: string; dot: string; color: string; chip: string; idle: string; active: string }
> = {
  passed: {
    label: 'Passed',
    icon: '✓',
    dot: 'bg-emerald-500',
    color: '#16a34a',
    chip: 'border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-900 dark:bg-emerald-950/30 dark:text-emerald-400',
    idle: 'border-emerald-200 bg-emerald-50 text-emerald-700 hover:bg-emerald-100 dark:border-emerald-900 dark:bg-emerald-950/30 dark:text-emerald-400 dark:hover:bg-emerald-900/30',
    active:
      'border-emerald-400 bg-emerald-200 text-emerald-900 ring-1 ring-emerald-500 dark:border-emerald-600 dark:bg-emerald-800/60 dark:text-emerald-100',
  },
  failed: {
    label: 'Failed',
    icon: '✗',
    dot: 'bg-red-500',
    color: '#dc2626',
    chip: 'border-red-200 bg-red-50 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-400',
    idle: 'border-red-200 bg-red-50 text-red-700 hover:bg-red-100 dark:border-red-900 dark:bg-red-950/30 dark:text-red-400 dark:hover:bg-red-900/30',
    active:
      'border-red-400 bg-red-200 text-red-900 ring-1 ring-red-500 dark:border-red-600 dark:bg-red-800/60 dark:text-red-100',
  },
  skipped: {
    label: 'Skipped',
    icon: '⊘',
    dot: 'bg-amber-500',
    color: '#eab308',
    chip: 'border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-900 dark:bg-amber-950/30 dark:text-amber-400',
    idle: 'border-amber-200 bg-amber-50 text-amber-700 hover:bg-amber-100 dark:border-amber-900 dark:bg-amber-950/30 dark:text-amber-400 dark:hover:bg-amber-900/30',
    active:
      'border-amber-400 bg-amber-200 text-amber-900 ring-1 ring-amber-500 dark:border-amber-600 dark:bg-amber-800/60 dark:text-amber-100',
  },
  xfail: {
    label: 'XFAIL',
    icon: '⚠',
    dot: 'bg-purple-500',
    color: '#9333ea',
    chip: 'border-purple-200 bg-purple-50 text-purple-700 dark:border-purple-900 dark:bg-purple-950/30 dark:text-purple-400',
    idle: 'border-purple-200 bg-purple-50 text-purple-700 hover:bg-purple-100 dark:border-purple-900 dark:bg-purple-950/30 dark:text-purple-400 dark:hover:bg-purple-900/30',
    active:
      'border-purple-400 bg-purple-200 text-purple-900 ring-1 ring-purple-500 dark:border-purple-600 dark:bg-purple-800/60 dark:text-purple-100',
  },
  xpass: {
    label: 'XPASS',
    icon: '★',
    dot: 'bg-blue-600',
    color: '#2563eb',
    chip: 'border-blue-300 bg-blue-100 text-blue-800 dark:border-blue-800 dark:bg-blue-950/40 dark:text-blue-300',
    idle: 'border-blue-200 bg-blue-50 text-blue-700 hover:bg-blue-100 dark:border-blue-900 dark:bg-blue-950/30 dark:text-blue-400 dark:hover:bg-blue-900/30',
    active:
      'border-blue-400 bg-blue-200 text-blue-900 ring-1 ring-blue-500 dark:border-blue-600 dark:bg-blue-800/60 dark:text-blue-100',
  },
  not_run: {
    label: 'Not run',
    icon: '○',
    dot: 'bg-slate-400',
    color: '#9ca3af',
    chip: 'border-slate-200 bg-slate-50 text-slate-600 dark:border-slate-700 dark:bg-slate-900/30 dark:text-slate-400',
    idle: 'border-slate-200 bg-slate-50 text-slate-600 hover:bg-slate-100 dark:border-slate-700 dark:bg-slate-900/30 dark:text-slate-400 dark:hover:bg-slate-800/40',
    active:
      'border-slate-400 bg-slate-300 text-slate-900 ring-1 ring-slate-500 dark:border-slate-500 dark:bg-slate-700/60 dark:text-slate-100',
  },
};

// Styling for the "All" clear-filter button — deliberately a neutral white
// (bg-background) rather than the grey used for "Not run", so the two are
// never confused at a glance.
const ALL_FILTER_META = {
  idle: 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-accent-foreground',
  active: 'border-foreground/30 bg-background text-foreground ring-1 ring-foreground/20',
};

type PlanGroupData = {
  planId: string;
  planName: string;
  passed: AnalyticsListItem[];
  failed: AnalyticsListItem[];
  skipped: AnalyticsListItem[];
  neverRun: AnalyticsListItem[];
};

// Left-border color reflecting the worst status present: any failure → red,
// otherwise any skipped → amber, otherwise green if anything ran, otherwise grey.
function ribbonBorderClass(group: PlanGroupData): string {
  if (group.failed.length > 0) return 'border-l-red-500';
  if (group.skipped.length > 0) return 'border-l-amber-500';
  if (group.passed.length > 0) return 'border-l-emerald-500';
  return 'border-l-slate-300 dark:border-l-slate-600';
}

// Build the minimal TestFileInfo that RunTestDialog needs from an analytics row,
// so a failed test can be re-run through the existing single-test dialog. The
// dialog only reads plan_id/test_file/plan_name/product_id/repo_id; repo_id is
// left null since analytics rows don't carry it — the dialog's branch list
// already merges branches across every repo registered for the product.
function analyticsItemToTestFileInfo(item: AnalyticsListItem, productId: string): TestFileInfo {
  return {
    plan_id: item.plan_id,
    plan_name: item.plan_name,
    source: '',
    source_path: null,
    repo_id: null,
    repo_name: null,
    product_id: productId || null,
    product_key: null,
    product_name: null,
    test_file: item.test_file,
    title: item.test_name,
    component: item.component,
    description: null,
    tags: item.tags,
    versions: item.versions,
  };
}

// Match a run's per-file test_file against an analytics row's test_file,
// tolerant of leading ./ and absolute-vs-relative prefixes.
function analyticsFileMatches(a?: string | null, b?: string | null): boolean {
  if (!a || !b) return false;
  const na = a.replace(/^\.?\//, '').replace(/\\/g, '/');
  const nb = b.replace(/^\.?\//, '').replace(/\\/g, '/');
  return na === nb || na.endsWith(nb) || nb.endsWith(na);
}

// Tailwind dot color for an effective per-case status (XFAIL → purple, etc.).
function caseDotClass(status: string): string {
  switch (status) {
    case 'FAILED':
    case 'ERROR':
      return 'bg-red-500';
    case 'XPASS':
      return 'bg-blue-600';
    case 'XFAIL':
      return 'bg-purple-500';
    case 'SKIPPED':
      return 'bg-yellow-500';
    case 'PASSED':
      return 'bg-emerald-500';
    default:
      return 'bg-slate-400';
  }
}

// Effective filterable status for a test file: derived from its per-case
// signal (case_status, falling back to last_status) so XFAIL/XPASS files are
// their own bucket rather than being folded into plain Passed/Failed —
// mirrors the coloring in `caseDotClass` above. Files with zero runs are
// always "not run", and an unrecognized/missing status also falls back to
// "not run" (matches how such files are already counted elsewhere).
function effectiveTone(item: AnalyticsListItem): ResultColumnTone {
  if (item.total_runs === 0) return 'not_run';
  const cs = (item.case_status || item.last_status || '').toUpperCase();
  switch (cs) {
    case 'FAILED':
    case 'ERROR':
      return 'failed';
    case 'XPASS':
      return 'xpass';
    case 'XFAIL':
      return 'xfail';
    case 'SKIPPED':
      return 'skipped';
    case 'PASSED':
      return 'passed';
    default:
      return 'not_run';
  }
}

// One test (file) row. Dot color + tickets come from the overview's per-case
// signal (immediate, no fetch); expanding the row lazily loads the latest run
// for the full per-function breakdown.
function TestRow({
  item,
  tone,
  version,
  jiraBase,
  onRun,
}: {
  item: AnalyticsListItem;
  tone: ResultColumnTone;
  version: string;
  jiraBase: string;
  onRun: () => void;
}) {
  const [open, setOpen] = useState(false);
  const showRun = tone !== 'not_run';
  const dotClass = item.case_status
    ? caseDotClass(item.case_status)
    : TONE_META[tone].dot;
  const tickets = item.case_tickets ?? [];
  const ticketHref = (t: string) => (jiraBase ? `${jiraBase}/browse/${t}` : null);

  // Full per-function breakdown — fetched only while the row is expanded.
  const { data: runDetail } = useRun(open && item.last_run_name ? item.last_run_name : '');
  const cases =
    runDetail?.test_results.find((r) => analyticsFileMatches(r.test_file, item.test_file))?.cases ??
    [];

  return (
    <details
      className="border-b last:border-b-0"
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 px-2 py-1 pl-6 hover:bg-muted/50">
        <span className={`h-2 w-2 shrink-0 rounded-full ${dotClass}`} />
        <span
          className="min-w-0 flex-1 truncate text-[13px] text-foreground/90"
          title={item.test_name}
        >
          {item.test_name}
        </span>
        {tickets.map((t) => {
          const href = ticketHref(t);
          return href ? (
            <a
              key={t}
              href={href}
              target="_blank"
              rel="noreferrer"
              onClick={(e) => e.stopPropagation()}
              className="shrink-0 rounded border border-purple-200 bg-purple-50 px-1 text-[10px] font-medium text-purple-700 hover:underline dark:border-purple-900 dark:bg-purple-950/30 dark:text-purple-300"
            >
              {t}
            </a>
          ) : (
            <span
              key={t}
              className="shrink-0 rounded border bg-muted/40 px-1 text-[10px] font-medium text-muted-foreground"
            >
              {t}
            </span>
          );
        })}
        {showRun && item.last_build ? (
          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
            {version ? `${version}-${item.last_build}` : item.last_build}
          </span>
        ) : null}
        {tone === 'failed' ? (
          <button
            type="button"
            title="Re-run this test"
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              onRun();
            }}
            className="inline-flex shrink-0 items-center gap-1 rounded-md border border-red-200 bg-red-50 px-1.5 py-0.5 text-[11px] font-medium text-red-700 hover:bg-red-100 dark:border-red-900 dark:bg-red-950/30 dark:text-red-400 dark:hover:bg-red-900/40"
          >
            <Play className="h-3 w-3" />
            Run
          </button>
        ) : null}
      </summary>
      <div className="space-y-0.5 px-2 pb-1.5 pl-8 text-[11px] text-muted-foreground">
        <div>Component: {item.component || '—'}</div>
        <div className="truncate" title={item.tags.join(', ')}>
          Tags: {item.tags.length ? item.tags.join(', ') : '—'}
        </div>
        {showRun && item.last_run_name ? (
          <div className="truncate">
            Run:{' '}
            <Link
              to={`/runs/${item.last_run_name}`}
              className="font-medium text-blue-700 hover:underline dark:text-blue-300"
            >
              {item.last_run_name}
            </Link>
          </div>
        ) : null}
        {showRun && item.last_run_finished_at
          ? (() => {
              const formatted = formatRunFinishedAt(item.last_run_finished_at);
              return <div>Finished: {formatted.secondary ?? formatted.primary}</div>;
            })()
          : null}
        {open && cases.length > 0 ? (
          <div className="pt-1">
            <div className="mb-0.5 font-medium text-foreground/70">Test cases · last run</div>
            <div className="space-y-1">
              {cases.map((c, i) => {
                const href = c.ticket && jiraBase ? `${jiraBase}/browse/${c.ticket}` : null;
                return (
                  <div key={i} className="flex flex-wrap items-center gap-1.5">
                    <Badge className={`${getStatusColor(c.status)} text-[10px]`}>{c.status}</Badge>
                    <span className="font-mono text-foreground/80">{c.name}</span>
                    {c.duration ? <span>{c.duration}</span> : null}
                    {c.ticket ? (
                      href ? (
                        <a
                          href={href}
                          target="_blank"
                          rel="noreferrer"
                          className="font-medium text-blue-700 hover:underline dark:text-blue-300"
                        >
                          {c.ticket}
                        </a>
                      ) : (
                        <Badge variant="outline" className="text-[10px] uppercase tracking-wide">
                          {c.ticket}
                        </Badge>
                      )
                    ) : null}
                    {c.reason ? (
                      <span className="truncate" title={c.reason}>
                        {c.reason}
                      </span>
                    ) : null}
                  </div>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </details>
  );
}

function PlanGroup({
  group,
  version,
  statusFilter,
  productId,
}: {
  group: PlanGroupData;
  version: string;
  /** Empty set = show every status. Otherwise only the selected tones. */
  statusFilter: Set<ResultColumnTone>;
  productId: string;
}) {
  const [runTarget, setRunTarget] = useState<AnalyticsListItem | null>(null);
  const { data: jiraConfig } = useJiraConfig();
  const jiraBase = (jiraConfig?.url ?? '').trim().replace(/\/+$/, '');

  const total =
    group.passed.length + group.failed.length + group.skipped.length + group.neverRun.length;

  // Order entries inside a plan: failed first (so a red dot is the first thing
  // you see), then skipped, passed, never-run — the tone of each entry is its
  // effective per-case status (so XFAIL/XPASS files filter independently of
  // plain Passed/Failed even though they're bucketed under `group.passed`).
  // When one or more status filters are active, only the matching tones are
  // kept and a plan with no matches is hidden entirely.
  const allEntries: Array<{ tone: ResultColumnTone; item: AnalyticsListItem }> = [
    ...group.failed.map((item) => ({ tone: effectiveTone(item), item })),
    ...group.skipped.map((item) => ({ tone: effectiveTone(item), item })),
    ...group.passed.map((item) => ({ tone: effectiveTone(item), item })),
    ...group.neverRun.map((item) => ({ tone: effectiveTone(item), item })),
  ];
  const entries =
    statusFilter.size > 0 ? allEntries.filter((entry) => statusFilter.has(entry.tone)) : allEntries;
  if (statusFilter.size > 0 && entries.length === 0) return null;

  const showSubtitle = group.planName && group.planName !== group.planId;

  return (
    <>
    <details
      className={`group overflow-hidden rounded-md border border-l-4 bg-card ${ribbonBorderClass(group)}`}
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 px-2 py-1.5 hover:bg-muted/30">
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform group-open:rotate-90" />
          <span
            className="min-w-0 flex-1 truncate text-sm text-foreground/90"
            title={showSubtitle ? `${group.planName}\n${group.planId}` : group.planName}
          >
            {group.planName || group.planId}
          </span>
          <span className="flex shrink-0 flex-wrap items-center justify-end gap-0.5 max-w-[55%]">
            {entries.map(({ tone, item }) => {
              const dotCls = item.case_status
                ? caseDotClass(item.case_status)
                : TONE_META[tone].dot;
              return (
                <span
                  key={`mosaic-${tone}-${item.test_file}`}
                  className={`h-2.5 w-2.5 rounded-sm ${dotCls}`}
                  title={`${item.test_name} · ${item.case_status ?? TONE_META[tone].label}`}
                />
              );
            })}
          </span>
          <span className="flex shrink-0 items-center gap-1.5 text-[11px] tabular-nums">
            {group.failed.length > 0 ? (
              <span className="text-red-700 dark:text-red-400">✗{group.failed.length}</span>
            ) : null}
            {group.skipped.length > 0 ? (
              <span className="text-amber-700 dark:text-amber-400">⊘{group.skipped.length}</span>
            ) : null}
            {group.passed.length > 0 ? (
              <span className="text-emerald-700 dark:text-emerald-400">✓{group.passed.length}</span>
            ) : null}
            {group.neverRun.length > 0 ? (
              <span className="text-slate-500 dark:text-slate-400">○{group.neverRun.length}</span>
            ) : null}
            <span className="text-muted-foreground">·</span>
            <span className="text-muted-foreground">{total}</span>
          </span>
        </summary>

        <div className="border-t">
          {entries.length === 0 ? (
            <p className="px-3 py-1.5 text-xs text-muted-foreground">—</p>
          ) : (
            entries.map(({ tone, item }) => (
              <TestRow
                key={`row-${tone}-${item.test_file}`}
                item={item}
                tone={tone}
                version={version}
                jiraBase={jiraBase}
                onRun={() => setRunTarget(item)}
              />
            ))
          )}
        </div>
    </details>
    {runTarget && (
      <RunTestDialog
        test={analyticsItemToTestFileInfo(runTarget, productId)}
        open
        onOpenChange={(o) => {
          if (!o) setRunTarget(null);
        }}
        initialPlatform={runTarget.last_platform ?? undefined}
      />
    )}
    </>
  );
}

interface StatusCounts {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
  xfail: number;
  xpass: number;
  not_run: number;
}

function donutFor(counts: StatusCounts, includeNotRun: boolean) {
  const keys: ResultColumnTone[] = ['passed', 'failed', 'skipped', 'xfail', 'xpass'];
  if (includeNotRun) keys.push('not_run');
  const cats = keys.map((k) => ({ ...TONE_META[k], value: counts[k] })).filter((c) => c.value > 0);
  return {
    labels: cats.map((c) => c.label),
    colors: cats.map((c) => c.color),
    series: cats.map((c) => c.value),
  };
}

function donutOptions(
  d: { labels: string[]; colors: string[] },
  theme: ChartTheme
): ApexOptions {
  return {
    labels: d.labels,
    colors: d.colors,
    chart: { type: 'donut', toolbar: { show: false } },
    legend: { position: 'bottom' },
    dataLabels: { enabled: true, formatter: (v) => `${Math.round(Number(v))}%` },
    // The slice separator is the card behind the donut, not white — a white stroke
    // draws light seams across a dark card.
    stroke: { colors: [theme.mode === 'dark' ? '#0f172a' : '#ffffff'] },
    theme: { mode: theme.mode },
  };
}

/** A row of status marker chips (Total + per-status counts). */
function StatChips({
  counts,
  includeNotRun,
}: {
  counts: StatusCounts;
  includeNotRun?: boolean;
}) {
  const keys: ResultColumnTone[] = ['passed', 'failed', 'skipped', 'xfail', 'xpass'];
  if (includeNotRun) keys.push('not_run');
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-xs">
      <span className="rounded-md border bg-muted/40 px-2 py-1 font-medium tabular-nums text-muted-foreground">
        Total {counts.total}
      </span>
      {keys.map((k) => (
        <span key={k} className={`rounded-md border px-2 py-1 tabular-nums ${TONE_META[k].chip}`}>
          {TONE_META[k].label} {counts[k]}
        </span>
      ))}
    </div>
  );
}

export function AnalyticsDashboard({ scope, planId }: AnalyticsDashboardProps) {
  const chartTheme = useChartTheme();
  const [searchParams, setSearchParams] = useSearchParams();
  const { data: products, isLoading: productsLoading } = useProducts();

  const [selectedProductId, setSelectedProductId] = useState(searchParams.get('product_id') || '');
  const [selectedVersion, setSelectedVersion] = useState(searchParams.get('version') || '');
  // Empty string = "All branches" (no branch filter applied).
  const [selectedBranch, setSelectedBranch] = useState(searchParams.get('branch') || '');
  const [daysTrend, setDaysTrend] = useState(Number(searchParams.get('days_trend') || '30'));
  const [groupBy, setGroupBy] = useState<AnalyticsGroupBy>((searchParams.get('group_by') as AnalyticsGroupBy) || 'none');
  const [groupValue, setGroupValue] = useState(searchParams.get('group_value') || '');
  // Empty set = show every status. Populated by clicking the status filter
  // buttons in the "Test results" card; multiple tones can be active at once.
  const [statusFilter, setStatusFilter] = useState<Set<ResultColumnTone>>(new Set());
  const toggleStatusFilter = (tone: ResultColumnTone) => {
    setStatusFilter((prev) => {
      const next = new Set(prev);
      if (next.has(tone)) next.delete(tone);
      else next.add(tone);
      return next;
    });
  };

  const { data: versions } = useObservedProductVersions(selectedProductId);
  // Preserve the backend order (most-recent run first) so the latest versions
  // surface at the top rather than being re-sorted numerically.
  const sortedVersions = useMemo(
    () =>
      [...(versions || [])]
        .map((v) => v.trim())
        .filter((v) => v.length > 0)
        .map((version) => ({ version })),
    [versions]
  );

  const { data: branches, isFetched: branchesFetched } = useObservedProductBranches(selectedProductId);
  // Keep backend order (most-recent run first); drop blanks.
  const branchOptions = useMemo(
    () => [...(branches || [])].map((b) => b.trim()).filter((b) => b.length > 0),
    [branches]
  );
  // "All branches" is a real selectable option (value '') so the searchable
  // combobox can show/clear it like any other entry — branch counts can run
  // into the hundreds, so plain click-to-scroll is no longer practical.
  const branchComboboxOptions = useMemo(
    () => [{ value: '', label: 'All branches' }, ...branchOptions.map((b) => ({ value: b, label: b }))],
    [branchOptions]
  );

  useEffect(() => {
    if (!products?.length) {
      return;
    }

    if (selectedProductId) {
      return;
    }

    const preferred = products.find((item) => item.key.trim().toUpperCase() === 'VHP') || products[0];
    if (preferred) {
      setSelectedProductId(preferred.id);
    }
  }, [products, selectedProductId]);

  useEffect(() => {
    if (!sortedVersions.length) {
      setSelectedVersion('');
      return;
    }

    const hasCurrent = sortedVersions.some((item) => item.version === selectedVersion);
    if (!hasCurrent) {
      setSelectedVersion(sortedVersions[0].version);
    }
  }, [selectedVersion, sortedVersions]);

  // Reset a stale branch filter (e.g. after switching product) back to "all"
  // so we never query for a branch that produced no runs for this product.
  // Gated on `branchesFetched` — `branches` is undefined while the query is
  // in-flight, which would otherwise make `branchOptions` momentarily empty
  // and wipe a URL-seeded `?branch=...` before the real list ever loads.
  useEffect(() => {
    if (branchesFetched && selectedBranch && !branchOptions.includes(selectedBranch)) {
      setSelectedBranch('');
    }
  }, [branchesFetched, branchOptions, selectedBranch]);

  const overviewQuery = useMemo<AnalyticsOverviewQuery | null>(() => {
    if (!selectedProductId || !selectedVersion) {
      return null;
    }

    const query: AnalyticsOverviewQuery = {
      product_id: selectedProductId,
      version: selectedVersion,
      scope,
      days_heatmap: 7,
      days_trend: daysTrend,
      group_by: groupBy,
    };
    if (scope === 'plan' && planId) {
      query.plan_id = planId;
    }
    // A selected branch narrows the run universe: changing it re-keys the
    // React Query cache, so the test list and their latest results are
    // re-collected for that branch.
    if (selectedBranch.trim()) {
      query.branch = selectedBranch.trim();
    }
    if (groupBy !== 'none' && groupValue.trim()) {
      query.group_value = groupValue.trim();
    }
    return query;
  }, [daysTrend, groupBy, groupValue, planId, scope, selectedBranch, selectedProductId, selectedVersion]);

  const { data: overview, isLoading: overviewLoading, isError: overviewError, refetch } = useAnalyticsOverview(
    overviewQuery || {
      product_id: '',
      version: '',
      scope,
      plan_id: planId,
    },
    !!overviewQuery
  );
  const collectCases = useCollectCases();

  useEffect(() => {
    const next = new URLSearchParams(searchParams);
    if (selectedProductId) next.set('product_id', selectedProductId); else next.delete('product_id');
    if (selectedVersion) next.set('version', selectedVersion); else next.delete('version');
    if (selectedBranch.trim()) next.set('branch', selectedBranch.trim()); else next.delete('branch');
    next.set('days_trend', String(daysTrend));
    if (groupBy && groupBy !== 'none') next.set('group_by', groupBy); else next.delete('group_by');
    if (groupValue.trim()) next.set('group_value', groupValue.trim()); else next.delete('group_value');
    if (next.toString() !== searchParams.toString()) {
      setSearchParams(next, { replace: true });
    }
  }, [daysTrend, groupBy, groupValue, selectedBranch, selectedProductId, selectedVersion, searchParams, setSearchParams]);

  const activeGroupValues = useMemo(() => {
    if (!overview) return [];
    if (groupBy === 'component') return overview.grouped.component.map((item) => item.value);
    if (groupBy === 'tag') return overview.grouped.tag.map((item) => item.value);
    return [];
  }, [groupBy, overview]);

  const qualityVectorOptions = useMemo<ApexOptions>(
    () => ({
      chart: { type: 'bar', toolbar: { show: false }, animations: { speed: 450 } },
      colors: ['#0f766e'],
      plotOptions: {
        bar: {
          horizontal: true,
          borderRadius: 4,
          barHeight: '62%',
        },
      },
      xaxis: {
        categories: overview?.quality_vectors?.items.map((item) => item.vector) || [],
        min: 0,
        forceNiceScale: true,
        title: { text: 'Unique test files' },
      },
      yaxis: {
        labels: { maxWidth: 220 },
      },
      dataLabels: { enabled: true },
      legend: { show: false },
      ...chartThemeOptions(chartTheme),
    }),
    [overview, chartTheme]
  );

  const qualityVectorSeries = useMemo(
    () => [
      {
        name: 'Tests',
        data: overview?.quality_vectors?.items.map((item) => item.tests) || [],
      },
    ],
    [overview]
  );

  const buildDistributionOptions = useMemo<ApexOptions>(
    () => {
      // Vertical stacked columns — one column per build, build labels along the
      // bottom, status counts grow upward. Narrow column width keeps the chart
      // tidy when there are only a few builds.
      const buildCount = overview?.build_distribution.length ?? 0;
      const columnWidth =
        buildCount <= 1 ? '20%' : buildCount === 2 ? '32%' : buildCount === 3 ? '44%' : '60%';
      return {
      // Order matches the series below: passed (base, green), failed (red),
      // skipped (amber on top).
      chart: { type: 'bar', toolbar: { show: false }, animations: { speed: 450 }, stacked: true },
      colors: ['#16a34a', '#dc2626', '#f59e0b'],
      xaxis: {
        categories:
          overview?.build_distribution.map((item) =>
            overview.version ? `${overview.version}-${item.build}` : item.build
          ) || [],
        title: { text: 'Build' },
        labels: { rotate: -45, rotateAlways: false, trim: true, hideOverlappingLabels: true },
      },
      yaxis: {
        min: 0,
        forceNiceScale: true,
        title: { text: 'Count' },
      },
      legend: { position: 'top' },
      plotOptions: {
        bar: {
          horizontal: false,
          columnWidth,
          borderRadius: 4,
        },
      },
      tooltip: {
        y: {
          formatter: (value, context) => {
            const dataPointIndex = context?.dataPointIndex;
            const item =
              dataPointIndex === undefined
                ? undefined
                : overview?.build_distribution[dataPointIndex];
            if (!item) {
              return `${value}`;
            }
            const executed = item.executed_total;
            return `${value} (executed: ${executed})`;
          },
        },
      },
      ...chartThemeOptions(chartTheme),
      };
    },
    [overview, chartTheme]
  );

  const buildDistributionSeries = useMemo(
    () => [
      {
        name: 'Passed',
        data: overview?.build_distribution.map((item) => item.passed) || [],
      },
      {
        name: 'Failed',
        data: overview?.build_distribution.map((item) => item.failed) || [],
      },
      {
        // Skipped tests count toward executed_total but not toward passed/failed
        // — surface them so the bar reflects the full executed volume.
        name: 'Skipped',
        data:
          overview?.build_distribution.map((item) =>
            Math.max(0, item.executed_total - item.passed - item.failed)
          ) || [],
      },
    ],
    [overview]
  );

  const trendOptions = useMemo<ApexOptions>(
    () => ({
      chart: { type: 'line', toolbar: { show: false }, animations: { speed: 450 } },
      stroke: { curve: 'smooth', width: 3 },
      colors: ['#16a34a', '#dc2626', '#64748b'],
      xaxis: {
        categories: overview?.trend.points.map((point) => point.day) || [],
        labels: { rotate: -20 },
      },
      yaxis: { min: 0, forceNiceScale: true },
      legend: { position: 'top' },
      ...chartThemeOptions(chartTheme),
    }),
    [overview, chartTheme]
  );

  const trendSeries = useMemo(
    () => [
      { name: 'Passed', data: overview?.trend.points.map((point) => point.passed) || [] },
      { name: 'Failed', data: overview?.trend.points.map((point) => point.failed) || [] },
      { name: 'Not run', data: overview?.trend.points.map((point) => point.not_run) || [] },
    ],
    [overview]
  );

  // Test-case stats (individual functions) straight from the summary.
  const caseCounts = useMemo<StatusCounts>(() => {
    const s = overview?.summary;
    const passed = s?.case_passed ?? 0;
    const failed = s?.case_failed ?? 0;
    const skipped = s?.case_skipped ?? 0;
    const xfail = s?.case_xfail ?? 0;
    const xpass = s?.case_xpass ?? 0;
    const executed = passed + failed + skipped + xfail + xpass;
    // Total = the real number of cases: the exact collected count when present,
    // else the executed count. Statuses are the executed subset; the rest are
    // Not run.
    const total = Math.max(s?.case_expected ?? 0, s?.case_total ?? 0, executed);
    return {
      total,
      passed,
      failed,
      skipped,
      xfail,
      xpass,
      not_run: Math.max(0, total - executed),
    };
  }, [overview]);

  // Test (file) stats, bucketed by each file's effective case status so XFAIL /
  // XPASS surface at the file level too; never-run files count as Not run.
  const testFileCounts = useMemo<StatusCounts>(() => {
    const c: StatusCounts = { total: 0, passed: 0, failed: 0, skipped: 0, xfail: 0, xpass: 0, not_run: 0 };
    if (!overview) return c;
    const items = [...overview.lists.passed, ...overview.lists.failed, ...overview.lists.not_run];
    for (const it of items) {
      c.total++;
      c[effectiveTone(it)]++;
    }
    return c;
  }, [overview]);

  const caseDonut = useMemo(() => donutFor(caseCounts, true), [caseCounts]);
  const testDonut = useMemo(() => donutFor(testFileCounts, true), [testFileCounts]);

  const planGroups = useMemo<PlanGroupData[]>(() => {
    if (!overview) return [];
    const buckets = new Map<string, PlanGroupData>();
    const ensure = (item: AnalyticsListItem): PlanGroupData => {
      const planId = item.plan_id || '__unassigned__';
      let entry = buckets.get(planId);
      if (!entry) {
        entry = {
          planId,
          planName: item.plan_name || item.plan_id || 'Unassigned',
          passed: [],
          failed: [],
          skipped: [],
          neverRun: [],
        };
        buckets.set(planId, entry);
      }
      return entry;
    };
    for (const item of overview.lists.passed) ensure(item).passed.push(item);
    for (const item of overview.lists.failed) ensure(item).failed.push(item);
    for (const item of overview.lists.not_run) {
      const entry = ensure(item);
      if (item.total_runs > 0) entry.skipped.push(item);
      else entry.neverRun.push(item);
    }
    // Plans with at least one failed test float to the top; otherwise alphabetic
    // by plan name so the order is stable across refreshes.
    return [...buckets.values()].sort((a, b) => {
      const aHasFail = a.failed.length > 0 ? 0 : 1;
      const bHasFail = b.failed.length > 0 ? 0 : 1;
      if (aHasFail !== bHasFail) return aHasFail - bHasFail;
      return a.planName.localeCompare(b.planName);
    });
  }, [overview]);

  // CONTRACT-DIFF §9.3, condition and copy taken verbatim. Evaluated only on a
  // settled query, so a loading page shows nothing. `case_expected` is the plan
  // universe qa-catalog resolved and stays accurate; the outcome counts below are
  // 0 when no execution row matched the selected version, which is not the same
  // statement as "nothing passed" — the banner is the one fact no field carries.
  let noExecutionDataCount: number | null = null;
  if (!overviewLoading && !overviewError && overview) {
    const s = overview.summary;
    const executed = s.case_passed + s.case_failed + s.case_skipped + s.case_xfail + s.case_xpass;
    const showNoExecutionData = s.case_expected > 0 && executed === 0;
    noExecutionDataCount = showNoExecutionData ? s.case_expected : null;
  }

  if (productsLoading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {noExecutionDataCount !== null && (
        <UnavailableNotice size="page" title="No execution data for this selection">
          The {noExecutionDataCount} tests below are the plan universe for this product
          and branch, and that count is accurate. Every outcome count is 0 because no
          execution rows matched the selected version — not because those tests failed
          or were skipped. Check the version filter; if no version ever matches, this
          deployment is not recording a product version on its runs.
        </UnavailableNotice>
      )}

      <div className="flex flex-wrap items-center gap-2 rounded-md border bg-card px-3 py-2">
        <Select value={selectedProductId} onValueChange={setSelectedProductId}>
          <SelectTrigger className="h-8 w-[180px]">
            <SelectValue placeholder="Product" />
          </SelectTrigger>
          <SelectContent>
            {(products || []).map((product) => (
              <SelectItem key={product.id} value={product.id}>
                {product.name} ({product.key})
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Select value={selectedVersion} onValueChange={setSelectedVersion}>
          <SelectTrigger className="h-8 w-[140px]">
            <SelectValue placeholder="Version" />
          </SelectTrigger>
          <SelectContent>
            {sortedVersions.map((version) => (
              <SelectItem key={version.version} value={version.version}>
                {version.version}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Combobox
          value={selectedBranch}
          onChange={setSelectedBranch}
          options={branchComboboxOptions}
          placeholder="All branches"
          emptyText="No branches"
          className="w-[200px]"
          triggerClassName="h-8"
        />

        <Select value={groupBy} onValueChange={(value) => setGroupBy(value as AnalyticsGroupBy)}>
          <SelectTrigger className="h-8 w-[150px]">
            <SelectValue placeholder="Group by" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="none">No drill-down</SelectItem>
            <SelectItem value="component">Component</SelectItem>
            <SelectItem value="tag">Tag</SelectItem>
          </SelectContent>
        </Select>

        {groupBy !== 'none' && (
          <Select value={groupValue || '__all__'} onValueChange={(value) => setGroupValue(value === '__all__' ? '' : value)}>
            <SelectTrigger className="h-8 w-[160px]">
              <SelectValue placeholder="Group value" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__all__">All</SelectItem>
              {activeGroupValues.map((value) => (
                <SelectItem key={value} value={value}>
                  {value}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}

        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>Trend</span>
          <Input
            type="number"
            min={7}
            max={365}
            value={daysTrend}
            onChange={(event) => setDaysTrend(Math.min(365, Math.max(7, Number(event.target.value) || 30)))}
            className="h-8 w-[80px]"
          />
          <span>days</span>
        </div>

        <Button
          variant="outline"
          size="sm"
          disabled={collectCases.isPending}
          onClick={() =>
            collectCases.mutate(selectedBranch.trim() || undefined, {
              onSuccess: (d) =>
                toast.success(`Collect launched for ${d.launched} repo(s) on ${d.branch}`, {
                  description: 'Exact case counts update on the next Analytics refresh.',
                }),
              onError: (e) => toast.error('Failed to launch collect', { description: String(e) }),
            })
          }
          title={
            selectedBranch.trim()
              ? `Recount exact test cases (pytest --collect-only) for branch ${selectedBranch.trim()}`
              : 'Recount exact test cases (pytest --collect-only) for the default branch'
          }
        >
          {collectCases.isPending ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : null}
          Collect cases
        </Button>

        <Button variant="ghost" size="icon" onClick={() => refetch()} title="Refresh" aria-label="Refresh">
          <RefreshCcw className="h-4 w-4" />
        </Button>

        {scope === 'plan' && planId ? <Badge variant="outline" className="ml-auto">Plan: {planId}</Badge> : null}
      </div>

      {overviewLoading || !overview ? (
        <div className="flex h-64 items-center justify-center">
          <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : (
        <>
          {/* Stats side by side: test cases (left) and tests/files (right). */}
          <div className="grid gap-4 lg:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>Test Cases</CardTitle>
                <CardDescription>Individual test functions · latest run per test</CardDescription>
              </CardHeader>
              <CardContent className="space-y-3">
                <StatChips counts={caseCounts} includeNotRun />
                {caseDonut.series.length > 0 ? (
                  <ReactApexChart type="donut" options={donutOptions(caseDonut, chartTheme)} series={caseDonut.series} height={200} />
                ) : (
                  <p className="text-sm text-muted-foreground">No case data.</p>
                )}
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Tests</CardTitle>
                <CardDescription>Test files by latest effective status</CardDescription>
              </CardHeader>
              <CardContent className="space-y-3">
                <StatChips counts={testFileCounts} includeNotRun />
                {testDonut.series.length > 0 ? (
                  <ReactApexChart type="donut" options={donutOptions(testDonut, chartTheme)} series={testDonut.series} height={200} />
                ) : (
                  <p className="text-sm text-muted-foreground">No test data.</p>
                )}
              </CardContent>
            </Card>
          </div>

          <div className="grid gap-4 lg:grid-cols-3 lg:items-start">
          <Card className="lg:col-span-2">
            <CardHeader>
              <CardTitle>Test results</CardTitle>
              <CardDescription>
                Grouped by plan · latest result per test{overview.version ? ` for ${overview.version}` : ''}
              </CardDescription>
            </CardHeader>
            <CardContent>
                <div className="mb-3 flex flex-wrap items-center gap-1.5 text-xs">
                  <button
                    type="button"
                    onClick={() => setStatusFilter(new Set())}
                    title="Show every status"
                    aria-pressed={statusFilter.size === 0}
                    className={cn(
                      'rounded-md border px-2 py-1 font-medium tabular-nums transition-colors',
                      statusFilter.size === 0 ? ALL_FILTER_META.active : ALL_FILTER_META.idle
                    )}
                  >
                    All {testFileCounts.total}
                  </button>
                  {(
                    [
                      ['passed', testFileCounts.passed],
                      ['failed', testFileCounts.failed],
                      ['skipped', testFileCounts.skipped],
                      ['xfail', testFileCounts.xfail],
                      ['xpass', testFileCounts.xpass],
                      ['not_run', testFileCounts.not_run],
                    ] as Array<[ResultColumnTone, number]>
                  ).map(([tone, count]) => {
                    const meta = TONE_META[tone];
                    const active = statusFilter.has(tone);
                    return (
                      <button
                        key={tone}
                        type="button"
                        onClick={() => toggleStatusFilter(tone)}
                        title={`Filter test results to ${meta.label}`}
                        aria-pressed={active}
                        className={cn(
                          'rounded-md border px-2 py-1 font-medium tabular-nums transition-colors',
                          active ? meta.active : meta.idle
                        )}
                      >
                        {meta.icon} {count}
                      </button>
                    );
                  })}
                </div>
                <div className="space-y-1">
                  {!planGroups.length ? (
                    <p className="text-sm text-muted-foreground">—</p>
                  ) : (
                    planGroups.map((group) => (
                      <PlanGroup
                        key={group.planId}
                        group={group}
                        version={overview.version}
                        statusFilter={statusFilter}
                        productId={selectedProductId}
                      />
                    ))
                  )}
                </div>
              </CardContent>
            </Card>

            <div className="lg:col-span-1 space-y-4">
              <Card>
                <CardHeader>
                  <CardTitle>Tests per build</CardTitle>
                  <CardDescription>Per-build passed/failed counts</CardDescription>
                </CardHeader>
                <CardContent>
                  {!overview.build_distribution.length ? (
                    <p className="text-sm text-muted-foreground">No completed runs found.</p>
                  ) : (
                    <ReactApexChart type="bar" options={buildDistributionOptions} series={buildDistributionSeries} height={200} />
                  )}
                </CardContent>
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>Quality Vectors</CardTitle>
                  <CardDescription>Unique tests by quality vector for selected product/scope (all versions)</CardDescription>
                </CardHeader>
                <CardContent className="space-y-3">
                  {!(overview.quality_vectors?.items.length ?? 0) ? (
                    <p className="text-sm text-muted-foreground">No quality-vector metadata found in current scope.</p>
                  ) : (
                    <ReactApexChart type="bar" options={qualityVectorOptions} series={qualityVectorSeries} height={260} />
                  )}
                  {(() => {
                    const total = overview.quality_vectors?.total_tests ?? 0;
                    const unclassified = overview.quality_vectors?.unclassified_tests ?? 0;
                    const pct = total > 0 ? Math.round((unclassified / total) * 100) : 0;
                    return (
                      <div className="text-xs text-muted-foreground">
                        Considered {total} test{total === 1 ? '' : 's'} ·{' '}
                        <span className={unclassified > 0 ? 'text-amber-700 dark:text-amber-400' : ''}>
                          {unclassified} / {total} ({pct}%) missing <code className="text-[10px]">TEST_META</code>
                        </span>
                      </div>
                    );
                  })()}
                </CardContent>
              </Card>
            </div>
          </div>

          <div className="grid gap-4 lg:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>{daysTrend}-Day Trend</CardTitle>
                <CardDescription>Daily latest-state counts</CardDescription>
              </CardHeader>
              <CardContent>
                <ReactApexChart type="line" options={trendOptions} series={trendSeries} height={260} />
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Flaky Tests</CardTitle>
                <CardDescription>Pass rate 40%-80% with ≥5 executions in selected trend window</CardDescription>
              </CardHeader>
              <CardContent>
                {!overview.flaky.length ? (
                  <p className="text-sm text-muted-foreground">No flaky tests in current scope.</p>
                ) : (
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Test</TableHead>
                        <TableHead className="text-right">Pass</TableHead>
                        <TableHead className="text-right">Runs</TableHead>
                        <TableHead className="text-right">Counts</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {overview.flaky.map((item) => (
                        <TableRow key={item.test_file}>
                          <TableCell className="max-w-[220px] truncate text-foreground/80" title={item.test_name}>
                            {item.test_name}
                          </TableCell>
                          <TableCell className="text-right tabular-nums">{item.pass_rate}%</TableCell>
                          <TableCell className="text-right tabular-nums text-muted-foreground">{item.executions}</TableCell>
                          <TableCell className="text-right tabular-nums text-xs">
                            <span className="text-emerald-600 dark:text-emerald-400">{item.pass_count}</span>
                            <span className="text-muted-foreground"> / </span>
                            <span className="text-red-600 dark:text-red-400">{item.fail_count}</span>
                            <span className="text-muted-foreground"> / </span>
                            <span className="text-muted-foreground">{item.skipped_count}</span>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                )}
              </CardContent>
            </Card>
          </div>
        </>
      )}
    </div>
  );
}
