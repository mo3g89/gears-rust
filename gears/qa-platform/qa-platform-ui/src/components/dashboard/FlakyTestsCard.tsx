import { Link } from 'react-router-dom';
import { FlakyTestCard } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Zap } from 'lucide-react';
import { displayPlanName } from '@/lib/utils';

interface FlakyTestsCardProps {
  tests: FlakyTestCard[];
}

function passRatePct(passed: number, total: number): string {
  if (total === 0) return '—';
  return `${Math.round((passed / total) * 100)}%`;
}

function testShortName(testName: string, testFile: string | null): string {
  const base = testName.split('::').pop() || testName;
  const trimmed = displayPlanName(base);
  if (trimmed) return trimmed;
  if (testFile) return testFile.split('/').pop() || testFile;
  return testName;
}

export function FlakyTestsCard({ tests }: FlakyTestsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Zap className="h-4 w-4 text-amber-500" />
          Flaky Tests (7d)
        </CardTitle>
        <CardDescription>Tests that both passed and failed in the last 7 days</CardDescription>
      </CardHeader>
      <CardContent>
        {tests.length === 0 ? (
          <p className="py-4 text-sm text-muted-foreground">No flaky tests detected in the last 7 days.</p>
        ) : (
          <ul className="divide-y">
            {tests.map((test, idx) => {
              // The test catalog and its detail view have no backend here, so a
              // flaky test links to its plan (see REMOVED-SURFACES.md, Task 8a C1).
              const target = `/plans/${test.plan_id}`;
              return (
                <li key={`${test.plan_id}-${test.test_name}-${idx}`} className="flex items-center justify-between gap-3 py-2">
                  <div className="min-w-0 flex-1">
                    <Link
                      to={target}
                      className="block truncate text-sm text-foreground/80 hover:text-foreground hover:underline"
                      title={test.test_name}
                    >
                      {testShortName(test.test_name, test.test_file)}
                    </Link>
                    <div className="mt-0.5 text-xs text-muted-foreground">
                      <Link to={`/plans/${test.plan_id}`} className="hover:text-foreground hover:underline">
                        {test.plan_id}
                      </Link>
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-3 text-xs tabular-nums">
                    <span className="text-emerald-600 dark:text-emerald-400">{test.passed} ✓</span>
                    <span className="text-red-600 dark:text-red-400">{test.failed} ✗</span>
                    <span className="text-muted-foreground">{passRatePct(test.passed, test.total)}</span>
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
