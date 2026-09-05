import { useState } from 'react';
import { useDashboard } from '@/api/hooks';
import { KpiStrip } from '@/components/dashboard/KpiStrip';
import { ActiveRunsCard } from '@/components/dashboard/ActiveRunsCard';
import { RecentFailuresCard } from '@/components/dashboard/RecentFailuresCard';
import { FlakyTestsCard } from '@/components/dashboard/FlakyTestsCard';
import { QualityVectorsCard } from '@/components/dashboard/QualityVectorsCard';
import { PassRateChart } from '@/components/dashboard/PassRateChart';
import { EnvironmentsStrip } from '@/components/dashboard/EnvironmentsStrip';
import { Loader2, LayoutDashboard } from 'lucide-react';

export function DashboardPage() {
  const [statusTrendDays, setStatusTrendDays] = useState(7);
  const { data, isLoading, error } = useDashboard(statusTrendDays);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="text-center py-8">
        <p className="text-destructive">Failed to load dashboard data</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  if (!data) {
    return null;
  }

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-xl font-semibold flex items-center gap-2">
          <LayoutDashboard className="h-5 w-5" />
          Dashboard
        </h1>
        <p className="text-sm text-muted-foreground">Live view of runs and failures</p>
      </div>

      <KpiStrip
        activeRuns={data.active_runs}
        failed24h={data.failed_24h_count}
        failedPrev24h={data.failed_prev_24h_count}
        passRate24h={data.pass_rate_24h}
        passRatePrev24h={data.pass_rate_prev_24h}
        flakyCount={data.flaky_tests.length}
      />

      <div className="grid gap-4 lg:grid-cols-2">
        <ActiveRunsCard runs={data.active_runs_list} />
        <RecentFailuresCard failures={data.failed_recent} />
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <FlakyTestsCard tests={data.flaky_tests} />
        <QualityVectorsCard vectors={data.quality_vectors_pass_rate} />
      </div>

      <PassRateChart
        trend={data.daily_test_status_trend || []}
        days={statusTrendDays}
        onDaysChange={setStatusTrendDays}
      />

      <EnvironmentsStrip />
    </div>
  );
}
