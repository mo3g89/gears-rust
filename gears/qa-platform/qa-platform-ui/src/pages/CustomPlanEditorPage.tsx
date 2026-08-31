import { useEffect, useMemo, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { toast } from 'sonner';
import {
  useTests,
  useCreateCustomPlan,
  useUpdateCustomPlan,
  useActiveProduct,
  usePlans,
  useCustomPlans,
  useCustomPlan,
} from '@/api/hooks';
import { useSelectedBranch } from '@/lib/selectedBranch';
import { resolveCustomPlanTests } from '@/lib/customPlanTests';
import { CustomPlanTest } from '@/api/types';
import { TestSelector } from '@/components/custom-plans/TestSelector';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Checkbox } from '@/components/ui/checkbox';
import { Loader2, ArrowLeft } from 'lucide-react';

export function CustomPlanEditorPage() {
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const isEdit = !!id;

  const [branch] = useSelectedBranch();
  const { data: existingPlan, isLoading: planLoading } = useCustomPlan(id ?? '');
  const { data: tests, isLoading: testsLoading } = useTests(branch);
  const { data: standardPlans } = usePlans(branch);
  const { data: customPlans } = useCustomPlans();
  const product = useActiveProduct();
  const createPlan = useCreateCustomPlan();
  const updatePlan = useUpdateCustomPlan();

  const [name, setName] = useState('');
  const [selectedTests, setSelectedTests] = useState<CustomPlanTest[]>([]);
  const [includedPlans, setIncludedPlans] = useState<string[]>([]);

  useEffect(() => {
    if (existingPlan) {
      setName(existingPlan.name);
      setSelectedTests(existingPlan.tests);
      setIncludedPlans(existingPlan.included_plans ?? []);
    }
  }, [existingPlan]);

  const includablePlans = useMemo(
    () => [
      ...(standardPlans ?? []).map((p) => ({ id: p.id, label: p.id, kind: 'standard' as const })),
      ...(customPlans ?? [])
        .filter((p) => p.id !== id)
        .map((p) => ({ id: p.id, label: p.name, kind: 'custom' as const })),
    ],
    [standardPlans, customPlans, id]
  );

  const toggleIncluded = (planId: string) => {
    setIncludedPlans((prev) =>
      prev.includes(planId) ? prev.filter((p) => p !== planId) : [...prev, planId]
    );
  };

  // Resolved test count for the "whole plans" tab badge — that tab selects
  // plans rather than individual tests, so the raw `includedPlans.length`
  // isn't the test count; resolve it the same way the list/detail pages do.
  const includedPlansTestCount = useMemo(
    () =>
      resolveCustomPlanTests(
        { id, tests: [], included_plans: includedPlans, nodes: [] },
        standardPlans ?? [],
        customPlans ?? []
      ).length,
    [id, includedPlans, standardPlans, customPlans]
  );
  const empty = selectedTests.length === 0 && includedPlans.length === 0;
  const saving = createPlan.isPending || updatePlan.isPending;

  const goBack = () => navigate('/plans?tab=custom');

  const handleSubmit = () => {
    if (!name.trim() || empty) return;
    const formData = {
      name: name.trim(),
      included_plans: includedPlans,
      tests: selectedTests,
      product_id: existingPlan?.product_id ?? product?.id,
    };
    if (isEdit && existingPlan) {
      updatePlan.mutate(
        { id: existingPlan.id, data: formData },
        { onSuccess: goBack, onError: (e) => toast.error(`Failed to update custom plan: ${e}`) }
      );
    } else {
      createPlan.mutate(formData, {
        onSuccess: goBack,
        onError: (e) => toast.error(`Failed to create custom plan: ${e}`),
      });
    }
  };

  if (isEdit && planLoading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-7xl space-y-6">
      <div className="space-y-1">
        <button
          type="button"
          onClick={goBack}
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="h-4 w-4" /> Back to plans
        </button>
        <h1 className="text-xl font-semibold">{isEdit ? 'Edit' : 'Add'} Test Plan</h1>
        <p className="text-muted-foreground">
          Compose a test plan from individual tests or whole plans.
        </p>
      </div>

      {/* Name only: a custom plan carries no description in this deployment, and a
          field whose content the backend silently drops is worse than no field
          (REMOVED-SURFACES.md, Task 8a C7). */}
      <div className="space-y-1.5">
        <Label htmlFor="name">Plan Name</Label>
        <Input
          id="name"
          placeholder="e.g. Nightly Critical Tests"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>

      <Tabs defaultValue="tests">
        <TabsList>
          <TabsTrigger value="tests">
            Individual tests
            {selectedTests.length > 0 && (
              <span className="ml-1.5 rounded-full bg-primary/15 px-1.5 text-xs text-primary">
                {selectedTests.length}
              </span>
            )}
          </TabsTrigger>
          <TabsTrigger value="plans">
            Whole plans
            {includedPlans.length > 0 && (
              <span className="ml-1.5 rounded-full bg-primary/15 px-1.5 text-xs text-primary">
                {includedPlansTestCount}
              </span>
            )}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="tests" className="mt-4">
          {testsLoading ? (
            <div className="flex h-32 items-center justify-center">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : tests ? (
            <TestSelector tests={tests} selectedTests={selectedTests} onSelectionChange={setSelectedTests} />
          ) : (
            <p className="text-sm text-muted-foreground">No tests available</p>
          )}
        </TabsContent>

        <TabsContent value="plans" className="mt-4 space-y-2">
          <p className="text-xs text-muted-foreground">
            Pull in every test of another plan. Included plans are expanded fresh on each run.
          </p>
          {includedPlans.length > 0 && (
            <p className="text-sm text-muted-foreground">
              {includedPlans.length} plan{includedPlans.length !== 1 ? 's' : ''} selected —{' '}
              {includedPlansTestCount} test{includedPlansTestCount !== 1 ? 's' : ''} total
            </p>
          )}
          {includablePlans.length === 0 ? (
            <p className="text-sm text-muted-foreground">No other plans available</p>
          ) : (
            <div className="min-h-[300px] max-h-[calc(100vh-22rem)] divide-y overflow-y-auto rounded-md border">
              {includablePlans.map((p) => (
                <label
                  key={`${p.kind}-${p.id}`}
                  className="flex cursor-pointer items-center gap-2 px-3 py-2 text-sm hover:bg-accent"
                >
                  <Checkbox
                    checked={includedPlans.includes(p.id)}
                    onCheckedChange={() => toggleIncluded(p.id)}
                  />
                  <span className="min-w-0 truncate">{p.label}</span>
                </label>
              ))}
            </div>
          )}
        </TabsContent>
      </Tabs>

      <div className="flex items-center justify-end gap-3 border-t pt-4">
        <Button variant="outline" onClick={goBack}>
          Cancel
        </Button>
        <Button onClick={handleSubmit} disabled={saving || !name.trim() || empty}>
          {saving ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              {isEdit ? 'Updating...' : 'Creating...'}
            </>
          ) : (
            <>{isEdit ? 'Update' : 'Create'} Plan</>
          )}
        </Button>
      </div>
    </div>
  );
}
