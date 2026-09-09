import { Link } from 'react-router-dom';
import { FailedTestCard } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { AlertTriangle } from 'lucide-react';
import { displayPlanName } from '@/lib/utils';

interface RecentFailuresCardProps {
  failures: FailedTestCard[];
}

function relativeTime(value: string | null): string {
  if (!value) return '—';
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return '—';
  const diff = Date.now() - d.getTime();
  const m = Math.floor(diff / 60000);
  if (m < 1) return 'just now';
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const days = Math.floor(h / 24);
  return `${days}d ago`;
}

function testShortName(testName: string, testFile: string | null): string {
  const base = testName.split('::').pop() || testName;
  const trimmed = displayPlanName(base);
  if (trimmed) return trimmed;
  if (testFile) return testFile.split('/').pop() || testFile;
  return testName;
}

export function RecentFailuresCard({ failures }: RecentFailuresCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <AlertTriangle className="h-4 w-4 text-red-500" />
          Recent Failures (24h)
          <Badge variant="secondary" className="ml-1 px-1.5 py-0 text-xs tabular-nums">{failures.length}</Badge>
        </CardTitle>
        <CardDescription>FAILED / ERROR test results in the last 24 hours</CardDescription>
      </CardHeader>
      <CardContent>
        {failures.length === 0 ? (
          <p className="py-4 text-sm text-muted-foreground">No failures in the last 24h. 🎉</p>
        ) : (
          <ul className="divide-y">
            {failures.map((failure, idx) => {
              return (
                <li
                  key={`${failure.workflow_name}-${failure.test_name}-${idx}`}
                  className="flex items-center justify-between gap-3 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <Link
                      to={`/runs/${failure.workflow_name}`}
                      className="block truncate text-sm text-foreground/80 hover:text-foreground hover:underline"
                      title={failure.test_name}
                    >
                      {testShortName(failure.test_name, failure.test_file)}
                    </Link>
                    <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
                      <Link to={`/plans/${failure.plan_id}`} className="hover:text-foreground hover:underline">
                        {failure.plan_id}
                      </Link>
                      {failure.platform && <span>· {failure.platform}</span>}
                      <span>· {relativeTime(failure.finished_at)}</span>
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {failure.jira_key && (
                      <Badge variant="outline" className="text-[10px] uppercase tracking-wide">
                        {failure.jira_key}
                      </Badge>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
