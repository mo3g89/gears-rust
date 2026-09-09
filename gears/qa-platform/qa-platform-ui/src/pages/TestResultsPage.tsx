import { Link, useParams } from 'react-router-dom';
import { ArrowLeft } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { AnalyticsDashboard } from '@/components/analytics/AnalyticsDashboard';

export function TestResultsPage() {
  const { planId } = useParams<{ planId: string }>();

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Link to={planId ? `/plans/${planId}` : '/plans'}>
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div>
          <h1 className="text-xl font-semibold">Test Results Analytics</h1>
          <p className="text-muted-foreground">Plan: {planId || '-'}</p>
        </div>
      </div>

      <AnalyticsDashboard scope="plan" planId={planId} />
    </div>
  );
}
