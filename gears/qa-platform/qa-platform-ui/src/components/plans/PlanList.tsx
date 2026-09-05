import { Link } from 'react-router-dom';
import { useEffect, useMemo, useState } from 'react';
import { TestPlanInfo } from '@/api/types';
import { Button } from '../ui/button';
import { Badge } from '../ui/badge';
import { CreateScheduleDialog } from '@/components/schedules/CreateScheduleDialog';
import { RunPlanDialog } from './RunPlanDialog';
import { RecentRunDots } from './RecentRunDots';
import { compileFql } from '@/lib/fql';
import { displayPlanName } from '@/lib/utils';
import { FqlQueryInput } from '@/components/filters/FqlQueryInput';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '../ui/table';
import { Play, CalendarPlus } from 'lucide-react';

interface PlanListProps {
  plans: TestPlanInfo[];
  /** Branch the listing was loaded from; forwarded to Run/Schedule dialogs
   *  so they default to the same branch the user is browsing. */
  branch?: string;
}

function summarizeList(items: string[], visible = 2): { shown: string; more: number } {
  if (items.length === 0) return { shown: '', more: 0 };
  const head = items.slice(0, visible);
  return { shown: head.join(', '), more: Math.max(0, items.length - visible) };
}

export function PlanList({ plans, branch }: PlanListProps) {
  const [selectedPlan, setSelectedPlan] = useState<TestPlanInfo | null>(null);
  const [runDialogOpen, setRunDialogOpen] = useState(false);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(10);
  const [query, setQuery] = useState('');

  const fqlFields = useMemo(
    () => ['id', 'plan', 'planId', 'name', 'planName', 'description', 'desc', 'product', 'productKey', 'productName', 'tags', 'tag', 'tests', 'testCount'],
    []
  );

  const fqlValues = useMemo(() => {
    const collect = (items: string[]) => Array.from(new Set(items.filter(Boolean))).sort();
    return {
      id: collect(plans.map((plan) => plan.id)),
      plan: collect(plans.map((plan) => plan.id)),
      planid: collect(plans.map((plan) => plan.id)),
      name: collect(plans.map((plan) => plan.plan.name)),
      planname: collect(plans.map((plan) => plan.plan.name)),
      description: collect(plans.map((plan) => plan.plan.description)),
      desc: collect(plans.map((plan) => plan.plan.description)),
      product: collect(plans.map((plan) => plan.product_key || '')),
      productkey: collect(plans.map((plan) => plan.product_key || '')),
      productname: collect(plans.map((plan) => plan.product_name || '')),
      tags: collect(plans.flatMap((plan) => plan.plan.tags)),
      tag: collect(plans.flatMap((plan) => plan.plan.tags)),
      tests: collect(plans.map((plan) => String(plan.test_files.length))),
      testcount: collect(plans.map((plan) => String(plan.test_files.length))),
      source: collect(plans.map((plan) => plan.source)),
      repo: collect(plans.map((plan) => plan.repo_name || '')),
      reponame: collect(plans.map((plan) => plan.repo_name || '')),
    } as Record<string, string[]>;
  }, [plans]);

  const compiled = useMemo(
    () =>
      compileFql<TestPlanInfo>(
        query,
        (plan, field) => {
          switch (field) {
            case 'id':
            case 'plan':
            case 'planid':
              return plan.id;
            case 'name':
            case 'planname':
              return plan.plan.name;
            case 'description':
            case 'desc':
              return plan.plan.description;
            case 'product':
            case 'productkey':
              return plan.product_key || '';
            case 'productname':
              return plan.product_name || '';
            case 'tag':
            case 'tags':
              return plan.plan.tags;
            case 'tests':
            case 'testcount':
              return plan.test_files.length;
            case 'source':
              return plan.source;
            case 'repo':
            case 'reponame':
              return plan.repo_name || '';
            default:
              return undefined;
          }
        },
        (plan) => [
          plan.id,
          plan.plan.name,
          plan.plan.description,
          plan.product_key || '',
          plan.product_name || '',
          ...plan.plan.tags,
          String(plan.test_files.length),
          plan.source,
          plan.repo_name || '',
        ].join(' ')
      ),
    [query]
  );

  const filteredPlans = useMemo(
    () =>
      plans
        .map((plan, index) => ({ plan, index }))
        .filter(({ plan }) => compiled.matches(plan))
        .sort(
          (a, b) =>
            Number(b.plan.plan.validation) - Number(a.plan.plan.validation) || a.index - b.index
        )
        .map(({ plan }) => plan),
    [plans, compiled]
  );

  const totalPages = Math.max(1, Math.ceil(filteredPlans.length / pageSize));
  const currentPage = Math.min(page, totalPages);

  useEffect(() => {
    if (page > totalPages) {
      setPage(totalPages);
    }
  }, [page, totalPages]);

  useEffect(() => {
    setPage(1);
  }, [query, pageSize]);

  const pagedPlans = useMemo(() => {
    const start = (currentPage - 1) * pageSize;
    return filteredPlans.slice(start, start + pageSize);
  }, [filteredPlans, currentPage, pageSize]);

  if (plans.length === 0) {
    return (
      <div className="rounded-md border border-dashed py-12 text-center">
        <p className="text-sm font-medium">No test plans available</p>
        <p className="text-xs text-muted-foreground mt-1">
          Add test repositories in Product Catalog to load plans and tests.
        </p>
      </div>
    );
  }

  const handleRunClick = (plan: TestPlanInfo) => {
    setSelectedPlan(plan);
    setRunDialogOpen(true);
  };

  return (
    <div className="rounded-md border">
      <div className="border-b p-3 space-y-2">
        <FqlQueryInput
          value={query}
          onChange={setQuery}
          placeholder='product = KEY AND tags ~ smoke AND tests >= 10'
          fields={fqlFields}
          valueSuggestions={fqlValues}
          savedFiltersKey="plans"
        />
        {compiled.error && (
          <div className="text-xs text-amber-600">
            Invalid FQL ({compiled.error}). Using plain text search fallback.
          </div>
        )}
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Plan Name</TableHead>
            <TableHead className="text-right">Tests</TableHead>
            <TableHead>Tags</TableHead>
            <TableHead>Recent runs</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {pagedPlans.length === 0 ? (
            <TableRow>
              <TableCell colSpan={5} className="text-center text-muted-foreground">
                No test plans match current filter
              </TableCell>
            </TableRow>
          ) : (
            pagedPlans.map((plan) => {
              const tagSummary = summarizeList(plan.plan.tags, 2);

              return (
                <TableRow
                  key={plan.id}
                  className={plan.plan.validation ? 'bg-amber-50/40 dark:bg-amber-950/10' : undefined}
                >
                  <TableCell>
                    <div className="flex items-center gap-2">
                      <Link
                        to={`/plans/${plan.id}`}
                        className="text-foreground/80 hover:text-foreground hover:underline"
                        title={plan.plan.description ? `${plan.plan.name} — ${plan.plan.description}` : plan.plan.name}
                      >
                        {displayPlanName(plan.plan.name)}
                      </Link>
                      {plan.plan.validation && (
                        <Badge
                          variant="outline"
                          className="px-1 py-0 text-[9px] font-normal uppercase tracking-wide text-muted-foreground"
                        >
                          Validation
                        </Badge>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="text-right tabular-nums">{plan.test_files.length}</TableCell>
                  <TableCell className="max-w-[220px] truncate text-muted-foreground" title={plan.plan.tags.join(', ')}>
                    {plan.plan.tags.length === 0 ? (
                      <span>—</span>
                    ) : (
                      <span>
                        {tagSummary.shown}
                        {tagSummary.more > 0 && (
                          <span className="ml-1">+{tagSummary.more}</span>
                        )}
                      </span>
                    )}
                  </TableCell>
                  <TableCell>
                    <RecentRunDots runs={plan.recent_runs || []} visible={5} />
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1">
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-7 w-7"
                        onClick={() => handleRunClick(plan)}
                        title="Run plan"
                        aria-label="Run plan"
                      >
                        <Play className="h-4 w-4" />
                      </Button>
                      <CreateScheduleDialog
                        initialPlanId={plan.id}
                        initialBranch={branch}
                        trigger={
                          <Button
                            size="icon"
                            variant="ghost"
                            className="h-7 w-7"
                            title="Schedule plan"
                            aria-label="Schedule plan"
                          >
                            <CalendarPlus className="h-4 w-4" />
                          </Button>
                        }
                      />
                    </div>
                  </TableCell>
                </TableRow>
              );
            })
          )}
        </TableBody>
      </Table>

      <div className="flex flex-col gap-3 border-t px-4 py-3 text-sm sm:flex-row sm:items-center sm:justify-between">
        <div className="text-muted-foreground">
          Showing {Math.min((currentPage - 1) * pageSize + 1, filteredPlans.length)}-
          {Math.min(currentPage * pageSize, filteredPlans.length)} of {filteredPlans.length}
        </div>
        <div className="flex items-center gap-2">
          <label className="text-muted-foreground" htmlFor="plans-page-size">Rows:</label>
          <select
            id="plans-page-size"
            className="h-8 rounded border bg-background px-2"
            value={pageSize}
            onChange={(e) => {
              setPageSize(Number(e.target.value));
              setPage(1);
            }}
          >
            <option value={10}>10</option>
            <option value={20}>20</option>
            <option value={50}>50</option>
          </select>
          <Button
            size="sm"
            variant="outline"
            disabled={currentPage <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            Prev
          </Button>
          <span className="px-2">
            {currentPage} / {totalPages}
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={currentPage >= totalPages}
            onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
          >
            Next
          </Button>
        </div>
      </div>

      {selectedPlan && (
        <RunPlanDialog
          plan={selectedPlan}
          initialBranch={branch}
          open={runDialogOpen}
          onOpenChange={(open) => {
            setRunDialogOpen(open);
            if (!open) {
              setSelectedPlan(null);
            }
          }}
        />
      )}
    </div>
  );
}
