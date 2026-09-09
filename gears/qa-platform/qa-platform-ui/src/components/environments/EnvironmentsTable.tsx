import { Link } from 'react-router-dom';
import { useProducts } from '@/api/hooks';
import { EnvironmentInfo } from '@/api/types';
import { Button } from '@/components/ui/button';
import { EditEnvironmentDialog } from '@/components/environments/EditEnvironmentDialog';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { Eye, Pencil, Trash2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useProductPlugins } from '@/api/productPlugins';
import { attrText, pluginForProduct, tableColumns } from '@/lib/fieldDesc';
import { healthDotClass, healthLabel } from '@/lib/environment-observation';

interface EnvironmentsTableProps {
  environments: EnvironmentInfo[];
  onDelete?: (name: string) => void;
  isDeleting?: boolean;
}

/**
 * The "Status" column is back. It was removed because `version_detect_error` and
 * `version_detected_at` were columns nothing in this deployment ever set, so every row
 * read a permanent "Unknown" — a "not yet" that was really a "never" (REMOVED-SURFACES.md,
 * Task 8a, C3). The environment-observation cycle now writes both
 * (`qa-environments/src/infra/storage/environments_sea_repo.rs`' `record_observation`), so the column
 * reports a real measurement.
 *
 * It is the **plugin's health verdict** since Task 19: `health_state` and its classified
 * `health_detail`, which replaced the five `cluster_*` columns. `healthLabel` falls back to
 * the reachability/detection label for an environment whose verdict is still `unknown`,
 * and `healthDotClass` uses the same fallback, so the dot and the text never disagree.
 *
 * A "Detection failed" row still shows a Version: a failed attempt deliberately leaves
 * the previously observed values in place, so that version is the last one confirmed, not
 * the current one — which is exactly why the status column has to be visible next to it.
 */
export function EnvironmentsTable({ environments, onDelete, isDeleting }: EnvironmentsTableProps) {
  const { data: products } = useProducts();
  const { data: productPlugins } = useProductPlugins();
  const productById = new Map((products || []).map((product) => [product.id, product]));

  // **The descriptor columns are the product's, not the table's.**
  //
  // A table renders one column set, so it takes the schema of the FIRST
  // product it finds rows for. That is not a compromise in the deployment this
  // ships to -- every environment in a list belongs to the selected product,
  // because the page is scoped to one -- and it is stated here rather than
  // assumed, since a mixed list would render the second product's attributes
  // under the first product's headings. `RequireProduct` wraps the route.
  const firstProductId = environments.find((p) => p.product_id)?.product_id ?? null;
  const observedSchema =
    pluginForProduct(
      productPlugins ?? [],
      firstProductId ? productById.get(firstProductId)?.plugin_instance_id : null,
    )?.observed_schema ?? [];
  const columns = tableColumns(observedSchema);

  if (environments.length === 0) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        No environments configured
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          {/*
            FIXED LEADING COLUMNS, then one per `in_table` descriptor.
            "Namespace" and "VHP URL" stood here as fixed headings -- two of
            VHP's attributes, hardcoded into a table every product shares.
            VHP's own table is unchanged afterwards, because its plugin
            declares `namespace` and `baseDomain` with `in_table: true`.
          */}
          <TableHead>Name</TableHead>
          <TableHead>Health</TableHead>
          <TableHead>Product</TableHead>
          <TableHead>Available</TableHead>
          {columns.map((field) => (
            <TableHead key={field.key} title={field.help ?? undefined}>
              {field.label}
            </TableHead>
          ))}
          <TableHead>Created</TableHead>
          <TableHead className="w-[140px] text-right">Actions</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {environments.map((environment) => {
          const product = environment.product_id ? productById.get(environment.product_id) : undefined;
          const productLabel = product
            ? `${product.key}`
            : environment.product_id || '';
          const productTooltip = product
            ? `${product.name} (${product.key})`
            : environment.product_id || undefined;
          const health = healthLabel(environment);
          return (
          <TableRow key={environment.name}>
            <TableCell>
              <span className="flex items-center gap-2">
                <Link
                  to={`/environments/${encodeURIComponent(environment.name)}`}
                  className="text-foreground/80 hover:text-foreground hover:underline"
                  title={environment.description || undefined}
                >
                  {environment.name}
                </Link>
                {/*
                  Beside the name rather than in its own column: the flag is a property of
                  one row, not a value every row carries, and an extra column would print
                  an em dash on every non-default environment to say nothing. The `title`
                  spells out what "Default" actually does, since the word alone could be
                  read as "default branch" or "default product".
                */}
                {environment.is_default && (
                  <span
                    className="rounded border border-primary/40 bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary"
                    title={'The "Default cluster" option in the Run Plan and Schedule dialogs resolves to this environment for its product'}
                  >
                    Default
                  </span>
                )}
              </span>
            </TableCell>
            <TableCell>
              <span
                className="inline-flex items-center gap-1.5 text-xs whitespace-nowrap"
                title={health.title}
              >
                <span
                  className={cn('h-2 w-2 shrink-0 rounded-full', healthDotClass(environment))}
                  aria-hidden
                />
                <span
                  className={health.bad ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'}
                >
                  {health.text}
                </span>
              </span>
            </TableCell>
            <TableCell>
              {environment.product_id ? (
                <span title={productTooltip}>{productLabel}</span>
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </TableCell>
            <TableCell className="text-muted-foreground">
              {environment.available ? 'Yes' : 'No'}
            </TableCell>
            {columns.map((field) => {
              const value = attrText(field, environment.observed_attrs);
              return (
                <TableCell
                  key={field.key}
                  className="max-w-[260px] truncate text-muted-foreground"
                  title={value === '—' ? undefined : value}
                >
                  {value}
                </TableCell>
              );
            })}
            <TableCell className="text-muted-foreground">
              {environment.created_at
                ? new Date(environment.created_at).toLocaleDateString()
                : '—'}
            </TableCell>
            <TableCell>
              <div className="flex items-center justify-end gap-1">
                <Link to={`/environments/${encodeURIComponent(environment.name)}`}>
                  <Button variant="ghost" size="icon" title="View environment">
                    <Eye className="h-4 w-4" />
                  </Button>
                </Link>
                <EditEnvironmentDialog
                  environment={environment}
                  trigger={
                    <Button variant="ghost" size="icon" title="Edit environment">
                      <Pencil className="h-4 w-4" />
                    </Button>
                  }
                />
                {onDelete && (
                  <Button
                    variant="ghost"
                    size="icon"
                    title="Delete environment"
                    onClick={() => onDelete(environment.name)}
                    disabled={isDeleting}
                  >
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                )}
              </div>
            </TableCell>
          </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
