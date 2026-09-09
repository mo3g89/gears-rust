import { Link } from 'react-router-dom';
import { WorkflowRun } from '@/api/types';
import { RunPhaseBadge } from '@/components/runs/RunPhaseBadge';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';

interface RecentRunsTableProps {
  runs: WorkflowRun[];
}

export function RecentRunsTable({ runs }: RecentRunsTableProps) {
  if (runs.length === 0) {
    return (
      <div className="text-center py-8 text-muted-foreground">
        No recent runs found
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Plan</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Started</TableHead>
          <TableHead>Duration</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {runs.map((run) => (
          <TableRow key={run.name}>
            <TableCell>
              <Link
                to={`/runs/${run.name}`}
                className="text-foreground/80 hover:text-foreground hover:underline"
              >
                {run.name}
              </Link>
            </TableCell>
            <TableCell>
              <Link
                to={`/plans/${run.plan_id}`}
                className="text-muted-foreground hover:text-foreground hover:underline"
              >
                {run.plan_id}
              </Link>
            </TableCell>
            <TableCell>
              <RunPhaseBadge phase={run.phase} />
            </TableCell>
            <TableCell className="text-muted-foreground">
              {run.started_at ? new Date(run.started_at).toLocaleString() : '—'}
            </TableCell>
            <TableCell className="text-muted-foreground">
              {run.duration || '—'}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
