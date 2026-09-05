import { AnalyticsDashboard } from '@/components/analytics/AnalyticsDashboard';

export function AnalyticsPage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-semibold">Analytics</h1>
        <p className="text-muted-foreground">Product and version analytics overview</p>
      </div>
      <AnalyticsDashboard scope="all" />
    </div>
  );
}
