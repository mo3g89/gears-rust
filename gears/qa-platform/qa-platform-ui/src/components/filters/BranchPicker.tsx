import { useMemo, useState } from 'react';
import {
  useTestRepositories,
  useTestRepoBranchesForRepos,
  useRefreshBranch,
} from '@/api/hooks';
import { Combobox } from '@/components/ui/combobox';
import { GitBranch, RefreshCw } from 'lucide-react';
import { toast } from 'sonner';

interface BranchPickerProps {
  value: string;
  onChange: (branch: string) => void;
  /** Optional product scope. When set, branches come from that product's
   *  repositories; otherwise every registered repository is used. */
  productId?: string;
  className?: string;
  /** Called when a branch option is hovered — wire to a prefetch so the
   *  listing for that branch is warming before the user clicks. */
  onPrefetch?: (branch: string) => void;
}

/**
 * Searchable branch selector used on listing pages (Plans, Test Catalog) so
 * the user can browse plans/tests from a non-default branch — e.g. to see
 * tests that only exist on a feature branch. An empty value means "each
 * repository's default branch". A product can have more than one git
 * repository registered, so branches are merged across all of them (the
 * backend already lists plans/tests from every repo for a given branch —
 * see `list_plans_with_repos`), rather than only the most recently added one.
 */
export function BranchPicker({ value, onChange, productId, className, onPrefetch }: BranchPickerProps) {
  const { data: reposData } = useTestRepositories(productId);
  const repos = useMemo(() => (Array.isArray(reposData) ? reposData : []), [reposData]);
  const repoIds = useMemo(() => repos.map((r) => r.id), [repos]);
  const repoDefault =
    repos.length === 1 ? repos[0].default_branch?.trim() || 'main' : 'default branch';
  const branches = useTestRepoBranchesForRepos(repoIds);
  const refresh = useRefreshBranch();
  const [isRefreshing, setIsRefreshing] = useState(false);

  const handleRefresh = async () => {
    if (repos.length === 0 || isRefreshing) return;
    setIsRefreshing(true);
    const results = await Promise.allSettled(
      repos.map((r) => refresh.mutateAsync({ repoId: r.id, branch: value || undefined }))
    );
    setIsRefreshing(false);
    const failed = results.filter((r) => r.status === 'rejected').length;
    if (failed === 0) {
      toast.success(`Refreshed ${value || repoDefault} across ${repos.length} repositor${repos.length === 1 ? 'y' : 'ies'}`, {
        duration: 2500,
      });
    } else {
      toast.error(`Failed to refresh ${failed} of ${repos.length} repositories`);
    }
  };

  return (
    <div className={`flex items-center gap-2 ${className ?? ''}`}>
      <GitBranch className="h-4 w-4 text-muted-foreground" />
      <Combobox
        value={value}
        onChange={onChange}
        options={branches}
        allowCustom
        placeholder={`${repoDefault} (default)`}
        emptyText="No branches cached"
        className="w-[240px]"
        onOptionHover={onPrefetch}
      />
      <button
        type="button"
        onClick={handleRefresh}
        disabled={repos.length === 0 || isRefreshing}
        title="Fetch latest commits for this branch across all repositories"
        aria-label="Refresh branch"
        className="inline-flex h-8 w-8 items-center justify-center rounded-md border text-muted-foreground hover:bg-accent hover:text-accent-foreground disabled:opacity-50"
      >
        <RefreshCw className={`h-4 w-4 ${isRefreshing ? 'animate-spin' : ''}`} />
      </button>
      {value && (
        <button
          type="button"
          className="text-xs text-muted-foreground hover:text-foreground underline"
          onClick={() => onChange('')}
        >
          reset
        </button>
      )}
    </div>
  );
}
