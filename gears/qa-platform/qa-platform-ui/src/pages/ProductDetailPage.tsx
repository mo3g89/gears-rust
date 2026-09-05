import { useEffect, useState } from 'react';
import { useParams, Link } from 'react-router-dom';
import { toast } from 'sonner';
import { useConfirm } from '@/components/ui/confirm-dialog';
import {
  useCreateTestRepository,
  useDeleteTestRepository,
  usePlans,
  useProduct,
  useSyncTestRepository,
  useTestRepositories,
  useSshKeys,
  useUpdateProduct,
} from '@/api/hooks';
import { useSelectedBranch } from '@/lib/selectedBranch';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { ProductCoverageCard } from '@/components/products/ProductCoverageCard';
import {
  Loader2,
  ArrowLeft,
  BarChart3,
  CalendarDays,
  Clock3,
  PlayCircle,
  Plus,
  Pencil,
  RefreshCw,
  Trash2,
} from 'lucide-react';

function formatDateTime(dateValue: string | null): string {
  if (!dateValue) return '-';
  const date = new Date(dateValue);
  if (Number.isNaN(date.getTime())) return '-';

  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(date);
}

function formatRelativeTime(dateValue: string | null): string | null {
  if (!dateValue) return null;
  const date = new Date(dateValue);
  if (Number.isNaN(date.getTime())) return null;

  const diffMs = Date.now() - date.getTime();
  const absSeconds = Math.floor(Math.abs(diffMs) / 1000);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });

  if (absSeconds < 60) return rtf.format(-Math.round(diffMs / 1000), 'second');
  const absMinutes = Math.floor(absSeconds / 60);
  if (absMinutes < 60) return rtf.format(-Math.round(diffMs / 60000), 'minute');
  const absHours = Math.floor(absMinutes / 60);
  if (absHours < 24) return rtf.format(-Math.round(diffMs / 3600000), 'hour');
  const absDays = Math.floor(absHours / 24);
  if (absDays < 30) return rtf.format(-Math.round(diffMs / 86400000), 'day');
  const absMonths = Math.floor(absDays / 30);
  if (absMonths < 12) return rtf.format(-Math.round(diffMs / (30 * 86400000)), 'month');
  return rtf.format(-Math.round(diffMs / (365 * 86400000)), 'year');
}

export function ProductDetailPage() {
  const { id } = useParams<{ id: string }>();
  const productId = id || '';

  const [branch] = useSelectedBranch();
  const { data: product, isLoading, error } = useProduct(productId);
  const { data: allPlans } = usePlans(branch);
  const { data: repos, isLoading: reposLoading } = useTestRepositories(productId);
  const { data: sshKeys } = useSshKeys();
  const updateProduct = useUpdateProduct();

  const createRepo = useCreateTestRepository();
  const deleteRepo = useDeleteTestRepository();
  const syncRepo = useSyncTestRepository();
  const confirm = useConfirm();

  const [repoDialogOpen, setRepoDialogOpen] = useState(false);
  const [productDialogOpen, setProductDialogOpen] = useState(false);
  const [productForm, setProductForm] = useState({
    name: '',
    key: '',
    description: '',
    tests_folder: '',
  });
  // Git only: an archive upload has no endpoint in this deployment, so the
  // form no longer offers one (REMOVED-SURFACES.md, Task 8a C4).
  const [repoForm, setRepoForm] = useState({
    name: '',
    url: '',
    default_branch: 'main',
    tests_root: '',
    ssh_key_id: '',
  });

  useEffect(() => {
    if (!product) return;
    setProductForm({
      name: product.name,
      key: product.key,
      description: product.description,
      tests_folder: product.tests_folder,
    });
  }, [product]);

  const productPlans =
    allPlans?.filter((plan) =>
      product
        ? plan.product_id === product.id || plan.dir_path.startsWith(product.tests_folder + '/')
        : false
    ) || [];

  const resetRepoForm = () => {
    setRepoForm({
      name: '',
      url: '',
      default_branch: 'main',
      tests_root: '',
      ssh_key_id: '',
    });
  };

  const handleCreateRepo = () => {
    if (!repoForm.name.trim()) {
      toast.error('Name is required');
      return;
    }

    if (!repoForm.url.trim()) {
      toast.error('Git URL is required');
      return;
    }

    createRepo.mutate(
      {
        name: repoForm.name.trim(),
        url: repoForm.url.trim(),
        product_id: productId,
        default_branch: repoForm.default_branch.trim() || 'main',
        tests_root: repoForm.tests_root.trim() || undefined,
        ssh_key_id: repoForm.ssh_key_id || undefined,
      },
      {
        onSuccess: () => {
          toast.success('Repository added');
          setRepoDialogOpen(false);
          resetRepoForm();
        },
        onError: (err) => {
          toast.error('Failed to add repository', { description: String(err) });
        },
      }
    );
  };

  const handleDeleteRepo = async (repoId: string, repoName: string) => {
    const ok = await confirm({
      title: `Delete test repository "${repoName}"?`,
      description: 'This repository link will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (!ok) return;

    deleteRepo.mutate(repoId, {
      onSuccess: () => toast.success('Repository deleted'),
      onError: (err) => toast.error('Failed to delete repository', { description: String(err) }),
    });
  };

  const handleSyncRepo = (repoId: string) => {
    syncRepo.mutate(repoId, {
      // A sync that FAILED still answers HTTP 200: the gear records the engine
      // error in `sync_error` on the returned row rather than failing the
      // request (see the OpenAPI description of POST /test-repos/{id}/sync), so
      // React Query calls this `onSuccess` either way. Reporting success here
      // unconditionally is what made a permanently broken repository announce
      // "Repository synced" -- so the row the gear just returned decides.
      onSuccess: (repo) =>
        repo.sync_error
          ? toast.error('Sync failed', { description: repo.sync_error })
          : toast.success('Repository synced'),
      onError: (err) => toast.error('Failed to sync repository', { description: String(err) }),
    });
  };

  const saveProduct = () => {
    // Guards `product.plugin_instance_id` below -- unreachable in practice
    // (the dialog this calls from only renders once `product` has loaded,
    // past the page's own loading/error early returns) but not visible to
    // the type checker from inside this closure. The toast makes that claim
    // self-checking: if it ever is reachable, Save stops being a silent
    // no-op (m-5).
    if (!product) {
      toast.error('Product data is not loaded yet');
      return;
    }
    if (!productForm.name.trim() || !productForm.key.trim() || !productForm.tests_folder.trim()) {
      toast.error('Product name, key, and tests folder are required');
      return;
    }

    updateProduct.mutate(
      {
        id: productId,
        data: {
          name: productForm.name.trim(),
          key: productForm.key.trim().toUpperCase(),
          description: productForm.description.trim(),
          tests_folder: productForm.tests_folder.trim(),
          // This dialog has no plugin picker (that's ProductsPage's job, Task
          // 23) -- it re-sends the product's own current binding untouched,
          // which is Step 3's "edit preserves it" applied to a form that never
          // displays the field at all. `product` is this page's own fetch, so
          // it is never stale relative to the id being edited.
          plugin_instance_id: product.plugin_instance_id ?? '',
        },
        // The product's own stored binding, so `productReqFromForm` (G-2) omits the
        // key rather than resending it: this dialog never changes it, so every save
        // through here must be a no-op on the binding, never a rebind attempt that
        // the live per-process plugin registry could refuse.
        currentPluginInstanceId: product.plugin_instance_id,
      },
      {
        onSuccess: () => {
          toast.success('Product updated');
          setProductDialogOpen(false);
        },
        onError: (err) => toast.error('Failed to update product', { description: String(err) }),
      }
    );
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !product) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Product not found</p>
        <Link to="/products"><Button variant="outline" className="mt-4">Back to Product Catalog</Button></Link>
      </div>
    );
  }

  const createdAt = formatDateTime(product.created_at);
  const createdAtRelative = formatRelativeTime(product.created_at);
  const updatedAt = formatDateTime(product.updated_at);
  const updatedAtRelative = formatRelativeTime(product.updated_at);

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/products">
          <Button variant="ghost" size="icon"><ArrowLeft className="h-5 w-5" /></Button>
        </Link>
        <div className="flex-1">
          <h1 className="text-xl font-semibold">{product.name}</h1>
          <p className="text-muted-foreground">{product.description || 'No description'}</p>
        </div>
        <Dialog open={productDialogOpen} onOpenChange={setProductDialogOpen}>
          <DialogTrigger asChild>
            <Button variant="outline">
              <Pencil className="mr-2 h-4 w-4" />
              Edit Product
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Edit Product</DialogTitle>
            </DialogHeader>
            <div className="space-y-4 py-2">
              <div className="space-y-2">
                <Label htmlFor="product-name">Product Name *</Label>
                <Input
                  id="product-name"
                  value={productForm.name}
                  onChange={(e) => setProductForm((prev) => ({ ...prev, name: e.target.value }))}
                  placeholder="Product name"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="product-key">Product Key *</Label>
                <Input
                  id="product-key"
                  value={productForm.key}
                  onChange={(e) => setProductForm((prev) => ({ ...prev, key: e.target.value.toUpperCase() }))}
                  placeholder="KEY"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="product-tests-folder">Tests Folder *</Label>
                <Input
                  id="product-tests-folder"
                  value={productForm.tests_folder}
                  onChange={(e) => setProductForm((prev) => ({ ...prev, tests_folder: e.target.value }))}
                  placeholder="key"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="product-description">Description</Label>
                <Input
                  id="product-description"
                  value={productForm.description}
                  onChange={(e) => setProductForm((prev) => ({ ...prev, description: e.target.value }))}
                  placeholder="Product description"
                />
              </div>
              <Button onClick={saveProduct} disabled={updateProduct.isPending} className="w-full">
                {updateProduct.isPending ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
                Save Product
              </Button>
            </div>
          </DialogContent>
        </Dialog>
        <span className="font-mono text-sm tabular-nums text-muted-foreground" title="Product key">
          {product.key}
        </span>
      </div>

      <div className="grid grid-cols-1 gap-6">
        <Card>
          <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>Product Details</CardTitle>
              <CardDescription>Registration and coverage details</CardDescription>
            </div>
            <div className="grid grid-cols-1 gap-2 sm:min-w-[250px]">
              <div className="rounded-md border bg-muted/30 p-2">
                <div className="flex items-center justify-between gap-2">
                  <span className="inline-flex items-center gap-2 text-[11px] uppercase tracking-wide text-muted-foreground">
                    <CalendarDays className="h-3.5 w-3.5" />
                    Created
                  </span>
                  {createdAtRelative && (
                    <Badge variant="outline" className="text-[11px]">
                      {createdAtRelative}
                    </Badge>
                  )}
                </div>
                <p className="mt-1 text-xs font-medium">{createdAt}</p>
              </div>

              <div className="rounded-md border bg-muted/30 p-2">
                <div className="flex items-center justify-between gap-2">
                  <span className="inline-flex items-center gap-2 text-[11px] uppercase tracking-wide text-muted-foreground">
                    <Clock3 className="h-3.5 w-3.5" />
                    Updated
                  </span>
                  {updatedAtRelative && (
                    <Badge variant="outline" className="text-[11px]">
                      {updatedAtRelative}
                    </Badge>
                  )}
                </div>
                <p className="mt-1 text-xs font-medium">{updatedAt}</p>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Product Key</span>
              <Badge>{product.key}</Badge>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Tests Folder</span>
              <span className="font-mono text-sm">{product.tests_folder}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Test Plans</span>
              <span>{productPlans.length} plan{productPlans.length !== 1 ? 's' : ''}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Test Sources</span>
              <span>{repos?.length || 0}</span>
            </div>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <div className="flex items-center justify-between gap-2">
            <div>
              <CardTitle>Test Repositories</CardTitle>
              <CardDescription>
                Git repositories linked to this product
              </CardDescription>
            </div>
            <Dialog
              open={repoDialogOpen}
              onOpenChange={(open) => {
                setRepoDialogOpen(open);
                if (open) resetRepoForm();
              }}
            >
              <DialogTrigger asChild>
                <Button>
                  <Plus className="h-4 w-4 mr-2" /> Add Git Repository
                </Button>
              </DialogTrigger>
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>Add Git Repository</DialogTitle>
                </DialogHeader>
                <div className="space-y-4 py-2">
                  <div className="space-y-2">
                    <Label htmlFor="repo-name">Name *</Label>
                    <Input
                      id="repo-name"
                      value={repoForm.name}
                      onChange={(e) => setRepoForm((prev) => ({ ...prev, name: e.target.value }))}
                      placeholder="Functional tests"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="repo-url">Git URL *</Label>
                    <Input
                      id="repo-url"
                      value={repoForm.url}
                      onChange={(e) => setRepoForm((prev) => ({ ...prev, url: e.target.value }))}
                      placeholder="https://git.example.com/team/tests.git"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="repo-ssh-key">SSH Key</Label>
                    <Select
                      value={repoForm.ssh_key_id || '__none__'}
                      onValueChange={(value) =>
                        setRepoForm((prev) => ({
                          ...prev,
                          ssh_key_id: value === '__none__' ? '' : value,
                        }))
                      }
                    >
                      <SelectTrigger id="repo-ssh-key">
                        <SelectValue placeholder="No SSH key" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="__none__">No SSH key</SelectItem>
                        {(sshKeys || []).map((key) => (
                          <SelectItem key={key.id} value={key.id}>
                            {key.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <p className="text-xs text-muted-foreground">
                      For <code>git@...</code> repositories select a private SSH key.
                    </p>
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="repo-branch">Default Branch</Label>
                    <Input
                      id="repo-branch"
                      value={repoForm.default_branch}
                      onChange={(e) => setRepoForm((prev) => ({ ...prev, default_branch: e.target.value }))}
                      placeholder="main"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="repo-tests-root">Tests Root Folder</Label>
                    <Input
                      id="repo-tests-root"
                      value={repoForm.tests_root}
                      onChange={(e) => setRepoForm((prev) => ({ ...prev, tests_root: e.target.value }))}
                      placeholder="qa/product-tests (optional)"
                    />
                    <p className="text-xs text-muted-foreground">
                      Optional sub-folder inside the repository that contains <code>plans/</code> and <code>tests/</code>.
                    </p>
                  </div>
                  <Button
                    className="w-full"
                    onClick={handleCreateRepo}
                    disabled={createRepo.isPending}
                  >
                    {createRepo.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin mr-2" />
                    ) : null}
                    Save Repository
                  </Button>
                </div>
              </DialogContent>
            </Dialog>
          </div>
        </CardHeader>
        <CardContent>
          {reposLoading ? (
            <div className="py-8 text-center text-muted-foreground">Loading repositories...</div>
          ) : !repos?.length ? (
            <div className="rounded-md border border-dashed py-10 text-center">
              <p className="text-sm font-medium">No test repositories configured</p>
              <p className="text-xs text-muted-foreground mt-1">
                Add a git repository to parse tests and plans for this product.
              </p>
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Source</TableHead>
                  <TableHead>Tests Root</TableHead>
                  <TableHead>Default Ref</TableHead>
                  <TableHead>Location</TableHead>
                  <TableHead>Auth</TableHead>
                  <TableHead>Sync</TableHead>
                  <TableHead className="w-[120px]"></TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {repos.map((repo) => (
                  <TableRow key={repo.id}>
                    <TableCell className="font-medium">{repo.name}</TableCell>
                    <TableCell>
                      {repo.source_type === 'archive' ? (
                        <Badge variant="secondary">Archive</Badge>
                      ) : (
                        <div className="flex flex-col gap-1">
                          <Badge variant="secondary">Git</Badge>
                          {repo.ssh_key_name ? (
                            <span className="text-xs text-muted-foreground">
                              SSH: {repo.ssh_key_name}
                            </span>
                          ) : null}
                        </div>
                      )}
                    </TableCell>
                    <TableCell>
                      <code className="text-xs">{repo.tests_root || '.'}</code>
                    </TableCell>
                    <TableCell>
                      <Badge variant="outline">{repo.default_branch}</Badge>
                    </TableCell>
                    <TableCell className="max-w-[420px] truncate text-muted-foreground">
                      {repo.source_type === 'archive'
                        ? repo.archive_file_name || repo.url
                        : repo.url}
                    </TableCell>
                    <TableCell>
                      {repo.source_type === 'archive'
                        ? '-'
                        : (repo.has_token ? 'Token set' : 'Public/none')}
                    </TableCell>
                    <TableCell>
                      {repo.source_type === 'archive' ? (
                        '-'
                      ) : repo.sync_error ? (
                        // Populated sync_error always wins, even if last_synced_at also
                        // holds a past timestamp (synced once, now failing) — see
                        // repos.rs::record_sync_failure, which leaves last_synced_at
                        // untouched on a failed attempt.
                        <Badge variant="destructive" title={repo.sync_error}>
                          Sync failed
                        </Badge>
                      ) : repo.last_synced_at ? (
                        <span className="text-muted-foreground">
                          {formatDateTime(repo.last_synced_at)}
                        </span>
                      ) : (
                        <Badge variant="secondary">Never synced</Badge>
                      )}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="icon"
                          onClick={() => handleSyncRepo(repo.id)}
                          disabled={syncRepo.isPending}
                          title={repo.source_type === 'archive' ? 'Rescan archive source' : 'Sync repository'}
                        >
                          <RefreshCw className="h-4 w-4" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon"
                          onClick={() => handleDeleteRepo(repo.id, repo.name)}
                          disabled={deleteRepo.isPending}
                          title="Delete repository"
                        >
                          <Trash2 className="h-4 w-4 text-destructive" />
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <ProductCoverageCard productId={productId} />

      <Card>
        <CardHeader>
          <CardTitle>Quick Actions</CardTitle>
          <CardDescription>Shortcuts for this product</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <Link to="/analytics">
            <Button variant="outline" className="w-full">
              <BarChart3 className="h-4 w-4 mr-2" /> View Analytics
            </Button>
          </Link>
          <Link to="/plans">
            <Button variant="outline" className="w-full">
              <PlayCircle className="h-4 w-4 mr-2" /> View All Test Plans
            </Button>
          </Link>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Test Plans</CardTitle>
          <CardDescription>Plans belonging to this product</CardDescription>
        </CardHeader>
        <CardContent>
          {productPlans.length === 0 ? (
            <p className="text-sm text-muted-foreground text-center py-8">No test plans found in this product folder</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Plan</TableHead>
                  <TableHead>Description</TableHead>
                  <TableHead>Tests</TableHead>
                  <TableHead>Tags</TableHead>
                  <TableHead className="w-[200px]"></TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {productPlans.map((plan) => (
                  <TableRow key={plan.id}>
                    <TableCell>
                      <div className="flex items-center gap-2">
                        <Link to={`/plans/${plan.id}`} className="font-medium hover:underline">{plan.plan.name}</Link>
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
                    <TableCell className="text-muted-foreground max-w-[250px] truncate">{plan.plan.description}</TableCell>
                    <TableCell>{plan.test_files.length} files</TableCell>
                    <TableCell>
                      <div className="flex flex-wrap gap-1">
                        {plan.plan.tags.slice(0, 3).map((tag) => (
                          <Badge key={tag} variant="secondary" className="text-xs">{tag}</Badge>
                        ))}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-2 justify-end">
                        <Link to={`/plans/${plan.id}`}>
                          <Button variant="outline" size="sm">
                            <PlayCircle className="h-4 w-4 mr-1" /> Details
                          </Button>
                        </Link>
                        <Link to={`/analytics/plan/${plan.id}`}>
                          <Button variant="outline" size="sm">
                            <BarChart3 className="h-4 w-4 mr-1" /> Analytics
                          </Button>
                        </Link>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
