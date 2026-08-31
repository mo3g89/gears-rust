import { useState } from 'react';
import { toast } from 'sonner';
import { Clock, Loader2, PlayCircle, X } from 'lucide-react';
import { useCancelQueuedRun, useForceStartQueuedRun, useRunQueue } from '@/api/hooks';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import type { RunQueueEntry } from '@/api/types';

/** Poll at the same cadence as the Runs table's auto-refresh. */
const REFETCH_MS = 5000;

/** "1h 4m" / "12m" / "8s" — coarse, because queue waits are minutes to hours. */
function formatSeconds(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return `${seconds}s`;
}

function formatElapsed(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return '-';
  return formatSeconds((Date.now() - then) / 1000);
}

/**
 * TTL remaining. A null `ttl_expires_at` means expiry is switched off
 * server-side, which is shown as "off" rather than as a missing value.
 */
function formatRemaining(iso: string | null): string {
  if (!iso) return 'off';
  const deadline = new Date(iso).getTime();
  if (Number.isNaN(deadline)) return '-';
  const remaining = (deadline - Date.now()) / 1000;
  return remaining <= 0 ? 'due' : formatSeconds(remaining);
}

interface QueuedRunsCardProps {
  /** Restrict to one platform. Omit for every platform, and the column appears. */
  platform?: string;
}

export function QueuedRunsCard({ platform }: QueuedRunsCardProps) {
  const { data, isLoading } = useRunQueue(platform, REFETCH_MS);
  const cancel = useCancelQueuedRun();
  const forceStart = useForceStartQueuedRun();
  const confirm = useConfirm();
  const [busyId, setBusyId] = useState<string | null>(null);

  const queued: RunQueueEntry[] = (data || [])
    .filter((entry) => entry.state === 'queued')
    .sort((a, b) => a.enqueued_at.localeCompare(b.enqueued_at));

  // Nothing waiting: render nothing, so a page with an idle queue is unchanged.
  if (isLoading || queued.length === 0) {
    return null;
  }

  const handleCancel = async (entry: RunQueueEntry) => {
    const ok = await confirm({
      title: `Cancel queued run "${entry.target_id}"?`,
      description: 'It will be dropped from the queue and will never start.',
      confirmText: 'Cancel run',
      variant: 'destructive',
    });
    if (!ok) return;
    setBusyId(entry.id);
    cancel.mutate(entry.id, {
      onSuccess: () => {
        setBusyId(null);
        toast.success(`Dropped "${entry.target_id}" from the queue`);
      },
      onError: (error) => {
        setBusyId(null);
        toast.error('Failed to cancel the queued run', { description: String(error) });
      },
    });
  };

  const handleForceStart = async (entry: RunQueueEntry) => {
    const ok = await confirm({
      title: `Force start "${entry.target_id}" on ${entry.platform}?`,
      description:
        'This starts the run now, ignoring what is already running on the platform — ' +
        'including an exclusive run. The global max_concurrent_runs limit still applies.',
      confirmText: 'Force start',
      variant: 'destructive',
    });
    if (!ok) return;
    setBusyId(entry.id);
    forceStart.mutate(entry.id, {
      onSuccess: (result) => {
        setBusyId(null);
        toast.success(`Started ${result.workflow_name}`);
      },
      onError: (error) => {
        setBusyId(null);
        toast.error('Failed to force start the run', { description: String(error) });
      },
    });
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Clock className="h-4 w-4" />
          Queued
        </CardTitle>
        <CardDescription>
          {queued.length} run{queued.length !== 1 ? 's' : ''} waiting
          {platform ? ` on ${platform}` : ' across all platforms'}. Each starts automatically
          once its platform is free.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-12">#</TableHead>
                <TableHead>Target</TableHead>
                {!platform && <TableHead>Platform</TableHead>}
                <TableHead>Access</TableHead>
                <TableHead>Source</TableHead>
                <TableHead>Waiting for</TableHead>
                <TableHead>In queue</TableHead>
                <TableHead>TTL left</TableHead>
                <TableHead className="text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {queued.map((entry) => (
                <TableRow key={entry.id}>
                  <TableCell className="text-muted-foreground">
                    {entry.queue_position ?? '-'}
                  </TableCell>
                  <TableCell>
                    <div className="font-medium">{entry.target_id}</div>
                    <div className="text-xs text-muted-foreground">{entry.run_kind}</div>
                  </TableCell>
                  {!platform && <TableCell>{entry.platform}</TableCell>}
                  <TableCell>
                    <Badge variant={entry.exclusive ? 'default' : 'outline'}>
                      {entry.exclusive ? 'Exclusive' : 'Parallel'}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-muted-foreground">{entry.source}</TableCell>
                  <TableCell className="max-w-[280px] text-xs text-muted-foreground">
                    {entry.blocked_by ?? '-'}
                  </TableCell>
                  <TableCell title={entry.enqueued_at}>{formatElapsed(entry.enqueued_at)}</TableCell>
                  <TableCell title={entry.ttl_expires_at ?? 'expiry disabled'}>
                    {formatRemaining(entry.ttl_expires_at)}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex items-center justify-end gap-1">
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busyId === entry.id}
                        onClick={() => handleForceStart(entry)}
                        title="Start now, ignoring platform occupancy"
                      >
                        {busyId === entry.id ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <PlayCircle className="h-3.5 w-3.5" />
                        )}
                        <span className="ml-1">Force start</span>
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        disabled={busyId === entry.id}
                        onClick={() => handleCancel(entry)}
                        title="Drop from the queue"
                        aria-label="Cancel queued run"
                      >
                        <X className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </CardContent>
    </Card>
  );
}
