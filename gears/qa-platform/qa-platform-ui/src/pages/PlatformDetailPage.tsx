import { Link, useParams } from 'react-router-dom';
import { toast } from 'sonner';
import {
  usePlatformDetails,
  usePlatformVariables,
  useProducts,
  useRefreshPlatform,
  useUpdatePlatformVariables,
} from '@/api/hooks';
import { ClusterHealthCard } from '@/components/platforms/ClusterHealthCard';
import { EditPlatformDialog } from '@/components/platforms/EditPlatformDialog';
import { QueuedRunsCard } from '@/components/runs/QueuedRunsCard';
import { VariablesEditor } from '@/components/settings/VariablesEditor';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import {
  AlertTriangle,
  ArrowLeft,
  CalendarDays,
  Loader2,
  Pencil,
  RefreshCw,
  Server,
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

export function PlatformDetailPage() {
  const { name } = useParams<{ name: string }>();
  const platformName = name ? decodeURIComponent(name) : '';
  const { data: platform, isLoading, error } = usePlatformDetails(platformName);
  const { data: products } = useProducts();
  const refreshPlatform = useRefreshPlatform();

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !platform) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Failed to load platform</p>
        <p className="text-sm text-muted-foreground mt-2">{error ? String(error) : 'Platform not found'}</p>
        <Link to="/platforms">
          <Button variant="outline" className="mt-4">Back to Platforms</Button>
        </Link>
      </div>
    );
  }

  const linkedProduct = (products || []).find((product) => product.id === platform.product_id);
  const createdAt = formatDateTime(platform.created_at);
  const createdAtRelative = formatRelativeTime(platform.created_at);
  const versionDetectedAt = formatDateTime(platform.version_detected_at);
  const versionDetectedAtRelative = formatRelativeTime(platform.version_detected_at);

  // `POST /qa/v1/platforms/{id}/refresh` answers HTTP 200 even when detection failed --
  // the gear records the failure in `version_detect_error` on the returned row rather than
  // failing the request. Reporting success on the 200 alone would be the exact "lying
  // toast" defect fixed in 89b55eb22 for the test-repo sync button, so the returned row
  // decides which toast fires, not the HTTP status.
  const handleRefresh = () => {
    refreshPlatform.mutate(platform.name, {
      onSuccess: (result) =>
        result.version_detect_error
          ? toast.error('Detection failed', { description: result.version_detect_error })
          : toast.success('Platform refreshed'),
      onError: (err) => toast.error('Failed to refresh platform', { description: String(err) }),
    });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/platforms">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div className="flex-1">
          <h1 className="text-xl font-semibold flex items-center gap-2">
            <Server className="h-5 w-5" />
            {platform.name}
            {/* Same badge, same wording as the Platforms table's — an operator who
                learned it there should not have to re-learn it here. */}
            {platform.is_default && (
              <span
                className="rounded border border-primary/40 bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary"
                title={'The "Default cluster" option in the Run Plan and Schedule dialogs resolves to this platform for its product'}
              >
                Default
              </span>
            )}
          </h1>
          <p className="text-muted-foreground">
            Platform registration and configuration
          </p>
        </div>
        <EditPlatformDialog
          platform={{
            id: platform.id,
            name: platform.name,
            created_at: platform.created_at,
            description: platform.description,
            product_id: platform.product_id,
            is_default: platform.is_default,
            version: platform.version,
            build: platform.build,
            namespace: platform.namespace,
            vhp_base_url: platform.vhp_base_url,
            version_detected_at: platform.version_detected_at,
            version_detect_error: platform.version_detect_error,
            default_branch: platform.default_branch,
            cluster: platform.cluster,
          }}
          trigger={
            <Button variant="outline">
              <Pencil className="h-4 w-4 mr-2" />
              Edit
            </Button>
          }
        />
      </div>

      <ClusterHealthCard cluster={platform.cluster} />

      <QueuedRunsCard platform={platform.name} />

      <div className="grid grid-cols-1 items-start gap-6 lg:grid-cols-2">
        <Card>
          <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>Platform Metadata</CardTitle>
              <CardDescription>Registration and configuration details</CardDescription>
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
            </div>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Product</span>
              <span>
                {linkedProduct ? (
                  <Link to={`/products/${linkedProduct.id}`} className="hover:underline">
                    {linkedProduct.name} ({linkedProduct.key})
                  </Link>
                ) : (
                  platform.product_id || '-'
                )}
              </span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Test Repositories</span>
              {linkedProduct ? (
                <Link to={`/products/${linkedProduct.id}`} className="text-sm hover:underline">
                  Manage on Product page
                </Link>
              ) : (
                <span>-</span>
              )}
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Default Branch</span>
              <span>
                {platform.default_branch?.trim() ? (
                  <code className="text-xs">{platform.default_branch}</code>
                ) : (
                  <em className="text-muted-foreground">Falls back to repository default</em>
                )}
              </span>
            </div>
            <div className="flex justify-between gap-4">
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Platform Version</span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  disabled={refreshPlatform.isPending}
                  onClick={handleRefresh}
                  title="Run detection again"
                  aria-label="Run detection again"
                >
                  <RefreshCw
                    className={`h-3.5 w-3.5 ${refreshPlatform.isPending ? 'animate-spin' : ''}`}
                  />
                </Button>
              </div>
              <span>
                {platform.version || (
                  <em className="text-muted-foreground">Not observed</em>
                )}
              </span>
            </div>
            {(platform.version_detected_at || platform.version_detect_error) && (
              <div className="flex justify-between gap-4">
                <span className="text-muted-foreground">Last Detected</span>
                <span className="text-right text-sm">
                  {platform.version_detected_at ? (
                    <>
                      {versionDetectedAt}
                      {versionDetectedAtRelative && (
                        <span className="ml-1 text-xs text-muted-foreground">
                          ({versionDetectedAtRelative})
                        </span>
                      )}
                    </>
                  ) : (
                    '-'
                  )}
                </span>
              </div>
            )}
            {platform.version_detect_error && (
              <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 p-2 text-sm text-destructive">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                <span>{platform.version_detect_error}</span>
              </div>
            )}
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Platform Build</span>
              <span>{platform.build || '-'}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Kubernetes Namespace</span>
              <span>{platform.namespace || '-'}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">VHP Base URL</span>
              <span className="max-w-[360px] truncate text-right">{platform.vhp_base_url || '-'}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Platform ID</span>
              <span
                className="font-mono text-xs text-muted-foreground"
                title="Internal stable identifier. Renaming the platform leaves this unchanged."
              >
                {platform.id}
              </span>
            </div>
            <div className="space-y-2">
              <span className="text-muted-foreground">Description</span>
              <p className="text-sm whitespace-pre-wrap rounded-md border p-3">
                {platform.description || 'No description'}
              </p>
            </div>
          </CardContent>
        </Card>

        <PlatformVariablesCard platformName={platform.name} />
      </div>
    </div>
  );
}

function PlatformVariablesCard({ platformName }: { platformName: string }) {
  const { data, isLoading } = usePlatformVariables(platformName);
  const update = useUpdatePlatformVariables(platformName);

  return (
    <Card>
      <CardHeader className="space-y-1 py-3">
        <CardTitle className="text-sm">Platform Variables</CardTitle>
        <CardDescription className="text-xs">
          Injected as env vars on every run. Override pipeline variables of the same name.
        </CardDescription>
      </CardHeader>
      <CardContent className="pb-3">
        <VariablesEditor
          data={data}
          isLoading={isLoading}
          isSaving={update.isPending}
          onSave={(next) => update.mutateAsync(next)}
          emptyHint="Add a row above to ship a variable with every run on this platform."
        />
      </CardContent>
    </Card>
  );
}
