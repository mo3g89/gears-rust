import { useMemo } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { usePlans, useCustomPlans, usePlansPrefetch, useActiveProduct, useTestRepositories } from '@/api/hooks';
import { CustomPlan } from '@/api/types';
import { keepPlansForProduct } from '@/lib/productScope';
import { PlanList } from '@/components/plans/PlanList';
import { CustomPlansList } from '@/components/custom-plans/CustomPlansList';
import { BranchPicker } from '@/components/filters/BranchPicker';
import { useSelectedBranch } from '@/lib/selectedBranch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Button } from '@/components/ui/button';
import { Loader2, Plus, FileText } from 'lucide-react';

export function PlansPage() {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = searchParams.get('tab') === 'custom' ? 'custom' : 'standard';

  const [branch, setBranch] = useSelectedBranch();

  const { data: plans, isLoading: plansLoading, isPlaceholderData: plansStale, error: plansError } = usePlans(branch);
  const { data: customPlans, isLoading: customPlansLoading, error: customPlansError } = useCustomPlans();
  const { data: repos } = useTestRepositories();
  const prefetchPlans = usePlansPrefetch();
  const activeProduct = useActiveProduct();

  // **The Custom Plans tab is scoped exactly as the Standard Plans tab beside
  // it is** (spec §4: "while a product is selected, no list surface shows a row
  // belonging to a different product"). It was not, and a page whose two tabs
  // disagree about what the switcher means is the divergence this work exists
  // to end — §1's inventory was built from `hooks.ts` and missed this one,
  // because the scoping happens here rather than in a hook.
  //
  // Same resolver as the Runs and Schedules lists: a custom plan carries no
  // product key, so it is attributed through the repositories its tests name,
  // and D4's hide applies — a plan with no resolvable tests, or with tests
  // spanning two products, is listed under no product. `undefined` while the
  // repositories are still loading, so the tab shows its spinner rather than a
  // list that is briefly scoped against an empty repository map.
  const scopedCustomPlans = useMemo(
    () =>
      customPlans && repos ? keepPlansForProduct(customPlans, activeProduct?.id ?? null, repos) : undefined,
    [customPlans, repos, activeProduct?.id]
  );

  const handleCreate = () => navigate('/plans/custom/new');

  const handleEdit = (plan: CustomPlan) => navigate(`/plans/custom/${plan.id}/edit`);

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold flex items-center gap-2">
            <FileText className="h-5 w-5" />
            Test Plans
          </h1>
          <p className="text-muted-foreground">Manage and execute standard and custom test plans</p>
        </div>
        <Button onClick={handleCreate}>
          <Plus className="h-4 w-4 mr-2" />
          Add Test Plan
        </Button>
      </div>

      <Tabs value={tab} onValueChange={(value) => setSearchParams({ tab: value })}>
        <TabsList>
          <TabsTrigger value="standard">Standard Plans</TabsTrigger>
          <TabsTrigger value="custom">Custom Plans</TabsTrigger>
        </TabsList>

        <TabsContent value="standard" className="mt-4 space-y-3">
          <div className="flex items-center justify-between gap-2">
            <BranchPicker
              value={branch}
              onChange={setBranch}
              onPrefetch={prefetchPlans}
              productId={activeProduct?.id}
            />
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              {plansStale && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
              {plansStale
                ? <>Loading {branch.trim() ? <code>{branch}</code> : 'default branch'}…</>
                : <>Listing plans from {branch.trim() ? <code>{branch}</code> : 'the default branch'}</>}
            </span>
          </div>
          {plansLoading ? (
            <div className="flex items-center justify-center h-64">
              <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          ) : plansError ? (
            <div className="text-center py-8">
              <p className="text-destructive">Failed to load test plans</p>
              <p className="text-sm text-muted-foreground mt-2">{plansError.message}</p>
            </div>
          ) : (
            <div className={plansStale ? 'pointer-events-none opacity-50 transition-opacity' : 'transition-opacity'}>
              <PlanList plans={plans || []} branch={branch} />
            </div>
          )}
        </TabsContent>

        <TabsContent value="custom" className="mt-4">
          {customPlansLoading || plansLoading || !scopedCustomPlans ? (
            <div className="flex items-center justify-center h-64">
              <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          ) : customPlansError ? (
            <div className="text-center py-8">
              <p className="text-destructive">Failed to load custom plans</p>
              <p className="text-sm text-muted-foreground mt-2">{String(customPlansError)}</p>
            </div>
          ) : (
            <div className="space-y-3">
              <p className="text-xs text-muted-foreground">
                Scoped to the selected product. A custom plan carries no product of its
                own, so it is attributed through the repositories its tests belong to — a
                plan whose tests span two products, or whose repositories have all been
                deleted, is listed under neither.
              </p>
              <CustomPlansList
                plans={scopedCustomPlans}
                onEdit={handleEdit}
                standardPlans={plans || []}
              />
            </div>
          )}
        </TabsContent>
      </Tabs>
    </div>
  );
}
