import { useState, useMemo, useEffect } from 'react';
import { toast } from 'sonner';
import {
  usePlans,
  useCreateSchedule,
  useUpdateSchedule,
  usePlatforms,
  useSchedules,
  useCustomPlans,
  useTestRepositories,
  useTestRepoBranchesForRepos,
  useTests,
} from '@/api/hooks';
import { PlatformInfo, ScheduleInfo } from '@/api/types';
import { useAttributedRepoIds } from '@/lib/customPlanTests';
import { defaultPlatformForProduct, defaultPlatformLabel } from '@/lib/defaultPlatform';
import { TagMultiSelect } from '@/components/filters/TagMultiSelect';
import {
  ExclusivitySelect,
  Exclusivity,
  exclusivityToParam,
  exclusivityFromValue,
} from '@/components/runs/ExclusivitySelect';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Combobox } from '@/components/ui/combobox';
import { formatPlatformLabel, pickDefaultBranch } from '@/lib/utils';
import { getSelectedBranch } from '@/lib/selectedBranch';
import { Plus } from 'lucide-react';

const CRON_PRESETS: Array<{ label: string; expr: string }> = [
  { label: 'Every 15 min', expr: '*/15 * * * *' },
  { label: 'Hourly', expr: '0 * * * *' },
  { label: 'Every 6h', expr: '0 */6 * * *' },
  { label: 'Daily', expr: '0 0 * * *' },
  { label: 'Weekdays', expr: '0 0 * * 1-5' },
  { label: 'Weekly', expr: '0 0 * * 1' },
];

// Generate a numeric schedule ID for a plan
function generateScheduleId(planId: string, existingSchedules: any[]): string {
  const planSchedules = existingSchedules.filter((s) => s.plan_id === planId);
  const existingIds = planSchedules
    .map((s) => {
      const match = s.schedule_id.match(/(\d+)$/);
      return match ? parseInt(match[1], 10) : null;
    })
    .filter((id): id is number => id !== null);

  let nextId = 1;
  if (existingIds.length > 0) {
    const maxId = Math.max(...existingIds);
    nextId = maxId + 1;
  }

  return nextId.toString();
}

interface CreateScheduleDialogProps {
  initialPlanId?: string;
  /** Product that owns `initialPlanId`, when known up front (e.g. from a
   *  custom plan's detail page). Scopes the plan/custom-plan lookups to that
   *  product instead of the globally-active one, so the dialog still finds
   *  and correctly treats `initialPlanId` when opened while a *different*
   *  product is active elsewhere in the app. */
  initialProductId?: string | null;
  initialBranch?: string;
  testFile?: string;
  /** When provided, the dialog edits this schedule (delete + recreate) instead
   *  of creating a new one. */
  editSchedule?: ScheduleInfo;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  trigger?: React.ReactNode;
}

export function CreateScheduleDialog({
  initialPlanId,
  initialProductId,
  initialBranch,
  testFile,
  editSchedule,
  open: controlledOpen,
  onOpenChange,
  trigger,
}: CreateScheduleDialogProps = {}) {
  const isEdit = !!editSchedule;
  const lockedPlanId = initialPlanId || editSchedule?.plan_id;
  const [internalOpen, setInternalOpen] = useState(false);
  const isControlled = controlledOpen !== undefined;
  const open = isControlled ? controlledOpen : internalOpen;

  const handleOpenChange = (newOpen: boolean) => {
    if (isControlled && onOpenChange) {
      onOpenChange(newOpen);
    } else {
      setInternalOpen(newOpen);
    }
    if (!newOpen) {
      if (!lockedPlanId) {
        setPlanId('');
      }
      setCronExpr('');
      setPlatform('');
      setBranch('');
      setIncludeTags([]);
      setExcludeTags([]);
      setExclusivity('auto');
    }
  };

  const [planId, setPlanId] = useState(lockedPlanId || '');
  const [cronExpr, setCronExpr] = useState(editSchedule?.schedule || '');
  const [platform, setPlatform] = useState(editSchedule?.platform || '');
  const [branch, setBranch] = useState(
    editSchedule?.branch?.trim() || initialBranch?.trim() || getSelectedBranch()
  );
  const parseTags = (raw: string | null | undefined) =>
    (raw || '').split(',').map((t) => t.trim()).filter((t) => t.length > 0);
  const [includeTags, setIncludeTags] = useState<string[]>(parseTags(editSchedule?.include_tags));
  const [excludeTags, setExcludeTags] = useState<string[]>(parseTags(editSchedule?.exclude_tags));
  const [exclusivity, setExclusivity] = useState<Exclusivity>(exclusivityFromValue(editSchedule?.exclusive));

  useEffect(() => {
    if (lockedPlanId) {
      setPlanId(lockedPlanId);
    }
  }, [lockedPlanId]);

  // Re-prefill when opening in edit mode (schedule may load/change).
  useEffect(() => {
    if (open && editSchedule) {
      setCronExpr(editSchedule.schedule || '');
      setPlatform(editSchedule.platform || '');
      setBranch(editSchedule.branch?.trim() || '');
      setIncludeTags(parseTags(editSchedule.include_tags));
      setExcludeTags(parseTags(editSchedule.exclude_tags));
      setExclusivity(exclusivityFromValue(editSchedule.exclusive));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, editSchedule?.name]);

  const isTestSchedule = !!(testFile || editSchedule?.test_file);

  const { data: plans } = usePlans(branch, initialProductId);
  // Repo-membership lookup only — deliberately not scoped to `branch` (which
  // repo a plan lives in doesn't change per branch), mirroring
  // `RunCustomPlanDialog`. Using the branch-scoped `plans` above for
  // attribution would under-count a multi-repo custom plan whenever one of
  // its contributing standard plans doesn't exist on the currently-typed
  // branch, wrongly clearing the branch-required gate. See `useAttributedRepoIds`.
  const { data: allProductPlans } = usePlans(undefined, initialProductId);
  const { data: customPlans } = useCustomPlans();
  const { data: platforms } = usePlatforms();
  const { data: schedules } = useSchedules();
  const createSchedule = useCreateSchedule();
  const updateSchedule = useUpdateSchedule();

  const selectedPlan = useMemo(
    () => (plans || []).find((p) => p.id === planId) || null,
    [plans, planId]
  );
  const selectedCustomPlan = useMemo(
    () => (customPlans || []).find((p) => p.id === planId) || null,
    [customPlans, planId]
  );
  const isCustomPlan = !!selectedCustomPlan;
  const requiresPlatform = isTestSchedule || isCustomPlan;

  const selectedPlanLabel = useMemo(() => {
    const name = selectedPlan?.plan.name || selectedCustomPlan?.name || planId;
    if (!name) return '';
    return planId && name !== planId ? `${name} (${planId})` : name;
  }, [selectedCustomPlan?.name, selectedPlan?.plan.name, planId]);

  const productId = selectedPlan?.product_id || selectedCustomPlan?.product_id || undefined;
  const { data: testRepos } = useTestRepositories(productId);

  // A standard plan is tied to exactly one repo (`repo_id`). A custom plan's
  // tests can span any of the product's repos — especially a "whole plan" /
  // "dependencies" plan pulling from a different repo than the first one
  // registered — so resolve its actual effective test set and take the
  // union of every contributing repo (see `RunCustomPlanDialog`, which has
  // the same requirement, and `useAttributedRepoIds`). Uses the
  // branch-unscoped `allProductPlans` (not the branch-scoped `plans` above)
  // for repo-membership so a contributing plan missing from this branch
  // doesn't drop out of the attribution. Falls back to every product repo
  // when nothing more specific can be attributed yet.
  const attributedRepoIds = useAttributedRepoIds(
    selectedPlan,
    selectedCustomPlan,
    allProductPlans,
    customPlans
  );

  const repoIds = useMemo(() => {
    if (attributedRepoIds.length > 0) return attributedRepoIds;
    const repos = Array.isArray(testRepos) ? testRepos : [];
    return repos.map((r) => r.id);
  }, [attributedRepoIds, testRepos]);

  // Only a genuinely multi-repo (attributed) custom plan must pin a branch.
  const branchRequired = !!selectedCustomPlan && attributedRepoIds.length > 1;

  const repoDefaultBranch = useMemo(() => {
    const repos = Array.isArray(testRepos) ? testRepos : [];
    const relevant = repos.filter((r) => repoIds.includes(r.id));
    return relevant.length === 1 ? relevant[0].default_branch ?? null : null;
  }, [repoIds, testRepos]);

  const branchList = useTestRepoBranchesForRepos(repoIds);

  // Suggest tags actually present on the selected plan's tests (for the chosen branch).
  const { data: testCatalog } = useTests(branch);
  const tagSuggestions = useMemo(() => {
    const set = new Set<string>();
    for (const t of testCatalog || []) {
      if (t.plan_id !== planId) continue;
      for (const tag of t.tags || []) {
        const trimmed = tag.trim();
        if (trimmed) set.add(trimmed);
      }
    }
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [testCatalog, planId]);

  const platformList = useMemo<PlatformInfo[]>(
    () => (Array.isArray(platforms) ? platforms.filter((p): p is PlatformInfo => !!p?.name) : []),
    [platforms]
  );

  const selectedPlatformInfo = useMemo(
    () => platformList.find((p) => p.name === platform) ?? null,
    [platformList, platform]
  );

  // Same "Default cluster" resolution the run dialog uses — see
  // `defaultPlatformForProduct`. A schedule that fires against an unintended environment
  // is worse than a manual run doing so: nobody is watching when it happens.
  const defaultPlatform = useMemo(
    () => defaultPlatformForProduct(platformList, productId ?? null),
    [platformList, productId]
  );

  useEffect(() => {
    if (!open) return;
    setBranch((prev) => {
      if (prev) return prev;
      if (initialBranch?.trim()) return initialBranch.trim();
      if (getSelectedBranch()) return getSelectedBranch();
      const platformDefault = selectedPlatformInfo?.default_branch?.trim();
      if (platformDefault) return platformDefault;
      if (repoDefaultBranch?.trim()) return repoDefaultBranch.trim();
      return pickDefaultBranch(branchList);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, repoDefaultBranch, branchList, selectedPlatformInfo?.default_branch, initialBranch]);

  const effectiveTestFile = testFile ?? editSchedule?.test_file ?? undefined;

  // Generate schedule_id when plan is selected (edit keeps the existing id).
  const scheduleId = useMemo(() => {
    if (editSchedule) return editSchedule.schedule_id;
    if (!planId || !schedules) return '';
    return generateScheduleId(planId, schedules);
  }, [editSchedule, planId, schedules]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();

    if (!planId || !cronExpr) return;

    const effectivePlatform = platform || (requiresPlatform ? '' : defaultPlatform?.name) || '';
    if (!effectivePlatform) {
      toast.error('Please select a platform', {
        description: requiresPlatform
          ? undefined
          : 'This product has no single default platform, so "Default cluster" cannot be resolved.',
      });
      return;
    }
    // Mixed custom plans (2+ repos) must pin a branch so every checkout and
    // result attribution share one Version target at trigger time.
    if (branchRequired && !branch.trim()) {
      toast.error('Please select a branch', {
        description:
          'This custom plan mixes tests from multiple repositories. Pick a branch that exists in each of them.',
      });
      return;
    }

    const finalScheduleId =
      scheduleId || editSchedule?.schedule_id || generateScheduleId(planId, schedules || []);

    const payload = {
      plan_id: planId,
      schedule_id: finalScheduleId,
      cron_expr: cronExpr,
      platform: effectivePlatform,
      branch: branch.trim() || undefined,
      test_file: effectiveTestFile || undefined,
      include_tags: includeTags.length ? includeTags.join(',') : undefined,
      exclude_tags: excludeTags.length ? excludeTags.join(',') : undefined,
      exclusive: exclusivityToParam(exclusivity) ?? null,
      // On create, Slack starts off (configured later via the menu). On edit,
      // preserve the schedule's existing Slack settings across the recreate.
      slack_notifications_enabled: editSchedule?.slack_notifications_enabled ?? false,
      slack_channel: editSchedule?.slack_channel ?? undefined,
      slack_notification_events: editSchedule?.slack_notification_events ?? undefined,
    };

    const onSuccess = () => {
      handleOpenChange(false);
      if (!lockedPlanId) {
        setPlanId('');
      }
      setCronExpr('');
      setPlatform('');
      setBranch('');
      setIncludeTags([]);
      setExcludeTags([]);
      setExclusivity('auto');
    };

    if (editSchedule) {
      updateSchedule.mutate({ name: editSchedule.name, data: payload }, { onSuccess });
    } else {
      createSchedule.mutate(payload, { onSuccess });
    }
  };

  const dialogContent = (
    <DialogContent>
      <form onSubmit={handleSubmit}>
        <DialogHeader>
          <DialogTitle>{isEdit ? 'Edit Schedule' : 'Create New Schedule'}</DialogTitle>
          <DialogDescription>
            {isTestSchedule
              ? `Cron schedule for the test file: ${effectiveTestFile}`
              : isCustomPlan
                ? 'Cron schedule to automatically run a custom plan'
                : 'Cron schedule to automatically run a test plan'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {!isTestSchedule && (
            <div className="space-y-2">
              <Label htmlFor="plan">
                {lockedPlanId ? 'Plan' : isCustomPlan ? 'Custom Plan' : 'Test Plan'}
              </Label>
              {lockedPlanId ? (
                <Input
                  id="plan"
                  value={selectedPlanLabel}
                  readOnly
                  className="bg-muted"
                />
              ) : (
                <Combobox
                  id="plan"
                  value={planId}
                  onChange={(next) => {
                    setPlanId(next);
                    setBranch('');
                    setPlatform('');
                  }}
                  options={[
                    ...(plans || []).map((plan) => ({
                      value: plan.id,
                      label: `Test Plan · ${plan.plan.name}`,
                    })),
                    ...(customPlans || []).map((plan) => ({
                      value: plan.id,
                      label: `Custom · ${plan.name}`,
                    })),
                  ]}
                  placeholder="Select a plan"
                  emptyText="No plans"
                />
              )}
            </div>
          )}

          {isTestSchedule && (
            <div className="space-y-2">
              <Label htmlFor="test-file">Test File</Label>
              <Input id="test-file" value={effectiveTestFile || ''} readOnly className="bg-muted" />
              <p className="text-xs text-muted-foreground">
                Scheduling single test file from plan: {planId || 'N/A'}
              </p>
            </div>
          )}

          <div className="space-y-2">
            <Label htmlFor="cron">Cron Expression</Label>
            <Input
              id="cron"
              value={cronExpr}
              onChange={(e) => setCronExpr(e.target.value)}
              placeholder="0 0 * * *"
              required
            />
            <div className="flex flex-wrap gap-1.5">
              {CRON_PRESETS.map((preset) => {
                const active = cronExpr.trim() === preset.expr;
                return (
                  <button
                    key={preset.expr}
                    type="button"
                    onClick={() => setCronExpr(preset.expr)}
                    className={
                      active
                        ? 'rounded-full border border-primary bg-primary/10 px-2.5 py-0.5 text-xs text-primary'
                        : 'rounded-full border px-2.5 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                    }
                    title={preset.expr}
                  >
                    {preset.label}
                  </button>
                );
              })}
            </div>
            <p className="text-xs text-muted-foreground">
              Pick a preset or type a cron expression — fields are min hour day month weekday.
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="platform">{requiresPlatform ? 'Platform' : 'Platform (Optional)'}</Label>
            {platformList.length > 0 ? (
              <Combobox
                id="platform"
                value={platform}
                onChange={(next) => {
                  setPlatform(next);
                  const nextDefault = platformList
                    .find((p) => p.name === next)
                    ?.default_branch?.trim();
                  if (nextDefault) {
                    setBranch(nextDefault);
                  }
                }}
                options={[
                  ...(requiresPlatform
                    ? []
                    : [{ value: '', label: defaultPlatformLabel(defaultPlatform) }]),
                  ...platformList.map((item) => ({ value: item.name, label: formatPlatformLabel(item) })),
                ]}
                placeholder={
                  requiresPlatform ? 'Select platform' : defaultPlatformLabel(defaultPlatform)
                }
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                {requiresPlatform
                  ? 'No platforms configured. Please add a platform first.'
                  : 'No platforms configured. Add one before scheduling.'}
              </p>
            )}
          </div>

          <div className="space-y-2">
            <Label htmlFor="branch">Branch</Label>
            <Combobox
              id="branch"
              value={branch}
              onChange={setBranch}
              options={branchList}
              allowCustom
              placeholder={repoDefaultBranch ? `${repoDefaultBranch} (default)` : 'main'}
              emptyText="No branches cached"
            />
            {repoDefaultBranch && (
              <p className="text-xs text-muted-foreground">
                Repository default: {repoDefaultBranch}. Schedule fails fast if the branch is gone at trigger time.
              </p>
            )}
          </div>

          <ExclusivitySelect
            value={exclusivity}
            onChange={setExclusivity}
            disabled={createSchedule.isPending || updateSchedule.isPending}
          />

          {!isTestSchedule && !isCustomPlan && (
            <div className="space-y-3 rounded-lg border p-3">
              <p className="text-sm font-medium">Tag filter (optional)</p>
              <div className="space-y-1.5">
                <Label className="text-xs text-muted-foreground">Include — run only tests with any of these tags</Label>
                <TagMultiSelect
                  id="include-tags"
                  value={includeTags}
                  onChange={setIncludeTags}
                  suggestions={tagSuggestions}
                  placeholder="e.g. smoke (leave empty = all)"
                />
              </div>
              <div className="space-y-1.5">
                <Label className="text-xs text-muted-foreground">Exclude — skip tests with any of these tags</Label>
                <TagMultiSelect
                  id="exclude-tags"
                  value={excludeTags}
                  onChange={setExcludeTags}
                  suggestions={tagSuggestions}
                  placeholder="e.g. destructive"
                />
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button type="button" variant="outline" onClick={() => handleOpenChange(false)}>
            Cancel
          </Button>
          <Button
            type="submit"
            disabled={
              !planId ||
              !cronExpr ||
              (requiresPlatform && !platform) ||
              (branchRequired && !branch.trim()) ||
              createSchedule.isPending ||
              updateSchedule.isPending
            }
          >
            {isEdit
              ? updateSchedule.isPending
                ? 'Saving...'
                : 'Save Changes'
              : createSchedule.isPending
                ? 'Creating...'
                : 'Create Schedule'}
          </Button>
        </DialogFooter>
      </form>
    </DialogContent>
  );

  if (trigger) {
    return (
      <>
        <div onClick={() => handleOpenChange(true)}>{trigger}</div>
        <Dialog open={open} onOpenChange={handleOpenChange}>
          {dialogContent}
        </Dialog>
      </>
    );
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Create Schedule
        </Button>
      </DialogTrigger>
      {dialogContent}
    </Dialog>
  );
}
