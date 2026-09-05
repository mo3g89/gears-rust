import { useState } from 'react';
import { useParams, Link, useNavigate } from 'react-router-dom';
import { toast } from 'sonner';
import { useRun, useDeleteRun, useRerunRun, isQueued } from '@/api/hooks';
import { WorkflowRun, isActiveRun } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { RunPhaseBadge } from '@/components/runs/RunPhaseBadge';
import { RunResultSummary, SucceededWithSkipsMarker } from '@/components/runs/RunResultSummary';
import { TestResultsTable } from '@/components/runs/TestResultsTable';
import { LogViewer } from '@/components/runs/LogViewer';
import { UnavailableNotice } from '@/components/ui/unavailable';
import { Loader2, ArrowLeft, Clock, Calendar, RotateCcw, Square, AlertTriangle } from 'lucide-react';

function getTriggerLabel(run: WorkflowRun): string {
  const normalized = (run.run_source || 'manual').trim().toLowerCase();
  return normalized === 'scheduled' ? 'Scheduled' : 'Manual';
}

function getSourceRefLabel(run: WorkflowRun): string {
  if ((run.source_ref_kind || '').trim().toLowerCase() === 'folder') {
    return 'Folder';
  }

  return 'Branch';
}

export function RunDetailPage() {
  const { name } = useParams<{ name: string }>();
  const navigate = useNavigate();
  const { data, isLoading, error } = useRun(name!, 5000);
  const stopRun = useDeleteRun();
  const rerunRun = useRerunRun();
  const [isStopping, setIsStopping] = useState(false);
  const confirm = useConfirm();

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
        <p className="text-destructive">Failed to load run details</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  if (!data) {
    return null;
  }

  const { run, test_results } = data;
  const triggerLabel = getTriggerLabel(run);
  const repositoryLabel = run.repo_name || run.repo_id || null;
  const sourceRefLabel = getSourceRefLabel(run);
  const activeRun = isActiveRun(run);
  // Task 10: a skip no longer fails a run, so a `succeeded` run may have
  // asserted less than the word implies. Computed once and used at both the
  // header badge and the Test Results card, rather than re-deriving it twice.
  // `run.phase` is qa-runs' lowercase state set (`runFromDto` sets it to
  // `dto.state` "without re-casing", adapters.ts:378-383, decision X4) - not
  // Title Case, so the comparison below must match that, not `RunPhaseBadge`'s
  // (separate, pre-existing) Title-Case switch.
  const skippedCount = run.result?.skipped ?? 0;
  const succeededWithSkips = run.phase === 'succeeded' && skippedCount > 0;

  const handleStopRun = async () => {
    if (!activeRun) return;
    const ok = await confirm({
      title: `Stop run "${run.name}"?`,
      description: 'The running workflow will be terminated.',
      confirmText: 'Stop run',
      variant: 'destructive',
    });
    if (!ok) return;

    setIsStopping(true);
    stopRun.mutate(run.name, {
      onSuccess: () => {
        setIsStopping(false);
        toast.success(`Run "${run.name}" is stopping`);
      },
      onError: (err) => {
        setIsStopping(false);
        toast.error('Failed to stop run', { description: String(err) });
      },
    });
  };

  const handleRerun = () => {
    rerunRun.mutate(run.name, {
      onSuccess: (result) => {
        if (isQueued(result)) {
          toast.success(`Run "${run.name}" queued`, {
            description: 'The environment is busy with an exclusive run. It will start automatically.',
            duration: 8000,
          });
          return;
        }
        toast.success(`Run "${run.name}" restarted`);
        navigate(`/runs/${result.workflow_name}`);
      },
      onError: (err) => {
        toast.error('Failed to rerun', { description: String(err) });
      },
    });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/runs">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div className="flex-1">
          <h1 className="text-xl font-semibold">{run.name}</h1>
          <p className="text-muted-foreground">
            Plan: <Link to={`/plans/${run.plan_id}`} className="hover:underline">{run.plan_id}</Link>
          </p>
        </div>
        <RunPhaseBadge phase={run.phase} />
        {succeededWithSkips && <SucceededWithSkipsMarker skipped={skippedCount} />}
        {activeRun ? (
          <Button
            variant="outline"
            size="sm"
            onClick={handleStopRun}
            disabled={isStopping}
            className="text-destructive hover:text-destructive"
            title="Stop run"
          >
            {isStopping ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Square className="h-3.5 w-3.5 fill-current" />
            )}
            Stop
          </Button>
        ) : (
          <Button
            variant="outline"
            size="sm"
            onClick={handleRerun}
            disabled={rerunRun.isPending}
          >
            {rerunRun.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <RotateCcw className="h-4 w-4" />
            )}
            Rerun
          </Button>
        )}
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Run Metadata</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <div className="flex items-center gap-2 text-sm">
                <Calendar className="h-4 w-4 text-muted-foreground" />
                <span className="font-medium">Started:</span>
                <span className="text-muted-foreground">
                  {run.started_at ? new Date(run.started_at).toLocaleString() : '-'}
                </span>
              </div>
              
              <div className="flex items-center gap-2 text-sm">
                <Calendar className="h-4 w-4 text-muted-foreground" />
                <span className="font-medium">Finished:</span>
                <span className="text-muted-foreground">
                  {run.finished_at ? new Date(run.finished_at).toLocaleString() : '-'}
                </span>
              </div>
              
              <div className="flex items-center gap-2 text-sm">
                <Clock className="h-4 w-4 text-muted-foreground" />
                <span className="font-medium">Duration:</span>
                <span className="text-muted-foreground">{run.duration || '-'}</span>
              </div>
            </div>

            <div className="space-y-2">
              <div className="text-sm">
                <span className="font-medium">Execution Trigger:</span>
                <span className="ml-2 text-muted-foreground">{triggerLabel}</span>
              </div>

              {run.exclusive !== null && run.exclusive !== undefined && (
                <div className="text-sm">
                  <span className="font-medium">Environment Access:</span>
                  <span className="ml-2 text-muted-foreground">
                    {run.exclusive
                      ? 'Exclusive — this run held the environment on its own'
                      : 'Parallel'}
                  </span>
                </div>
              )}

              {repositoryLabel && (
                <div className="text-sm">
                  <span className="font-medium">Repository:</span>
                  <span className="ml-2 text-muted-foreground">{repositoryLabel}</span>
                  {run.repo_name && run.repo_id && run.repo_name !== run.repo_id && (
                    <span className="ml-2 text-muted-foreground/70">({run.repo_id})</span>
                  )}
                </div>
              )}

              {run.source_ref && (
                <div className="text-sm">
                  <span className="font-medium">{sourceRefLabel}:</span>
                  <span className="ml-2 text-muted-foreground">{run.source_ref}</span>
                </div>
              )}

              {run.platform && (
                <div className="text-sm">
                  <span className="font-medium">Environment:</span>
                  <Link
                    to={`/environments/${encodeURIComponent(run.platform)}`}
                    className="ml-2 text-muted-foreground hover:text-foreground hover:underline"
                  >
                    {run.platform}
                  </Link>
                </div>
              )}

              {run.app_version && (
                <div className="text-sm">
                  <span className="font-medium">App Version:</span>
                  <span className="ml-2 text-muted-foreground">{run.app_version}</span>
                </div>
              )}

              {run.test_version && (
                <div className="text-sm">
                  <span className="font-medium">Test Version:</span>
                  <span className="ml-2 text-muted-foreground">{run.test_version}</span>
                </div>
              )}

              {run.parameters && run.parameters.length > 0 && (
                <div className="text-sm">
                  <span className="font-medium">Parameters:</span>
                  <div className="mt-1 flex flex-wrap gap-1.5">
                    {run.parameters.map((parameter, index) => (
                      <span
                        key={`${parameter.name}-${index}`}
                        className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground"
                      >
                        {parameter.name}={parameter.value}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {run.message && (
                <div className="text-sm">
                  <span className="font-medium">Message:</span>
                  <p className="mt-1 text-muted-foreground">{run.message}</p>
                </div>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0">
          <div>
            <CardTitle>Test Results</CardTitle>
            <CardDescription>Individual test execution results</CardDescription>
          </div>
          <RunResultSummary result={run.result} className="text-sm" />
        </CardHeader>
        <CardContent className="space-y-3">
          {succeededWithSkips && (
            <div className="flex items-start gap-2 rounded-md border border-amber-300/60 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-800/60 dark:bg-amber-950/30 dark:text-amber-300">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
              <span>
                This run succeeded, but {skippedCount} test{skippedCount === 1 ? '' : 's'}{' '}
                {skippedCount === 1 ? 'was' : 'were'} skipped rather than executed. A skip no
                longer fails a run (product owner decision, 2026-08-28) because environment-gated
                skips are common in these suites — but a green result here does not mean every
                test in the plan ran.
              </span>
            </div>
          )}
          <UnavailableNotice title="Per-test logs are not available in this deployment">
            A run's stored outcomes carry no per-test log slice, so a failed test shows
            its status but no error detail of its own. The whole run's output is in the
            log viewer below.
          </UnavailableNotice>
          <TestResultsTable results={test_results} />
        </CardContent>
      </Card>

      <LogViewer runName={run.name} isTerminal={!activeRun} />
    </div>
  );
}
