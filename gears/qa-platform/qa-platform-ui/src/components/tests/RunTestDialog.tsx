import { useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';
import { EnvironmentInfo, RunParameter, TestFileInfo } from '@/api/types';
import {
  useRunSingleTest,
  useEnvironments,
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
import { formatEnvironmentLabel, pickDefaultBranch } from '@/lib/utils';
import { getSelectedBranch } from '@/lib/selectedBranch';

interface RunTestDialogProps {
  test: TestFileInfo;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trigger?: React.ReactNode;
  initialBranch?: string;
  initialEnvironment?: string;
}

export function RunTestDialog({ test, open, onOpenChange, trigger, initialBranch, initialEnvironment }: RunTestDialogProps) {
  const [selectedEnvironment, setSelectedEnvironment] = useState<string>('');
  const [selectedBranch, setSelectedBranch] = useState<string>(initialBranch?.trim() || getSelectedBranch());
  const [parameters, setParameters] = useState<RunParameter[]>([]);
  const [exclusivity, setExclusivity] = useState<Exclusivity>('auto');
  const { data: environments } = useEnvironments();
  const { data: testRepos } = useTestRepositories(test.product_id ?? undefined);
  const runTest = useRunSingleTest();

  const environmentList = useMemo<EnvironmentInfo[]>(
    () => (Array.isArray(environments) ? environments.filter((p): p is EnvironmentInfo => !!p?.name) : []),
    [environments]
  );

  const selectedEnvironmentInfo = useMemo(
    () => environmentList.find((p) => p.name === selectedEnvironment) ?? null,
    [environmentList, selectedEnvironment]
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
      const environmentDefault = selectedEnvironmentInfo?.default_branch?.trim();
      if (environmentDefault) return environmentDefault;
      if (repoDefaultBranch?.trim()) return repoDefaultBranch.trim();
      return pickDefaultBranch(branchList);
    });
  }, [open, repoDefaultBranch, branchList, selectedEnvironmentInfo?.default_branch, initialBranch]);

  // When opened with an initial environment (e.g. re-running a failed test on the
  // environment it last ran on), default the selector to it — but only when it
  // still matches a configured environment; otherwise leave the choice to the user.
  useEffect(() => {
    if (!open) return;
    setSelectedEnvironment((prev) => {
      if (prev) return prev;
      const wanted = initialEnvironment?.trim();
      return wanted && environmentList.some((p) => p.name === wanted) ? wanted : prev;
    });
  }, [open, initialEnvironment, environmentList]);

  const resetDialogState = () => {
    setSelectedEnvironment('');
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
    if (!selectedEnvironment) {
      toast.error('Please select an environment');
      return;
    }

    runTest.mutate(
      {
        planId: test.plan_id,
        testFile: test.test_file,
        platform: selectedEnvironment,
        branch: selectedBranch || undefined,
        parameters: cleanRunParameters(parameters),
        exclusive: exclusivityToParam(exclusivity),
      },
      {
        onSuccess: (data) => {
          handleDialogOpenChange(false);
          if (isQueued(data)) {
            toast.success('Test queued', {
              description: 'The environment is busy with an exclusive run. It will start automatically.',
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
          Pick an environment and branch to run{' '}
          <span className="font-mono font-medium text-foreground">{test.test_file}</span>{' '}
          from {test.plan_name}
        </DialogDescription>
      </DialogHeader>
      <div className="space-y-4 py-4">
        <div className="space-y-2">
          <Label htmlFor="environment">Environment</Label>
          {environmentList.length > 0 ? (
            <Combobox
              id="environment"
              value={selectedEnvironment}
              onChange={(next) => {
                setSelectedEnvironment(next);
                const nextDefault = environmentList
                  .find((p) => p.name === next)
                  ?.default_branch?.trim();
                if (nextDefault) {
                  setSelectedBranch(nextDefault);
                }
              }}
              options={environmentList.map((p) => ({ value: p.name, label: formatEnvironmentLabel(p) }))}
              placeholder="Select environment"
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              No environments configured. Please add an environment first.
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
          disabled={!selectedEnvironment || runTest.isPending}
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
