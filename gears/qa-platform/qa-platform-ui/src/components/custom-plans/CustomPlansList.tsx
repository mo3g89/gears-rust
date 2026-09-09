import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { toast } from 'sonner';
import { CustomPlan, TestPlanInfo } from '../../api/types';
import { useDeleteCustomPlan } from '../../api/hooks';
import { Button } from '../ui/button';
import { useConfirm } from '../ui/confirm-dialog';
import { CreateScheduleDialog } from '../schedules/CreateScheduleDialog';
import { RunCustomPlanDialog } from './RunCustomPlanDialog';
import { compileFql } from '@/lib/fql';
import { displayPlanName } from '@/lib/utils';
import { resolveCustomPlanTests } from '@/lib/customPlanTests';
import { FqlQueryInput } from '@/components/filters/FqlQueryInput';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '../ui/table';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu';
import { Play, MoreVertical, Pencil, Trash2, CalendarPlus } from 'lucide-react';

interface CustomPlansListProps {
  plans: CustomPlan[];
  onEdit: (plan: CustomPlan) => void;
  /** Standard plans on the currently-selected branch, used to resolve the
   *  effective test count of "whole plan" / "dependencies" custom plans
   *  (whose own `tests` array is empty — see `resolveCustomPlanTests`). */
  standardPlans?: TestPlanInfo[];
}

export function CustomPlansList({
  plans,
  onEdit,
  standardPlans = [],
}: CustomPlansListProps) {
  const deletePlan = useDeleteCustomPlan();
  const confirm = useConfirm();
  const [selectedPlan, setSelectedPlan] = useState<CustomPlan | null>(null);
  const [runDialogOpen, setRunDialogOpen] = useState(false);
  const [query, setQuery] = useState('');

  // "Whole plan" / "dependencies" custom plans don't populate their own
  // `tests` array (it's resolved at run time — see `resolveCustomPlanTests`),
  // so the displayed/filterable test set is resolved here per plan instead
  // of reading `plan.tests` directly.
  const effectiveTestsByPlanId = useMemo(() => {
    const map = new Map<string, ReturnType<typeof resolveCustomPlanTests>>();
    for (const plan of plans) {
      map.set(plan.id, resolveCustomPlanTests(plan, standardPlans, plans));
    }
    return map;
  }, [plans, standardPlans]);
  const effectiveTests = (plan: CustomPlan) => effectiveTestsByPlanId.get(plan.id) ?? plan.tests;

  const fqlFields = useMemo(
    () => ['id', 'name', 'tests', 'testCount', 'plan', 'planId', 'test', 'testFile', 'created', 'createdAt'],
    []
  );

  const fqlValues = useMemo(() => {
    const collect = (items: string[]) => Array.from(new Set(items.filter(Boolean))).sort();
    return {
      id: collect(plans.map((plan) => plan.id)),
      name: collect(plans.map((plan) => plan.name)),
      tests: collect(plans.map((plan) => String(effectiveTests(plan).length))),
      testcount: collect(plans.map((plan) => String(effectiveTests(plan).length))),
      plan: collect(plans.flatMap((plan) => effectiveTests(plan).map((test) => test.plan_id))),
      planid: collect(plans.flatMap((plan) => effectiveTests(plan).map((test) => test.plan_id))),
      test: collect(plans.flatMap((plan) => effectiveTests(plan).map((test) => test.test_file))),
      testfile: collect(plans.flatMap((plan) => effectiveTests(plan).map((test) => test.test_file))),
      created: collect(plans.map((plan) => plan.created_at)),
      createdat: collect(plans.map((plan) => plan.created_at)),
    } as Record<string, string[]>;
    // `effectiveTests` is a plain function of `effectiveTestsByPlanId` (already listed
    // below) recreated every render; adding it here would defeat this memo instead of
    // fixing anything.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plans, effectiveTestsByPlanId]);

  const compiled = useMemo(
    () =>
      compileFql<CustomPlan>(
        query,
        (plan, field) => {
          switch (field) {
            case 'id':
              return plan.id;
            case 'name':
              return plan.name;
            case 'tests':
            case 'testcount':
              return effectiveTests(plan).length;
            case 'plan':
            case 'planid':
              return effectiveTests(plan).map((test) => test.plan_id);
            case 'test':
            case 'testfile':
              return effectiveTests(plan).map((test) => test.test_file);
            case 'created':
            case 'createdat':
              return plan.created_at;
            default:
              return undefined;
          }
        },
        (plan) => [
          plan.id,
          plan.name,
          String(effectiveTests(plan).length),
          plan.created_at,
          ...effectiveTests(plan).flatMap((test) => [test.plan_id, test.test_file]),
        ].join(' ')
      ),
    // `effectiveTests` is a plain function of `effectiveTestsByPlanId` (already listed
    // below) recreated every render; adding it here would defeat this memo instead of
    // fixing anything.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [query, effectiveTestsByPlanId]
  );

  const filteredPlans = useMemo(() => {
    return plans.filter((plan) => compiled.matches(plan));
  }, [plans, compiled]);

  const handleRunClick = (plan: CustomPlan) => {
    setSelectedPlan(plan);
    setRunDialogOpen(true);
  };

  const handleDelete = async (plan: CustomPlan) => {
    const ok = await confirm({
      title: `Delete "${plan.name}"?`,
      description: 'This custom plan will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (!ok) {
      return;
    }

    deletePlan.mutate(plan.id, {
      onSuccess: () => {
        toast.success(`Custom plan "${plan.name}" deleted`);
      },
      onError: (error) => {
        toast.error('Failed to delete custom plan', {
          description: String(error),
        });
      },
    });
  };

  if (plans.length === 0) {
    return (
      <div className="text-center py-12 border rounded-md">
        <p className="text-muted-foreground">No plans yet</p>
        <p className="text-sm text-muted-foreground mt-2">
          Create your first test plan by clicking the button above
        </p>
      </div>
    );
  }

  return (
    <div className="rounded-md border">
      <div className="border-b p-3 space-y-2">
        <FqlQueryInput
          value={query}
          onChange={setQuery}
          placeholder='name ~ nightly AND tests >= 5 AND test ~ smoke'
          fields={fqlFields}
          valueSuggestions={fqlValues}
          savedFiltersKey="custom-plans"
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
            <TableHead>Name</TableHead>
            <TableHead className="text-right">Tests</TableHead>
            <TableHead>Created</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {filteredPlans.length === 0 ? (
            <TableRow>
              <TableCell colSpan={4} className="text-center text-muted-foreground">
                No plans match current filters
              </TableCell>
            </TableRow>
          ) : filteredPlans.map((plan) => (
            <TableRow key={plan.id}>
              <TableCell>
                <Link
                  to={`/plans/custom/${plan.id}`}
                  className="text-foreground/80 hover:text-foreground hover:underline"
                  title={plan.name}
                >
                  {displayPlanName(plan.name)}
                </Link>
              </TableCell>
              <TableCell className="text-right tabular-nums">{effectiveTests(plan).length}</TableCell>
              <TableCell className="text-muted-foreground">
                {new Date(plan.created_at).toLocaleDateString()}
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
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button size="icon" variant="ghost" className="h-7 w-7" aria-label="More actions">
                        <MoreVertical className="h-4 w-4" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={() => onEdit(plan)}>
                        <Pencil className="h-4 w-4 mr-2" />
                        Edit
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() => handleDelete(plan)}
                        className="text-destructive"
                      >
                        <Trash2 className="h-4 w-4 mr-2" />
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      
      {selectedPlan && (
        <RunCustomPlanDialog
          plan={selectedPlan}
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
