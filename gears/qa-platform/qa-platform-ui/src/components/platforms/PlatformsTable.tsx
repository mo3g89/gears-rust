import { Link } from 'react-router-dom';
import { useProducts } from '@/api/hooks';
import { PlatformInfo } from '@/api/types';
import { Button } from '@/components/ui/button';
import { EditPlatformDialog } from '@/components/platforms/EditPlatformDialog';
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
import { clusterStatusMessage, platformDotClass, platformObservation } from '@/lib/platform-observation';

interface PlatformsTableProps {
  platforms: PlatformInfo[];
  onDelete?: (name: string) => void;
  isDeleting?: boolean;
}

/**
 * The "Status" column is back. It was removed because `version_detect_error` and
 * `version_detected_at` were columns nothing in this deployment ever set, so every row
 * read a permanent "Unknown" — a "not yet" that was really a "never" (REMOVED-SURFACES.md,
 * Task 8a, C3). The platform-observation cycle now writes both
 * (`qa-environments/src/infra/storage/platforms_sea_repo.rs:290-364`), so the column
 * reports a real measurement.
 *
 * It reported **detection**, not legacy's node health, until Task 5 gave qa-environments a
 * real cluster-health reading: the column now shows `platform.cluster`'s status where a
 * cycle has reached the platform, falling back to the reachability/detection label
 * (`platformObservation`) for the platform a cycle has not reached yet — the same
 * cluster-then-reachability fallback `platformDotClass` uses for the dot (D-CH-6), so the
 * dot and the text never disagree.
 *
 * A "Detection failed" row still shows a Version: a failed attempt deliberately leaves
 * the previously observed values in place, so that version is the last one confirmed, not
 * the current one — which is exactly why the status column has to be visible next to it.
 */
export function PlatformsTable({ platforms, onDelete, isDeleting }: PlatformsTableProps) {
  const { data: products } = useProducts();
  const productById = new Map((products || []).map((product) => [product.id, product]));

  if (platforms.length === 0) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        No platforms configured
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Product</TableHead>
          <TableHead>Version</TableHead>
          <TableHead>Namespace</TableHead>
          <TableHead>VHP URL</TableHead>
          <TableHead>Created</TableHead>
          <TableHead className="w-[140px] text-right">Actions</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {platforms.map((platform) => {
          const product = platform.product_id ? productById.get(platform.product_id) : undefined;
          const productLabel = product
            ? `${product.key}`
            : platform.product_id || '';
          const productTooltip = product
            ? `${product.name} (${product.key})`
            : platform.product_id || undefined;
          const observation = platformObservation(platform);
          // Cluster status once a cycle has reached this platform; the existing
          // detection/reachability label otherwise (spec: "falling back to the existing
          // observation label"). Same fallback `platformDotClass` uses for the dot.
          const statusText = platform.cluster ? platform.cluster.status : observation.label;
          const statusTitle = platform.cluster
            ? (clusterStatusMessage(platform.cluster) ?? platform.cluster.status)
            : observation.detail;
          const statusIsBad =
            platform.cluster
              ? platform.cluster.status === 'Unhealthy' || platform.cluster.status === 'Unreachable'
              : observation.state === 'failed';
          return (
          <TableRow key={platform.name}>
            <TableCell>
              <span className="flex items-center gap-2">
                <Link
                  to={`/platforms/${encodeURIComponent(platform.name)}`}
                  className="text-foreground/80 hover:text-foreground hover:underline"
                  title={platform.description || undefined}
                >
                  {platform.name}
                </Link>
                {/*
                  Beside the name rather than in its own column: the flag is a property of
                  one row, not a value every row carries, and an extra column would print
                  an em dash on every non-default platform to say nothing. The `title`
                  spells out what "Default" actually does, since the word alone could be
                  read as "default branch" or "default product".
                */}
                {platform.is_default && (
                  <span
                    className="rounded border border-primary/40 bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-primary"
                    title={'The "Default cluster" option in the Run Plan and Schedule dialogs resolves to this platform for its product'}
                  >
                    Default
                  </span>
                )}
              </span>
            </TableCell>
            <TableCell>
              <span
                className="inline-flex items-center gap-1.5 text-xs whitespace-nowrap"
                title={statusTitle}
              >
                <span
                  className={cn('h-2 w-2 shrink-0 rounded-full', platformDotClass(platform))}
                  aria-hidden
                />
                <span
                  className={statusIsBad ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'}
                >
                  {statusText}
                </span>
              </span>
            </TableCell>
            <TableCell>
              {platform.product_id ? (
                <span title={productTooltip}>{productLabel}</span>
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </TableCell>
            <TableCell className="tabular-nums">
              {platform.version ? (
                platform.build ? `${platform.version}.${platform.build}` : platform.version
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </TableCell>
            <TableCell className="text-muted-foreground">
              {platform.namespace || '—'}
            </TableCell>
            <TableCell className="max-w-[260px] truncate text-muted-foreground" title={platform.vhp_base_url || undefined}>
              {platform.vhp_base_url || '—'}
            </TableCell>
            <TableCell className="text-muted-foreground">
              {platform.created_at
                ? new Date(platform.created_at).toLocaleDateString()
                : '—'}
            </TableCell>
            <TableCell>
              <div className="flex items-center justify-end gap-1">
                <Link to={`/platforms/${encodeURIComponent(platform.name)}`}>
                  <Button variant="ghost" size="icon" title="View platform">
                    <Eye className="h-4 w-4" />
                  </Button>
                </Link>
                <EditPlatformDialog
                  platform={platform}
                  trigger={
                    <Button variant="ghost" size="icon" title="Edit platform">
                      <Pencil className="h-4 w-4" />
                    </Button>
                  }
                />
                {onDelete && (
                  <Button
                    variant="ghost"
                    size="icon"
                    title="Delete platform"
                    onClick={() => onDelete(platform.name)}
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
