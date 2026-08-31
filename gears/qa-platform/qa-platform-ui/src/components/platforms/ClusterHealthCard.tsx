import type { ClusterHealth } from '@/api/types';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { UnavailableNotice } from '@/components/ui/unavailable';
import { clusterDotClass, clusterStatusMessage } from '@/lib/platform-observation';
import { cn } from '@/lib/utils';

function formatCheckedAt(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border bg-muted/30 p-2">
      <p className="text-[11px] uppercase tracking-wide text-muted-foreground">{label}</p>
      <p className="text-sm font-medium tabular-nums">{value}</p>
    </div>
  );
}

/**
 * The three states this card keeps distinguishable — this is the point of the card, not
 * an implementation detail:
 *
 *  - `cluster === null`: **never checked.** No detection cycle has reached this platform
 *    yet. This is a different sentence from the `UnavailableNotice` this card replaces,
 *    which said there was no source for this data *at all* — there is a source now, it
 *    has simply not reported on this platform.
 *  - `cluster.status === 'Unreachable'`: **checked, and the cluster could not be read.**
 *    `nodes` is `[]` and every field in `counts` is `0`, but those zeros are an artefact
 *    of an empty list, not a measurement — rendering "0/0 nodes Ready" beside
 *    "Unreachable" would state a measurement that was never taken, so the counts and the
 *    node table are suppressed and the status message (the server's own classified
 *    failure text) is shown instead.
 *  - Any other status: read successfully. Counts, the per-node table and the namespace
 *    count render as given — `namespace_count === null` means "could not be read" and
 *    renders as "not read", never as `0` (a `0` there would be a different, false claim:
 *    "the cluster genuinely has no namespaces").
 *
 * Counts are rendered, never re-derived: `NodeCountsDto`'s own doc says the UI must not
 * recompute them from `nodes`, so this component only reads `cluster.counts`.
 */
export function ClusterHealthCard({ cluster }: { cluster: ClusterHealth | null }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Cluster Health</CardTitle>
        <CardDescription>
          Node, control-plane and worker readiness read from this platform&apos;s own cluster.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {cluster === null ? (
          <UnavailableNotice title="No cluster health check has run yet">
            No detection cycle has reported on this platform&apos;s cluster. That is
            different from cluster health having no source: the next detection cycle will
            populate this card.
          </UnavailableNotice>
        ) : (
          <ClusterHealthBody cluster={cluster} />
        )}
      </CardContent>
    </Card>
  );
}

function ClusterHealthBody({ cluster }: { cluster: ClusterHealth }) {
  const message = clusterStatusMessage(cluster);
  const unreachable = cluster.status === 'Unreachable';

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <span
          className={cn('h-2.5 w-2.5 shrink-0 rounded-full', clusterDotClass(cluster.status))}
          aria-hidden
        />
        <Badge variant="outline">{cluster.status}</Badge>
        {message && <span className="text-sm text-muted-foreground">{message}</span>}
      </div>

      {unreachable ? (
        // `counts` on an Unreachable reading is all zeros because `nodes` came back
        // empty, not because the cluster has no nodes — showing "0/0 nodes Ready" here
        // would state a measurement that was never taken (spec rule 1).
        <p className="text-xs text-muted-foreground">
          Node and namespace counts are not shown: the last check could not reach the
          cluster, so nothing was measured.
        </p>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Stat label="Nodes" value={`${cluster.counts.ready}/${cluster.counts.total} Ready`} />
            <Stat
              label="Control plane"
              value={`${cluster.counts.ready_control_plane}/${cluster.counts.control_plane} Ready`}
            />
            <Stat
              label="Workers"
              value={`${cluster.counts.ready_worker}/${cluster.counts.worker} Ready`}
            />
            <Stat
              label="Namespaces"
              // `null` means "could not be read", never zero (spec rule 3) — legacy's
              // `unwrap_or(0)` conflated the two and this design deliberately does not.
              value={cluster.namespace_count === null ? 'not read' : String(cluster.namespace_count)}
            />
          </div>

          {cluster.nodes.length > 0 && (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Role</TableHead>
                  <TableHead>Ready</TableHead>
                  <TableHead>Kubelet</TableHead>
                  <TableHead>OS Image</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {cluster.nodes.map((node) => (
                  <TableRow key={node.name}>
                    <TableCell className="font-mono text-xs">{node.name}</TableCell>
                    <TableCell>{node.control_plane ? 'Control plane' : 'Worker'}</TableCell>
                    <TableCell>
                      <span className="inline-flex items-center gap-1.5">
                        <span
                          className={cn(
                            'h-2 w-2 rounded-full',
                            node.ready ? 'bg-emerald-500' : 'bg-red-500'
                          )}
                          aria-hidden
                        />
                        {node.ready ? 'Ready' : 'Not Ready'}
                      </span>
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {node.kubelet_version || '—'}
                    </TableCell>
                    <TableCell className="text-muted-foreground">{node.os_image || '—'}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </>
      )}

      <p className="text-xs text-muted-foreground">
        Checked {formatCheckedAt(cluster.checked_at)}
      </p>
    </div>
  );
}
