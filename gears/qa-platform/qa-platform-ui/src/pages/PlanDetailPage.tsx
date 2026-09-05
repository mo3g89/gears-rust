import { useParams, Link } from 'react-router-dom';
import { useMemo, useState } from 'react';
import { usePlan, useTests, useRunsByPlan } from '@/api/hooks';
import { useSelectedBranch } from '@/lib/selectedBranch';
import { TestFileInfo } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { DescriptionText } from '@/components/ui/description-text';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { Loader2, ArrowLeft, FileCode, Play, CalendarPlus, PlayCircle } from 'lucide-react';
import { CreateScheduleDialog } from '@/components/schedules/CreateScheduleDialog';
import { RunPlanDialog } from '@/components/plans/RunPlanDialog';
import { RunTestDialog } from '@/components/tests/RunTestDialog';

// `phase` is qa-runs' own lowercase `RunState` set (`created | queued |
// dispatching | running | succeeded | failed | canceled | timed_out | expired
// | error` — `RunState::as_str`, qa-runs-sdk/src/models.rs:271-284), passed
// through unchanged by `runFromDto` (decision X4). `'Pending'` and `'Skipped'`
// are left as literal, unmatchable placeholders: this gear's `RunState` has no
// such variant, so there is no real lowercase value to switch them to — see
// the fix report for this file.
function runPhaseDot(phase: string): { dot: string; pulse: boolean } {
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
      return { dot: 'bg-muted-foreground/60', pulse: false };
  }
}

function runRelativeTime(value: string | null): string {
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

/** A run's version label: app version/build if known, else the test ref. */
function runVersionLabel(run: {
  app_version: string | null;
  app_build: string | null;
  test_version: string | null;
}): string | null {
  if (run.app_version) {
    return run.app_build ? `${run.app_version} (${run.app_build})` : run.app_version;
  }
  return run.test_version || null;
}

export function PlanDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [branch] = useSelectedBranch();
  const { data: plan, isLoading, error } = usePlan(id!, branch);
  const { data: tests } = useTests(branch);
  const { data: planRuns } = useRunsByPlan(id!, 15000);
  const [runDialogOpen, setRunDialogOpen] = useState(false);
  const [selectedTest, setSelectedTest] = useState<TestFileInfo | null>(null);
  const [runTestDialogOpen, setRunTestDialogOpen] = useState(false);
  const testsByFile = useMemo(() => {
    const entries = new Map<string, TestFileInfo>();
    if (!plan) {
      return entries;
    }
    for (const item of tests || []) {
      if (item.plan_id === plan.id) {
        entries.set(item.test_file, item);
      }
    }
    return entries;
  }, [plan, tests]);

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
        <p className="text-destructive">Failed to load test plan</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  if (!plan) {
    return null;
  }

  const handleRunTest = (file: string) => {
    const testMeta = testsByFile.get(file);
    setSelectedTest({
      plan_id: plan.id,
      plan_name: plan.plan.name,
      source: plan.source,
      repo_id: plan.repo_id,
      repo_name: plan.repo_name,
      product_id: plan.product_id,
      product_key: plan.product_key,
      product_name: plan.product_name,
      test_file: file,
      title: testMeta?.title,
      component: testMeta?.component,
      tags: testMeta?.tags || plan.plan.tags,
      quality_vectors: testMeta?.quality_vectors,
      versions: testMeta?.versions || [],
      loc: testMeta?.loc,
    });
    setRunTestDialogOpen(true);
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/plans">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div className="flex-1">
          <div className="flex items-center gap-2">
            <h1 className="text-xl font-semibold">{plan.plan.name || 'Unknown Plan'}</h1>
            {plan.plan.validation && (
              <Badge
                variant="outline"
                className="px-1 py-0 text-[9px] font-normal uppercase tracking-wide text-muted-foreground"
              >
                Validation
              </Badge>
            )}
            {plan.source === 'repo' && plan.repo_name ? (
              <Badge variant="outline" className="text-xs uppercase tracking-wide bg-muted/50">
                Repo: {plan.repo_name}
              </Badge>
            ) : (
              <Badge variant="outline" className="text-xs uppercase tracking-wide text-muted-foreground bg-muted/20">
                Local
              </Badge>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={() => setRunDialogOpen(true)}>
            <PlayCircle className="mr-2 h-4 w-4" />
            Run Now
          </Button>
          <CreateScheduleDialog
            initialPlanId={plan.id}
            trigger={
              <Button variant="outline">
                <CalendarPlus className="mr-2 h-4 w-4" />
                Schedule
              </Button>
            }
          />
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Configuration</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {plan.plan.description && (
            <div>
              <p className="text-sm font-medium mb-1">Description</p>
              <DescriptionText text={plan.plan.description} className="text-muted-foreground" collapsible />
            </div>
          )}

          <div>
            <p className="text-sm font-medium mb-2">Product</p>
            {plan.product_key ? (
              <div className="flex items-center gap-2">
                <Badge variant="outline">{plan.product_key}</Badge>
                {plan.product_name && <span className="text-sm text-muted-foreground">{plan.product_name}</span>}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">Not linked</p>
            )}
          </div>

          {plan.plan.tags && plan.plan.tags.length > 0 && (
            <div>
              <p className="text-sm font-medium mb-2">Tags</p>
              <div className="flex flex-wrap gap-1">
                {plan.plan.tags.map((tag) => (
                  <Badge key={tag} variant="secondary">
                    {tag}
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {plan.plan.node_selector && Object.keys(plan.plan.node_selector).length > 0 && (
            <div>
              <p className="text-sm font-medium mb-2">Node Selector</p>
              <div className="space-y-1">
                {Object.entries(plan.plan.node_selector).map(([key, value]) => (
                  <div key={key} className="text-sm text-muted-foreground">
                    <code className="bg-muted px-1 py-0.5 rounded">{key}</code>: {String(value)}
                  </div>
                ))}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Runs</CardTitle>
          <CardDescription>
            <PlayCircle className="inline h-4 w-4 mr-1" />
            Recent runs of this plan{planRuns ? ` · ${planRuns.length}` : ''}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Status</TableHead>
                  <TableHead>Version</TableHead>
                  <TableHead>Target</TableHead>
                  <TableHead>Started</TableHead>
                  <TableHead className="text-right">Duration</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {planRuns && planRuns.length > 0 ? (
                  planRuns.map((run) => {
                    const phase = runPhaseDot(run.phase);
                    const version = runVersionLabel(run);
                    return (
                      <TableRow key={run.name}>
                        <TableCell>
                          <Link
                            to={`/runs/${encodeURIComponent(run.name)}`}
                            className="flex items-center gap-2 text-foreground/80 hover:text-foreground hover:underline"
                          >
                            <span className="relative inline-flex h-2 w-2 items-center justify-center">
                              {phase.pulse && (
                                <span className={`absolute inline-flex h-full w-full rounded-full opacity-70 animate-ping ${phase.dot}`} />
                              )}
                              <span className={`relative inline-flex h-2 w-2 rounded-full ${phase.dot}`} />
                            </span>
                            {run.phase}
                          </Link>
                        </TableCell>
                        <TableCell>
                          {version ? (
                            <Badge variant="outline" className="font-mono text-[10px]">{version}</Badge>
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell>
                          {run.platform ? (
                            <Link
                              to={`/environments/${encodeURIComponent(run.platform)}`}
                              className="text-foreground/80 hover:text-foreground hover:underline"
                            >
                              {run.platform}
                            </Link>
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell className="text-muted-foreground" title={run.started_at || undefined}>
                          {runRelativeTime(run.started_at) || '—'}
                        </TableCell>
                        <TableCell className="text-right tabular-nums text-muted-foreground">
                          {run.duration || '—'}
                        </TableCell>
                      </TableRow>
                    );
                  })
                ) : (
                  <TableRow>
                    <TableCell colSpan={5} className="text-center text-muted-foreground">
                      No runs yet
                    </TableCell>
                  </TableRow>
                )}
              </TableBody>
            </Table>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Test Files</CardTitle>
          <CardDescription>
            <FileCode className="inline h-4 w-4 mr-1" />
            {plan.test_files.length} file{plan.test_files.length !== 1 ? 's' : ''}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Test File</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {plan.test_files && plan.test_files.length > 0 ? (
                  plan.test_files.map((file) => {
                    return (
                      <TableRow key={file}>
                        <TableCell className="font-mono text-sm">{file}</TableCell>
                        <TableCell className="text-right">
                          <div className="flex justify-end gap-2">
                            <Button
                              size="sm"
                              variant="ghost"
                              onClick={() => handleRunTest(file)}
                            >
                              <Play className="h-4 w-4 mr-1" />
                              Run
                            </Button>
                            <CreateScheduleDialog
                              initialPlanId={plan.id}
                              testFile={file}
                              trigger={
                                <Button size="sm" variant="outline">
                                  <CalendarPlus className="h-4 w-4 mr-1" />
                                  Schedule
                                </Button>
                              }
                            />
                          </div>
                        </TableCell>
                      </TableRow>
                    );
                  })
                ) : (
                  <TableRow>
                    <TableCell colSpan={2} className="text-center text-muted-foreground">
                      No test files
                    </TableCell>
                  </TableRow>
                )}
              </TableBody>
            </Table>
          </div>
        </CardContent>
      </Card>

      <RunPlanDialog
        plan={plan}
        open={runDialogOpen}
        onOpenChange={setRunDialogOpen}
      />

      {selectedTest && (
        <RunTestDialog
          test={selectedTest}
          open={runTestDialogOpen}
          onOpenChange={(open) => {
            setRunTestDialogOpen(open);
            if (!open) {
              setSelectedTest(null);
            }
          }}
        />
      )}
    </div>
  );
}
