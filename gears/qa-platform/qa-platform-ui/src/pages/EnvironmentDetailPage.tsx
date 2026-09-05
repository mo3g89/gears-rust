import { Link, useParams } from 'react-router-dom';
import { toast } from 'sonner';
import {
  useEnvironmentDetails,
  useEnvironmentVariables,
  useProducts,
  useRefreshEnvironment,
  useUpdateEnvironmentVariables,
} from '@/api/hooks';
import { EditEnvironmentDialog } from '@/components/environments/EditEnvironmentDialog';
import { useProductPlugins } from '@/api/productPlugins';
import { attrText, detailRows, pluginForProduct } from '@/lib/fieldDesc';
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

export function EnvironmentDetailPage() {
  const { name } = useParams<{ name: string }>();
  const environmentName = name ? decodeURIComponent(name) : '';
  const { data: environment, isLoading, error } = useEnvironmentDetails(environmentName);
  const { data: products } = useProducts();
  // Unconditional, with every other hook: it is read far below an early
  // return, and React requires the call order to be stable.
  const { data: productPlugins } = useProductPlugins();
  const refreshEnvironment = useRefreshEnvironment();

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !environment) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Failed to load environment</p>
        <p className="text-sm text-muted-foreground mt-2">{error ? String(error) : 'Environment not found'}</p>
        <Link to="/environments">
          <Button variant="outline" className="mt-4">Back to Environments</Button>
        </Link>
      </div>
    );
  }

  const linkedProduct = (products || []).find((product) => product.id === environment.product_id);
  // The plugin whose descriptors this page renders. `[]` when the catalogue
  // has not loaded or the product's plugin is not registered here, which
  // degrades to "no descriptor rows" rather than blanking the page.
  const observedSchema =
    pluginForProduct(productPlugins ?? [], linkedProduct?.plugin_instance_id)?.observed_schema ??
    [];
  const createdAt = formatDateTime(environment.created_at);
  const createdAtRelative = formatRelativeTime(environment.created_at);
  const versionDetectedAt = formatDateTime(environment.version_detected_at);
  const versionDetectedAtRelative = formatRelativeTime(environment.version_detected_at);

  // `POST /qa/v1/environments/{id}/refresh` answers HTTP 200 even when detection failed --
  // the gear records the failure in `version_detect_error` on the returned row rather than
  // failing the request. Reporting success on the 200 alone would be the exact "lying
  // toast" defect fixed in 89b55eb22 for the test-repo sync button, so the returned row
  // decides which toast fires, not the HTTP status.
  const handleRefresh = () => {
    refreshEnvironment.mutate(environment.name, {
      onSuccess: (result) =>
        result.version_detect_error
          ? toast.error('Detection failed', { description: result.version_detect_error })
          : toast.success('Environment refreshed'),
      onError: (err) => toast.error('Failed to refresh environment', { description: String(err) }),
    });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to="/environments">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div className="flex-1">
          <h1 className="text-xl font-semibold flex items-center gap-2">
            <Server className="h-5 w-5" />
            {environment.name}
            {/* Same badge, same wording as the Environments table's — an operator who
                learned it there should not have to re-learn it here. */}
            {environment.is_default && (
              <span
                className="rounded border border-primary/40 bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary"
                title={'The "Default cluster" option in the Run Plan and Schedule dialogs resolves to this environment for its product'}
              >
                Default
              </span>
            )}
          </h1>
          <p className="text-muted-foreground">
            Environment registration and configuration
          </p>
        </div>
        <EditEnvironmentDialog
          environment={{
            id: environment.id,
            name: environment.name,
            created_at: environment.created_at,
            description: environment.description,
            product_id: environment.product_id,
            is_default: environment.is_default,
            available: environment.available,
            version: environment.version,
            build: environment.build,
            observed_attrs: environment.observed_attrs,
            health_state: environment.health_state,
            health_detail: environment.health_detail,
            version_detected_at: environment.version_detected_at,
            version_detect_error: environment.version_detect_error,
            default_branch: environment.default_branch,
          }}
          trigger={
            <Button variant="outline">
              <Pencil className="h-4 w-4 mr-2" />
              Edit
            </Button>
          }
        />
      </div>

      <QueuedRunsCard environment={environment.name} />

      <div className="grid grid-cols-1 items-start gap-6 lg:grid-cols-2">
        <Card>
          <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>Environment Metadata</CardTitle>
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
                  environment.product_id || '-'
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
                {environment.default_branch?.trim() ? (
                  <code className="text-xs">{environment.default_branch}</code>
                ) : (
                  <em className="text-muted-foreground">Falls back to repository default</em>
                )}
              </span>
            </div>
            <div className="flex justify-between gap-4">
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Environment Version</span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  disabled={refreshEnvironment.isPending}
                  onClick={handleRefresh}
                  title="Run detection again"
                  aria-label="Run detection again"
                >
                  <RefreshCw
                    className={`h-3.5 w-3.5 ${refreshEnvironment.isPending ? 'animate-spin' : ''}`}
                  />
                </Button>
              </div>
              <span>
                {environment.version || (
                  <em className="text-muted-foreground">Not observed</em>
                )}
              </span>
            </div>
            {(environment.version_detected_at || environment.version_detect_error) && (
              <div className="flex justify-between gap-4">
                <span className="text-muted-foreground">Last Detected</span>
                <span className="text-right text-sm">
                  {environment.version_detected_at ? (
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
            {environment.version_detect_error && (
              <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 p-2 text-sm text-destructive">
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                <span>{environment.version_detect_error}</span>
              </div>
            )}
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Environment Build</span>
              <span>{environment.build || '-'}</span>
            </div>
            {/*
              ONE ROW PER `in_detail` DESCRIPTOR, in the plugin's declaration
              order. "Kubernetes Namespace" and "VHP Base URL" stood here as
              fixed rows -- two of VHP's attributes hardcoded into a page every
              product shares. VHP's own page looks the same afterwards, because
              its plugin declares `namespace` and `baseDomain` with
              `in_detail: true`; what changed is that a second product now gets
              its own fields rather than two empty rows about someone else's.
            */}
            {detailRows(observedSchema).map((field) => (
              <div key={field.key} className="flex justify-between gap-4">
                <span className="text-muted-foreground" title={field.help ?? undefined}>
                  {field.label}
                </span>
                <span className="max-w-[360px] truncate text-right">
                  {attrText(field, environment.observed_attrs)}
                </span>
              </div>
            ))}
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Environment ID</span>
              <span
                className="font-mono text-xs text-muted-foreground"
                title="Internal stable identifier. Renaming the environment leaves this unchanged."
              >
                {environment.id}
              </span>
            </div>
            <div className="space-y-2">
              <span className="text-muted-foreground">Description</span>
              <p className="text-sm whitespace-pre-wrap rounded-md border p-3">
                {environment.description || 'No description'}
              </p>
            </div>
          </CardContent>
        </Card>

        <EnvironmentVariablesCard environmentName={environment.name} />
      </div>
    </div>
  );
}

function EnvironmentVariablesCard({ environmentName }: { environmentName: string }) {
  const { data, isLoading } = useEnvironmentVariables(environmentName);
  const update = useUpdateEnvironmentVariables(environmentName);

  return (
    <Card>
      <CardHeader className="space-y-1 py-3">
        <CardTitle className="text-sm">Environment Variables</CardTitle>
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
          emptyHint="Add a row above to ship a variable with every run on this environment."
        />
      </CardContent>
    </Card>
  );
}
