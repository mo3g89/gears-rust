import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useRuns, useDeleteRun, useEnvironments } from '@/api/hooks';
import { QueuedRunsCard } from '@/components/runs/QueuedRunsCard';
import { RunsTable } from '@/components/runs/RunsTable';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { toast } from 'sonner';
import { Loader2, RefreshCw, ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight, PlayCircle } from 'lucide-react';
import { isActiveRun, isCollectRun, WorkflowRun } from '@/api/types';
import { compileFql } from '@/lib/fql';
import { FqlQueryInput } from '@/components/filters/FqlQueryInput';
import { cn } from '@/lib/utils';

const PER_PAGE_OPTIONS = [10, 25, 50, 100];

// Quick status filter chips. Each chip maps to one or more run phases; "Failed"
// covers both Failed and Error (mirrors the phase color grouping).
//
// `phases` values are compared straight against `run.phase`, which is qa-runs'
// own lowercase `RunState` set (`created | queued | dispatching | running |
// succeeded | failed | canceled | timed_out | expired | error` —
// `RunState::as_str`, qa-runs-sdk/src/models.rs:271-284), passed through
// unchanged by `runFromDto` (decision X4, `adapters.ts`). These used to be
// Title-Case and so matched zero rows for every chip — this was the worst of
// the casing bugs: picking any chip filtered the run list to nothing and every
// chip count read zero. `'Pending'` and `'Skipped'` are left as literal,
// unmatchable placeholders: this gear's `RunState` has no `Pending` or
// `Skipped` variant at all, so there is no real lowercase value to switch them
// to — see the fix report for this file.
const STATUS_CHIPS: { key: string; label: string; phases: string[] }[] = [
  { key: 'Running', label: 'Running', phases: ['running'] },
  { key: 'Pending', label: 'Pending', phases: ['Pending'] },
  { key: 'Succeeded', label: 'Succeeded', phases: ['succeeded'] },
  { key: 'Failed', label: 'Failed', phases: ['failed', 'error'] },
  { key: 'Skipped', label: 'Skipped', phases: ['Skipped'] },
];

// Quick-filter chip, styled to match the logs view (LogViewer's FilterChip).
function StatusChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'rounded-full border px-2.5 py-1 text-[11px] transition-colors',
        active
          ? 'border-primary/50 bg-primary/15 text-primary'
          : 'border-border bg-muted/30 text-muted-foreground hover:bg-muted/60'
      )}
    >
      {children}
    </button>
  );
}

export function RunsPage() {
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [page, setPage] = useState(1);
  const [perPage, setPerPage] = useState(50);
  const [query, setQuery] = useState('');
  const [selectedStatuses, setSelectedStatuses] = useState<Set<string>>(new Set());
  // Collect runs are hidden until asked for -- see `showCollectRuns`'s chip below.
  const [showCollectRuns, setShowCollectRuns] = useState(false);
  const [stoppingRunName, setStoppingRunName] = useState<string | null>(null);

  // Fetch all runs at once for client-side filtering and pagination
  const { data: response, isLoading, error, refetch } = useRuns(autoRefresh ? 5000 : undefined, 1, 10000);
  const { data: environments } = useEnvironments();
  const deleteRun = useDeleteRun();
  const confirm = useConfirm();

  // Extract all runs from the response
  const allRuns: WorkflowRun[] = useMemo(() => {
    if (Array.isArray(response)) return response;
    if (response && typeof response === 'object' && 'runs' in response) {
      return (response as any).runs || [];
    }
    return [];
  }, [response]);

  // ---------------------------------------------------------------------------
  // COLLECT RUNS ARE NOT TEST RUNS, AND ARE NOT LISTED WITH THEM
  //
  // The source system creates a run row for every collect workflow and then
  // keeps it out of every listing: `list_runs_with_history` filters
  // `run_source != "collect"` with the comment *"Collect-only workflows aren't
  // real test runs -- keep them out of every run listing / plan recent-runs
  // view"* (`manager/src/services/run_history.rs:382-386`). This port had no
  // equivalent, so qa-insights' hourly cycle put 24 rows a day into this table
  // -- for one repository -- among the runs an operator actually came to see.
  //
  // HIDDEN HERE AND NOT IN `GET /qa/v1/runs`, deliberately. A listing endpoint
  // that silently omits rows is a trap for every other consumer of it (legacy
  // has exactly that trap), while "not a real test run" is a presentation
  // judgement. The API keeps answering for them, `$filter=run_kind eq
  // 'collect'` still works, and a collect run's own page still opens by URL
  // with its log intact.
  //
  // AND THEY ARE NOT SILENTLY DROPPED: the chip below carries their count, so
  // the hidden rows are visible AS a number and one click away.
  // ---------------------------------------------------------------------------
  const collectRunCount = useMemo(() => allRuns.filter(isCollectRun).length, [allRuns]);

  const listedRuns = useMemo(
    () => (showCollectRuns ? allRuns : allRuns.filter((run) => !isCollectRun(run))),
    [allRuns, showCollectRuns]
  );

  const fqlFields = useMemo(
    () => [
      'name',
      'run',
      'plan',
      'planId',
      'status',
      'phase',
      // `kind` is filterable so a collect run can be found by what it IS, not
      // only by the chip: `kind:collect` narrows the list once the chip has
      // included them, and `-kind:collect` excludes them again.
      'kind',
      'environment',
      // `platform` is the field's retired name (ruling G-5): unlike the OData
      // `$filter` field this UI never authors by hand, this one is typed by an
      // operator and persisted in `localStorage['qa:fql:saved:runs']`, so the old
      // spelling stays a working alias rather than becoming a silent zero-match.
      'platform',
      'version',
      'appVersion',
      'testVersion',
      'validation',
      'active',
      'duration',
      'started',
      'finished',
      'message',
    ],
    []
  );

  const fqlValues = useMemo(() => {
    const collect = (items: string[]) => Array.from(new Set(items.filter(Boolean))).sort();
    return {
      name: collect(listedRuns.map((run) => run.name)),
      run: collect(listedRuns.map((run) => run.name)),
      plan: collect(listedRuns.map((run) => run.plan_id)),
      planid: collect(listedRuns.map((run) => run.plan_id)),
      status: collect(listedRuns.map((run) => run.phase)),
      phase: collect(listedRuns.map((run) => run.phase)),
      kind: collect(listedRuns.map((run) => run.run_kind || '')),
      environment: collect(listedRuns.map((run) => run.platform || '')),
      // Deprecated alias — see the `fqlFields` comment above.
      platform: collect(listedRuns.map((run) => run.platform || '')),
      version: collect(listedRuns.map((run) => run.app_version || '')),
      appversion: collect(listedRuns.map((run) => run.app_version || '')),
      testversion: collect(listedRuns.map((run) => run.test_version || '')),
      validation: ['true', 'false'],
      active: ['true', 'false'],
      duration: collect(listedRuns.map((run) => run.duration || '')),
      started: collect(listedRuns.map((run) => run.started_at || '')),
      finished: collect(listedRuns.map((run) => run.finished_at || '')),
      message: collect(listedRuns.map((run) => run.message || '')),
    } as Record<string, string[]>;
  }, [listedRuns]);

  const compiled = useMemo(
    () =>
      compileFql<WorkflowRun>(
        query,
        (run, field) => {
          switch (field) {
            case 'name':
            case 'run':
              return run.name;
            case 'plan':
            case 'planid':
              return run.plan_id;
            case 'status':
            case 'phase':
              return run.phase;
            case 'kind':
              return run.run_kind || '';
            case 'environment':
            // Deprecated alias — see the `fqlFields` comment above. Accepting it here,
            // rather than only in the field list, is what actually prevents the silent
            // zero-match: `compileFql` does not validate field names against `fqlFields`
            // at all, so an unhandled case would fall to this switch's own `default:`
            // (`undefined`) regardless of what the autocomplete advertises.
            case 'platform':
              return run.platform || '';
            case 'version':
            case 'appversion':
              return run.app_version || '';
            case 'testversion':
              return run.test_version || '';
            case 'validation':
              return run.is_validation ? 'true' : 'false';
            case 'active':
              return isActiveRun(run) ? 'true' : 'false';
            case 'duration':
              return run.duration || '';
            case 'started':
              return run.started_at || '';
            case 'finished':
              return run.finished_at || '';
            case 'message':
              return run.message || '';
            default:
              return undefined;
          }
        },
        (run) => [
          run.name,
          run.plan_id,
          run.phase,
          run.run_kind || '',
          run.platform || '',
          run.app_version || '',
          run.test_version || '',
          run.is_validation ? 'validation' : '',
          isActiveRun(run) ? 'active' : '',
          run.duration || '',
          run.started_at || '',
          run.finished_at || '',
          run.message || '',
        ].join(' ')
      ),
    [query]
  );

  const filteredRuns = useMemo(
    () =>
      listedRuns
        .filter((run) => compiled.matches(run)),
    [listedRuns, compiled]
  );

  // Per-phase counts over the FQL-filtered set (before the status quick-filter),
  // so each chip's count stays stable while toggling chips.
  const statusCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const run of filteredRuns) {
      counts[run.phase] = (counts[run.phase] || 0) + 1;
    }
    return counts;
  }, [filteredRuns]);

  // Apply the status quick-filter (OR across selected chips) on top of FQL.
  const phaseFilteredRuns = useMemo(() => {
    if (selectedStatuses.size === 0) return filteredRuns;
    const allowed = new Set<string>();
    for (const chip of STATUS_CHIPS) {
      if (selectedStatuses.has(chip.key)) chip.phases.forEach((p) => allowed.add(p));
    }
    return filteredRuns.filter((run) => allowed.has(run.phase));
  }, [filteredRuns, selectedStatuses]);

  const environmentBuildByName = useMemo(() => {
    const map = new Map<string, string>();
    for (const environment of environments || []) {
      map.set(environment.name, environment.build?.trim() || '');
    }
    return map;
  }, [environments]);

  // Pagination computed from filtered runs (FQL + status quick-filter)
  const totalFiltered = phaseFilteredRuns.length;
  const totalPages = Math.max(1, Math.ceil(totalFiltered / perPage));
  const safePage = Math.min(page, totalPages);
  const startIdx = (safePage - 1) * perPage;
  const pageRuns = phaseFilteredRuns.slice(startIdx, startIdx + perPage);

  useEffect(() => {
    setPage(1);
  }, [query, perPage, selectedStatuses]);

  const handlePerPageChange = (newPerPage: number) => {
    setPerPage(newPerPage);
    setPage(1);
  };

  const toggleStatus = (key: string) => {
    setSelectedStatuses((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const handleStop = async (name: string) => {
    const ok = await confirm({
      title: `Stop run "${name}"?`,
      description: 'The running workflow will be terminated.',
      confirmText: 'Stop run',
      variant: 'destructive',
    });
    if (ok) {
      setStoppingRunName(name);
      deleteRun.mutate(name, {
        onSuccess: () => {
          setStoppingRunName(null);
          toast.success(`Run "${name}" is stopping`);
        },
        onError: (err) => {
          setStoppingRunName(null);
          toast.error('Failed to stop run', { description: String(err) });
        },
      });
    }
  };

  const handleRefresh = () => {
    refetch();
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Failed to load test runs</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  const activeRuns = filteredRuns.filter(isActiveRun);
  const hasActiveRuns = activeRuns.length > 0;

  // Build page number buttons: first, last, and a window around current
  const pageButtons = buildPageButtons(safePage, totalPages);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold flex items-center gap-2">
            <PlayCircle className="h-5 w-5" />
            Test Runs
          </h1>
          <p className="text-muted-foreground">
            View and manage workflow executions — every run in this deployment, not
            scoped to the selected product
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setAutoRefresh(!autoRefresh)}
          >
            {autoRefresh ? 'Auto-refresh: ON' : 'Auto-refresh: OFF'}
          </Button>
          <Button variant="outline" size="icon" onClick={handleRefresh}>
            <RefreshCw className="h-4 w-4" />
          </Button>
        </div>
      </div>

      {hasActiveRuns && (
        <div className="bg-blue-50 dark:bg-blue-950 border border-blue-200 dark:border-blue-800 rounded-lg p-4">
          <p className="text-sm text-blue-900 dark:text-blue-200">
            {activeRuns.length} active run{activeRuns.length !== 1 ? 's' : ''} in progress
          </p>
        </div>
      )}

      <QueuedRunsCard />

      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle>All Runs</CardTitle>
              <CardDescription>
                Showing {totalFiltered > 0 ? startIdx + 1 : 0}-{Math.min(startIdx + perPage, totalFiltered)} of {totalFiltered} runs
                {totalFiltered !== allRuns.length && (
                  <span className="ml-1">({allRuns.length} total)</span>
                )}
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="mb-4 p-3 bg-muted/50 rounded-lg border space-y-2">
            <div className="text-xs text-muted-foreground font-medium">FQL Filter</div>
            <FqlQueryInput
              value={query}
              onChange={setQuery}
              placeholder='Example: status = Failed AND environment = staging AND active = true'
              fields={fqlFields}
              valueSuggestions={fqlValues}
              savedFiltersKey="runs"
            />
            <div className="text-xs text-muted-foreground">
              Fields: <code>name</code>, <code>plan</code>, <code>status</code>, <code>environment</code>, <code>version</code>, <code>testVersion</code>, <code>validation</code>, <code>active</code>.
            </div>
            {compiled.error && (
              <div className="text-xs text-amber-600">
                Invalid FQL ({compiled.error}). Using plain text search fallback.
              </div>
            )}
          </div>

          <div className="mb-4 flex flex-wrap items-center gap-2">
            <StatusChip active={selectedStatuses.size === 0} onClick={() => setSelectedStatuses(new Set())}>
              All {filteredRuns.length}
            </StatusChip>
            {STATUS_CHIPS.map((chip) => {
              const count = chip.phases.reduce((n, p) => n + (statusCounts[p] || 0), 0);
              return (
                <StatusChip
                  key={chip.key}
                  active={selectedStatuses.has(chip.key)}
                  onClick={() => toggleStatus(chip.key)}
                >
                  {chip.label}{count > 0 ? ` ${count}` : ''}
                </StatusChip>
              );
            })}
            {/* Only when there is something to unhide: a chip reading "Collect 0"
                would advertise a run kind this deployment never launches. */}
            {collectRunCount > 0 && (
              <StatusChip
                active={showCollectRuns}
                onClick={() => setShowCollectRuns((shown) => !shown)}
              >
                Collect {collectRunCount}
              </StatusChip>
            )}
          </div>

          <RunsTable
            runs={pageRuns}
            environmentBuildByName={environmentBuildByName}
            onStop={handleStop}
            stoppingRunName={stoppingRunName}
          />

          {/* Pagination controls */}
          <div className="flex items-center justify-between mt-4 pt-4 border-t">
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">Per page:</span>
              <select
                className="h-8 text-sm border border-input rounded-md bg-background px-2 py-1 focus:outline-none focus:ring-1 focus:ring-ring"
                value={perPage}
                onChange={(e) => handlePerPageChange(Number(e.target.value))}
              >
                {PER_PAGE_OPTIONS.map((opt) => (
                  <option key={opt} value={opt}>{opt}</option>
                ))}
              </select>
              <span className="text-sm text-muted-foreground ml-2">
                {totalFiltered} run{totalFiltered !== 1 ? 's' : ''}
              </span>
            </div>

            {totalPages > 1 && (
              <div className="flex items-center gap-1">
                <Button
                  variant="outline"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => setPage(1)}
                  disabled={safePage === 1}
                  title="First page"
                >
                  <ChevronsLeft className="h-4 w-4" />
                </Button>
                <Button
                  variant="outline"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                  disabled={safePage === 1}
                  title="Previous page"
                >
                  <ChevronLeft className="h-4 w-4" />
                </Button>

                {pageButtons.map((btn, idx) =>
                  btn === '...' ? (
                    <span key={`ellipsis-${idx}`} className="px-1 text-sm text-muted-foreground">...</span>
                  ) : (
                    <Button
                      key={btn}
                      variant={btn === safePage ? 'default' : 'outline'}
                      size="sm"
                      className="h-8 w-8 p-0"
                      onClick={() => setPage(btn as number)}
                    >
                      {btn}
                    </Button>
                  )
                )}

                <Button
                  variant="outline"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                  disabled={safePage >= totalPages}
                  title="Next page"
                >
                  <ChevronRight className="h-4 w-4" />
                </Button>
                <Button
                  variant="outline"
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => setPage(totalPages)}
                  disabled={safePage >= totalPages}
                  title="Last page"
                >
                  <ChevronsRight className="h-4 w-4" />
                </Button>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

/** Build a window of page numbers around the current page, always showing first and last. */
function buildPageButtons(current: number, total: number): (number | '...')[] {
  if (total <= 7) {
    return Array.from({ length: total }, (_, i) => i + 1);
  }

  const pages: (number | '...')[] = [];
  const windowSize = 1; // pages around current
  const start = Math.max(2, current - windowSize);
  const end = Math.min(total - 1, current + windowSize);

  pages.push(1);
  if (start > 2) pages.push('...');
  for (let i = start; i <= end; i++) {
    pages.push(i);
  }
  if (end < total - 1) pages.push('...');
  pages.push(total);

  return pages;
}
