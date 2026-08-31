import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';
import { PlatformInfo, RunParameter, TestFileInfo } from '@/api/types';
import {
  useRunSingleTest,
  usePlatforms,
  useTestRepositories,
  useTestRepoBranchesForRepos,
  isQueued,
} from '@/api/hooks';
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
import { formatPlatformLabel, pickDefaultBranch } from '@/lib/utils';
import { getSelectedBranch } from '@/lib/selectedBranch';

interface RunTestDialogProps {
  test: TestFileInfo;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger?: React.ReactNode;
  initialBranch?: string;
  initialPlatform?: string;
}

export function RunTestDialog({ test, open, onOpenChange, trigger, initialBranch, initialPlatform }: RunTestDialogProps) {
  const [selectedPlatform, setSelectedPlatform] = useState<string>('');
  const [selectedBranch, setSelectedBranch] = useState<string>(initialBranch?.trim() || getSelectedBranch());
  const [parameters, setParameters] = useState<RunParameter[]>([]);
  const [exclusivity, setExclusivity] = useState<Exclusivity>('auto');
  const { data: platforms } = usePlatforms();
  const { data: testRepos } = useTestRepositories(test.product_id ?? undefined);
  const runTest = useRunSingleTest();

  const platformList = useMemo<PlatformInfo[]>(
    () => (Array.isArray(platforms) ? platforms.filter((p): p is PlatformInfo => !!p?.name) : []),
    [platforms]
  );

  const selectedPlatformInfo = useMemo(
    () => platformList.find((p) => p.name === selectedPlatform) ?? null,
    [platformList, selectedPlatform]
  );

  const repos = useMemo(() => (Array.isArray(testRepos) ? testRepos : []), [testRepos]);

  // The specific repo this test's default branch hint is based on: the test's
  // own repo when known, else the product's first repo (only used for the
  // "leave empty to use repository default branch" hint below — the branch
  // *options* come from every repo the product has, not just this one, since
  // a product commonly has more than one git repository registered).
  const repoId = useMemo(() => {
    if (test.repo_id) return test.repo_id;
    return repos[0]?.id ?? null;
  }, [test.repo_id, repos]);

  const repoDefaultBranch = useMemo(() => {
    if (!repoId) return null;
    return repos.find((r) => r.id === repoId)?.default_branch ?? null;
  }, [repoId, repos]);

  const repoIds = useMemo(() => repos.map((r) => r.id), [repos]);
  const branchList = useTestRepoBranchesForRepos(repoIds);

  useEffect(() => {
    if (!open) return;
    setSelectedBranch((prev) => {
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

  // When opened with an initial platform (e.g. re-running a failed test on the
  // platform it last ran on), default the selector to it — but only when it
  // still matches a configured platform; otherwise leave the choice to the user.
  useEffect(() => {
    if (!open) return;
    setSelectedPlatform((prev) => {
      if (prev) return prev;
      const wanted = initialPlatform?.trim();
      return wanted && platformList.some((p) => p.name === wanted) ? wanted : prev;
    });
  }, [open, initialPlatform, platformList]);

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

    runTest.mutate(
      {
        planId: test.plan_id,
        testFile: test.test_file,
        platform: selectedPlatform,
        branch: selectedBranch || undefined,
        parameters: cleanRunParameters(parameters),
        exclusive: exclusivityToParam(exclusivity),
      },
      {
        onSuccess: (data) => {
          handleDialogOpenChange(false);
          if (isQueued(data)) {
            toast.success('Test queued', {
              description: 'The platform is busy with an exclusive run. It will start automatically.',
              duration: 8000,
            });
            return;
          }
          const workflowName = data.workflow_name;
          toast.success('Test started', {
            duration: 8000,
            action: {
              label: 'Open Run',
              onClick: () => { window.location.href = `/runs/${workflowName}`; },
            },
          });
        },
        onError: (error) => {
          toast.error('Failed to start test', {
            description: String(error),
          });
        },
      }
    );
  };

  const dialogContent = (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Run Test</DialogTitle>
        <DialogDescription>
          Pick a platform and branch to run{' '}
          <span className="font-mono font-medium text-foreground">{test.test_file}</span>{' '}
          from {test.plan_name}
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
          disabled={runTest.isPending}
        />

        <RunParametersEditor
          value={parameters}
          onChange={setParameters}
          disabled={runTest.isPending}
        />
      </div>

      <DialogFooter>
        <Button variant="outline" onClick={() => handleDialogOpenChange(false)}>
          Cancel
        </Button>
        <Button
          onClick={handleRun}
          disabled={!selectedPlatform || runTest.isPending}
        >
          {runTest.isPending ? 'Starting...' : 'Run Test'}
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
