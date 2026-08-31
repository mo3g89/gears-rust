import { useQuery } from '@tanstack/react-query';
import { apiGet } from '@/api/client';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { UnavailableNotice } from '@/components/ui/unavailable';
import { Loader2 } from 'lucide-react';
import ReactApexChart from 'react-apexcharts';
import type { ApexOptions } from 'apexcharts';
import { chartThemeOptions, useChartTheme } from '@/lib/chart-theme';

interface CoverageBuild {
  build: string;
  coverage: {
    line_pct: number;
    branch_pct: number;
    function_pct: number;
  };
}

export function CoverageChart() {
  const chartTheme = useChartTheme();
  const { data, isLoading } = useQuery({
    queryKey: ['coverage'],
    queryFn: () => apiGet<CoverageBuild[]>('/dashboard/coverage'),
  });

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Code Coverage</CardTitle>
        </CardHeader>
        <CardContent className="flex justify-center py-8">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </CardContent>
      </Card>
    );
  }

  if (!data?.length) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Code Coverage</CardTitle>
          <CardDescription>Coverage for each product (latest reported version)</CardDescription>
        </CardHeader>
        <CardContent>
          <UnavailableNotice title="Code coverage is not available in this deployment">
            Nothing here measures a coverage point, and no number is derived from the
            ingested test results to stand in for one, so the coverage endpoint is
            empty by construction rather than because no run has reported yet. The
            chart returns as soon as a real measurement exists.
          </UnavailableNotice>
        </CardContent>
      </Card>
    );
  }

  const chartData = data.map((d) => ({
    build: d.build,
    'Line %': d.coverage.line_pct,
    'Branch %': d.coverage.branch_pct,
    'Function %': d.coverage.function_pct,
  }));

  return (
    <Card>
      <CardHeader>
        <CardTitle>Code Coverage</CardTitle>
        <CardDescription>All products, one point per latest reported version</CardDescription>
      </CardHeader>
      <CardContent>
        <ReactApexChart
          type="line"
          height={300}
          series={[
            { name: 'Line %', data: chartData.map((point) => point['Line %']) },
            { name: 'Branch %', data: chartData.map((point) => point['Branch %']) },
            { name: 'Function %', data: chartData.map((point) => point['Function %']) },
          ]}
          options={{
            chart: {
              type: 'line',
              toolbar: { show: false },
              animations: { easing: 'easeinout', speed: 500 },
            },
            colors: ['#3b82f6', '#10b981', '#f59e0b'],
            stroke: { curve: 'smooth', width: 3 },
            markers: { size: 4 },
            xaxis: {
              categories: chartData.map((point) => point.build),
            },
            yaxis: {
              min: 0,
              max: 100,
              labels: { formatter: (value) => `${Number(value).toFixed(0)}%` },
            },
            ...chartThemeOptions(chartTheme),
            legend: { position: 'top', horizontalAlign: 'left' },
            dataLabels: { enabled: false },
            tooltip: {
              y: {
                formatter: (value) => `${Number(value).toFixed(1)}%`,
              },
            },
          } as ApexOptions}
        />
      </CardContent>
    </Card>
  );
}
