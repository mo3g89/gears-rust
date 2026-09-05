import { useParams, Link } from 'react-router-dom';
import { useMemo, useState } from 'react';
import { useCustomPlan, useCustomPlans, usePlans } from '@/api/hooks';
import { useSelectedBranch } from '@/lib/selectedBranch';
import { resolveCustomPlanTests } from '@/lib/customPlanTests';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { Loader2, ArrowLeft, FileCode, PlayCircle, CalendarPlus, Pencil } from 'lucide-react';
import { CreateScheduleDialog } from '@/components/schedules/CreateScheduleDialog';
import { RunCustomPlanDialog } from '@/components/custom-plans/RunCustomPlanDialog';

export function CustomPlanDetailPage() {
  const { id } = useParams<{ id: string }>();
  const { data: plan, isLoading, error } = useCustomPlan(id!);
  const [branch] = useSelectedBranch();
  // Standard plans are scoped to this plan's own product (not the globally-active
  // one) so the resolved test count/list stays correct even if a different product
  // is active elsewhere in the app — e.g. after a bookmark, browser back/forward,
  // or a stale tab left open across a product switch. Both are disabled until
  // `plan` itself has loaded: starting `usePlans` against the *active* product
  // first and then switching to `plan.product_id` once known would briefly show
  // the active product's (wrong) list via its `keepPreviousData`. Custom plans
  // take no product scope at all — see `useCustomPlans`.
  const { data: standardPlans } = usePlans(branch, plan?.product_id, !!plan);
  const { data: customPlans } = useCustomPlans(!!plan);
  const [runDialogOpen, setRunDialogOpen] = useState(false);

  // "Whole plan" / "dependencies" custom plans don't populate their own
  // `tests` array (it's resolved at run time) — resolve the effective set
  // here so the counts/table below reflect what will actually run.
  const effectiveTests = useMemo(
    () => (plan ? resolveCustomPlanTests(plan, standardPlans ?? [], customPlans ?? []) : []),
    [plan, standardPlans, customPlans]
  );

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
        <p className="text-destructive">Failed to load custom plan</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  if (!plan) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Custom plan not found</p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/plans?tab=custom">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div className="flex-1">
          <h1 className="text-xl font-semibold">{plan.name}</h1>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={() => setRunDialogOpen(true)}>
            <PlayCircle className="mr-2 h-4 w-4" />
            Run Now
          </Button>
          <Link to={`/plans/custom/${plan.id}/edit`}>
            <Button variant="outline">
              <Pencil className="mr-2 h-4 w-4" />
              Edit
            </Button>
          </Link>
          <CreateScheduleDialog
            initialPlanId={plan.id}
            initialProductId={plan.product_id}
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
          <CardTitle>Plan Information</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="text-sm">
            <span className="font-medium">Created:</span>
            <span className="ml-2 text-muted-foreground">
              {new Date(plan.created_at).toLocaleString()}
            </span>
          </div>
          <div className="text-sm">
            <span className="font-medium">Total Tests:</span>
            <span className="ml-2 text-muted-foreground">{effectiveTests.length}</span>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Included Tests</CardTitle>
          <CardDescription>
            <FileCode className="inline h-4 w-4 mr-1" />
            {effectiveTests.length} test{effectiveTests.length !== 1 ? 's' : ''} in this plan
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Plan</TableHead>
                  <TableHead>Test File</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {effectiveTests.length > 0 ? (
                  effectiveTests.map((test, index) => (
                    <TableRow key={`${test.plan_id}-${test.test_file}-${index}`}>
                      <TableCell className="font-medium">
                        <Link 
                          to={`/plans/${test.plan_id}`}
                          className="text-indigo-600 hover:text-indigo-900 hover:underline"
                        >
                          {test.plan_id}
                        </Link>
                      </TableCell>
                      <TableCell className="font-mono text-sm">{test.test_file}</TableCell>
                    </TableRow>
                  ))
                ) : (
                  <TableRow>
                    <TableCell colSpan={2} className="text-center text-muted-foreground">
                      No tests in this plan
                    </TableCell>
                  </TableRow>
                )}
              </TableBody>
            </Table>
          </div>
        </CardContent>
      </Card>

      <RunCustomPlanDialog
        plan={plan}
        open={runDialogOpen}
        onOpenChange={setRunDialogOpen}
      />
    </div>
  );
}
