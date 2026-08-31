import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';
import { CustomPlan, PlatformInfo, RunParameter } from '@/api/types';
import { formatPlatformLabel, pickDefaultBranch } from '@/lib/utils';
import { getSelectedBranch } from '@/lib/selectedBranch';
import { RunParametersEditor, cleanRunParameters } from '@/components/runs/RunParametersEditor';
import { useAttributedRepoIds } from '@/lib/customPlanTests';
import { ExclusivitySelect, Exclusivity, exclusivityToParam } from '@/components/runs/ExclusivitySelect';
import {
  useRunCustomPlan,
  usePlatforms,
  usePlans,
  useCustomPlans,
  useTestRepositories,
  useTestRepoBranchesForRepos,
  isQueued,
} from '@/api/hooks';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { Combobox } from '@/components/ui/combobox';

interface RunCustomPlanDialogProps {
  plan: CustomPlan;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger?: React.ReactNode;
}

export function RunCustomPlanDialog({ plan, open, onOpenChange, trigger }: RunCustomPlanDialogProps) {
  const [selectedPlatform, setSelectedPlatform] = useState<string>('');
  const [selectedBranch, setSelectedBranch] = useState<string>(getSelectedBranch());
  const [parameters, setParameters] = useState<RunParameter[]>([]);
  const [exclusivity, setExclusivity] = useState<Exclusivity>('auto');
  const { data: platforms } = usePlatforms();
  const { data: testRepos } = useTestRepositories(plan.product_id ?? undefined);
  // Repo-membership lookup only — deliberately not scoped to `selectedBranch`
  // (which repo a plan lives in doesn't change per branch) so this doesn't
  // have to wait on the branch the user is about to pick. Scoped to the
  // plan's own product (not the globally-active one) so this stays correct
  // even if a different product is active elsewhere in the app.
  const { data: standardPlans } = usePlans(undefined, plan.product_id);
  const { data: customPlans } = useCustomPlans();
  const runPlan = useRunCustomPlan();

  const platformList = useMemo<PlatformInfo[]>(
    () => (Array.isArray(platforms) ? platforms.filter((p): p is PlatformInfo => !!p?.name) : []),
    [platforms]
  );

  const selectedPlatformInfo = useMemo(
    () => platformList.find((p) => p.name === selectedPlatform) ?? null,
    [platformList, selectedPlatform]
  );

  // A custom plan's tests can come from any of the product's git
  // repositories — a "whole plan" / "dependencies" plan in particular may
  // pull tests from a different repo than the first one registered for the
  // product. See `useAttributedRepoIds` for the resolution details (falls
  // back to every product repo below if nothing can be attributed yet, e.g.
  // while `standardPlans` is still loading).
  const attributedRepoIds = useAttributedRepoIds(null, plan, standardPlans, customPlans);

  const repoIds = useMemo(() => {
    if (attributedRepoIds.length > 0) return attributedRepoIds;
    // No repo could be attributed to any test yet (e.g. plans still
    // loading, or an empty/unsaved plan) — fall back to every repo
    // registered for the product so the picker still offers branches.
    const repos = Array.isArray(testRepos) ? testRepos : [];
    return repos.map((r) => r.id);
  }, [attributedRepoIds, testRepos]);

  // Only a genuinely multi-repo (attributed) plan must pin an explicit branch.
  const branchRequired = attributedRepoIds.length > 1;

  const relevantRepos = useMemo(() => {
    const repos = Array.isArray(testRepos) ? testRepos : [];
    return repos.filter((r) => repoIds.includes(r.id));
  }, [testRepos, repoIds]);

  const repoDefaultBranch = useMemo(
    () => (relevantRepos.length === 1 ? relevantRepos[0].default_branch ?? null : null),
    [relevantRepos]
  );

  const branchList = useTestRepoBranchesForRepos(repoIds);

  useEffect(() => {
    if (!open) return;
    setSelectedBranch((prev) => {
      if (prev) return prev;
      if (getSelectedBranch()) return getSelectedBranch();
      const platformDefault = selectedPlatformInfo?.default_branch?.trim();
      if (platformDefault) return platformDefault;
      if (repoDefaultBranch?.trim()) return repoDefaultBranch.trim();
      return pickDefaultBranch(branchList);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, repoDefaultBranch, branchList, selectedPlatformInfo?.default_branch]);

  const resetDialogState = () => {
    setSelectedPlatform('');
    setSelectedBranch('');
    setParameters([]);
    setExclusivity('auto');
  };

  const handleDialogOpenChange = (nextOpen: boolean) => {
    if (!nextOpen) {
      resetDialogState();
    }
    onOpenChange(nextOpen);
  };

  const handleRun = () => {
    if (!selectedPlatform) {
      toast.error('Please select a platform');
      return;
    }
    // Multi-repo (mixed) custom plans must pin an explicit branch so every
    // contributing checkout resolves the same ref and results register to it.
    if (branchRequired && !selectedBranch.trim()) {
      toast.error('Please select a branch', {
        description:
          'This plan mixes tests from multiple repositories. Pick a branch that exists in each of them.',
      });
      return;
    }

    runPlan.mutate(
      {
        id: plan.id,
        platform: selectedPlatform,
        branch: selectedBranch || undefined,
        parameters: cleanRunParameters(parameters),
        exclusive: exclusivityToParam(exclusivity),
      },
      {
        onSuccess: (data) => {
          handleDialogOpenChange(false);
          if (isQueued(data)) {
            toast.success('Test plan queued', {
              description: 'The platform is busy with an exclusive run. It will start automatically.',
              duration: 8000,
            });
            return;
          }
          const workflowName = data.workflow_name;
          toast.success('Test plan started', {
            duration: 8000,
            action: {
              label: 'Open Run',
              onClick: () => { window.location.href = `/runs/${workflowName}`; },
            },
          });
        },
        onError: (error) => {
          toast.error('Failed to run test plan', {
            description: String(error),
          });
        },
      }
    );
  };

  const dialogContent = (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Run Test Plan</DialogTitle>
        <DialogDescription>
          Pick a platform and branch to run "{plan.name}"
        </DialogDescription>
      </DialogHeader>
      <div className="space-y-4 py-4">
        <div className="space-y-2">
          <Label htmlFor="platform">Platform</Label>
          {platformList.length > 0 ? (
            <Combobox
              id="platform"
              value={selectedPlatform}
              onChange={(next) => {
                setSelectedPlatform(next);
                const nextDefault = platformList
                  .find((p) => p.name === next)
                  ?.default_branch?.trim();
                if (nextDefault) {
                  setSelectedBranch(nextDefault);
                }
              }}
              options={platformList.map((p) => ({ value: p.name, label: formatPlatformLabel(p) }))}
              placeholder="Select platform"
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              No platforms configured. Please add a platform first.
            </p>
          )}
        </div>

        <div className="space-y-2">
          <Label htmlFor="branch">Branch</Label>
          <Combobox
            id="branch"
            value={selectedBranch}
            onChange={setSelectedBranch}
            options={branchList}
            allowCustom
            placeholder={repoDefaultBranch ? `${repoDefaultBranch} (default)` : 'main'}
            emptyText="No branches cached"
          />
        </div>

        <ExclusivitySelect
          value={exclusivity}
          onChange={setExclusivity}
          disabled={runPlan.isPending}
        />

        <RunParametersEditor
          value={parameters}
          onChange={setParameters}
          disabled={runPlan.isPending}
        />
      </div>

      <DialogFooter>
        <Button variant="outline" onClick={() => handleDialogOpenChange(false)}>
          Cancel
        </Button>
        <Button
          onClick={handleRun}
          disabled={
            !selectedPlatform ||
            (branchRequired && !selectedBranch.trim()) ||
            runPlan.isPending
          }
        >
          {runPlan.isPending ? 'Starting...' : 'Run Plan'}
        </Button>
      </DialogFooter>
    </DialogContent>
  );

  if (trigger) {
    return (
      <>
        <div onClick={() => onOpenChange(true)}>{trigger}</div>
        <Dialog open={open} onOpenChange={handleDialogOpenChange}>
          {dialogContent}
        </Dialog>
      </>
    );
  }

  return (
    <Dialog open={open} onOpenChange={handleDialogOpenChange}>
      {dialogContent}
    </Dialog>
  );
}
