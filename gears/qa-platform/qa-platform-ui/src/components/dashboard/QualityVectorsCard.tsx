import { Link } from 'react-router-dom';
import { QualityVectorPassRate } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Diamond } from 'lucide-react';
import { cn } from '@/lib/utils';

interface QualityVectorsCardProps {
  vectors: QualityVectorPassRate[];
}

function rateTone(rate: number): string {
  if (rate >= 0.95) return 'text-emerald-600 dark:text-emerald-400';
  if (rate >= 0.8) return 'text-amber-600 dark:text-amber-400';
  return 'text-red-600 dark:text-red-400';
}

export function QualityVectorsCard({ vectors }: QualityVectorsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Diamond className="h-4 w-4 text-indigo-500" />
          Quality Vectors (7d)
        </CardTitle>
        <CardDescription>
          Pass-rate per quality-vector tag from <code className="text-[10px]">TEST_META</code>
        </CardDescription>
      </CardHeader>
      <CardContent>
        {vectors.length === 0 ? (
          <p className="py-4 text-sm text-muted-foreground">
            No quality-vector data in the last 7 days.{' '}
            <Link to="/analytics" className="hover:text-foreground hover:underline">
              Open Analytics →
            </Link>
          </p>
        ) : (
          <ul className="divide-y">
            {vectors.map((v) => {
              const rate = v.total > 0 ? v.passed / v.total : 0;
              const pct = v.total > 0 ? `${Math.round(rate * 100)}%` : '—';
              return (
                <li key={v.vector} className="flex items-center justify-between gap-3 py-2">
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm text-foreground/80" title={v.vector}>
                      {v.vector}
                    </div>
                    <div className="mt-0.5 text-xs text-muted-foreground">
                      {v.tests} test{v.tests === 1 ? '' : 's'} · {v.total} run{v.total === 1 ? '' : 's'}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-3 text-xs tabular-nums">
                    <span className="text-emerald-600 dark:text-emerald-400">{v.passed} ✓</span>
                    <span className="text-red-600 dark:text-red-400">{v.failed} ✗</span>
                    <span className={cn('font-medium', rateTone(rate))}>{pct}</span>
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
