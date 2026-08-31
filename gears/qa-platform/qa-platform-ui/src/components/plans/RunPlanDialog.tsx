import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';
import { PlatformInfo, RunParameter, TestPlanInfo } from '@/api/types';
import { useRunPlan, usePlatforms, useTestRepositories, useTestRepoBranches, isQueued } from '@/api/hooks';
import { formatPlatformLabel } from '@/lib/utils';
import { defaultPlatformForProduct, defaultPlatformLabel } from '@/lib/defaultPlatform';
import { getSelectedBranch } from '@/lib/selectedBranch';
import { RunParametersEditor, cleanRunParameters } from '@/components/runs/RunParametersEditor';
import { ExclusivitySelect, Exclusivity, exclusivityToParam } from '@/components/runs/ExclusivitySelect';
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

interface RunPlanDialogProps {
  plan: TestPlanInfo;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger?: React.ReactNode;
  /** Branch the user was browsing on the listing page; used as the initial
   *  branch so the run targets the same source they were looking at. */
  initialBranch?: string;
}

export function RunPlanDialog({ plan, open, onOpenChange, trigger, initialBranch }: RunPlanDialogProps) {
  const [selectedPlatform, setSelectedPlatform] = useState<string>('');
  const [selectedBranch, setSelectedBranch] = useState<string>(initialBranch?.trim() || getSelectedBranch());
  const [parameters, setParameters] = useState<RunParameter[]>([]);
  const [exclusivity, setExclusivity] = useState<Exclusivity>('auto');
  const { data: platforms } = usePlatforms();
  const { data: testRepos } = useTestRepositories(plan.product_id ?? undefined);
  const runPlan = useRunPlan();

  const platformList = useMemo<PlatformInfo[]>(
    () => (Array.isArray(platforms) ? platforms.filter((p): p is PlatformInfo => !!p?.name) : []),
    [platforms]
  );

  const selectedPlatformInfo = useMemo(
    () => platformList.find((p) => p.name === selectedPlatform) ?? null,
    [platformList, selectedPlatform]
  );

  // What "Default cluster" means here — see `defaultPlatformForProduct`'s doc for why it
  // is the product's platform and not legacy's "no platform at all".
  const defaultPlatform = useMemo(
    () => defaultPlatformForProduct(platformList, plan.product_id),
    [platformList, plan.product_id]
  );

  // Branch source: the test repository attached to the plan's product. With
  // one-repo-per-product, this collapses to a single repo.
  const repoId = useMemo(() => {
    if (plan.repo_id) return plan.repo_id;
    const first = Array.isArray(testRepos) ? testRepos[0] : undefined;
    return first?.id ?? null;
  }, [plan.repo_id, testRepos]);

  const repoDefaultBranch = useMemo(() => {
    if (!repoId || !Array.isArray(testRepos)) return null;
    return testRepos.find((r) => r.id === repoId)?.default_branch ?? null;
  }, [repoId, testRepos]);

  const { data: branchList } = useTestRepoBranches(repoId ?? '');

  useEffect(() => {
    if (!open) return;
    // Initial branch precedence: branch being browsed → platform default →
    // repo default → first known branch.
    setSelectedBranch((prev) => {
      if (prev) return prev;
      if (initialBranch?.trim()) return initialBranch.trim();
      if (getSelectedBranch()) return getSelectedBranch();
      const platformDefault = selectedPlatformInfo?.default_branch?.trim();
      if (platformDefault) return platformDefault;
      if (repoDefaultBranch?.trim()) return repoDefaultBranch.trim();
      const branches = Array.isArray(branchList) ? branchList : [];
      return branches[0] ?? '';
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, repoDefaultBranch, branchList, selectedPlatformInfo?.default_branch, initialBranch]);

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
    // "Default cluster" (the empty value) resolves to the product's platform. Refusing
    // here rather than launching keeps an ambiguous default from quietly running the
    // suite against an environment nobody picked.
    const effectivePlatform = selectedPlatform || defaultPlatform?.name;
    if (!effectivePlatform) {
      toast.error('Failed to start plan', {
        description: platformList.length
          ? 'This product has no single default platform. Pick one explicitly.'
          : 'No platforms are configured. Add one before launching.',
      });
      return;
    }
    runPlan.mutate(
      {
        planId: plan.id,
        platform: effectivePlatform,
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
          toast.success('Plan started', {
            duration: 8000,
            action: {
              label: 'Open Run',
              onClick: () => { window.location.href = `/runs/${workflowName}`; },
            },
          });
        },
        onError: (error) => {
          toast.error('Failed to start plan', {
            description: String(error),
          });
        },
      }
    );
  };

  const branches = Array.isArray(branchList) ? branchList : [];

  const dialogContent = (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Run Test Plan</DialogTitle>
        <DialogDescription>
          Choose a platform and branch to run "{plan.plan.name}"
        </DialogDescription>
      </DialogHeader>
      <div className="space-y-4 py-4">
        <div className="space-y-2">
          <Label htmlFor="platform">Platform (Optional)</Label>
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
              options={[
                { value: '', label: defaultPlatformLabel(defaultPlatform) },
                ...platformList.map((p) => ({ value: p.name, label: formatPlatformLabel(p) })),
              ]}
              placeholder={defaultPlatformLabel(defaultPlatform)}
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              No platforms configured. The plan will run with no target platform, against
              whatever cluster the runner itself is deployed into.
            </p>
          )}
        </div>

        <div className="space-y-2">
          <Label htmlFor="branch">Branch</Label>
          <Combobox
            id="branch"
            value={selectedBranch}
            onChange={setSelectedBranch}
            options={branches}
            allowCustom
            placeholder={repoDefaultBranch ? `${repoDefaultBranch} (default)` : 'main'}
            emptyText="No branches cached"
          />
          {repoDefaultBranch && !selectedBranch && (
            <p className="text-xs text-muted-foreground">
              Leave empty to use repository default branch ({repoDefaultBranch}).
            </p>
          )}
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
        <Button
          variant="outline"
          onClick={() => handleDialogOpenChange(false)}
        >
          Cancel
        </Button>
        <Button
          onClick={handleRun}
          disabled={runPlan.isPending}
        >
          {runPlan.isPending ? 'Starting...' : 'Run Plan'}
        </Button>
      </DialogFooter>
    </DialogContent>
  );

  if (trigger) {
    return (
      <>
        <div onClick={() => onOpenChange(true)}>
          {trigger}
        </div>
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
