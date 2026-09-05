import { useMemo } from 'react';
import { useProductCoverage } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { UnavailableNotice } from '@/components/ui/unavailable';
import { Loader2 } from 'lucide-react';
import ReactApexChart from 'react-apexcharts';
import type { ApexOptions } from 'apexcharts';
import { chartThemeOptions, useChartTheme } from '@/lib/chart-theme';

interface ProductCoverageCardProps {
  productId: string;
}

export function ProductCoverageCard({ productId }: ProductCoverageCardProps) {
  const chartTheme = useChartTheme();
  const { data, isLoading } = useProductCoverage(productId);

  const coveragePoints = useMemo(() => {
    return [...(data || [])].sort(
      (a, b) => new Date(a.collected_at).getTime() - new Date(b.collected_at).getTime()
    );
  }, [data]);

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Code Coverage</CardTitle>
          <CardDescription>Coverage per reported product version</CardDescription>
        </CardHeader>
        <CardContent className="flex justify-center py-8">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </CardContent>
      </Card>
    );
  }

  if (!coveragePoints.length) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Code Coverage</CardTitle>
          <CardDescription>Coverage per reported product version</CardDescription>
        </CardHeader>
        <CardContent>
          <UnavailableNotice title="Code coverage is not available in this deployment">
            Nothing here measures a coverage point, and no number is derived from the
            ingested test results to stand in for one, so this card is empty by
            construction rather than because no run has reported yet. The chart
            returns as soon as a real measurement exists.
          </UnavailableNotice>
        </CardContent>
      </Card>
    );
  }

  const series = [
    { name: 'Line %', data: coveragePoints.map((point) => point.coverage.line_pct) },
    { name: 'Branch %', data: coveragePoints.map((point) => point.coverage.branch_pct) },
    { name: 'Function %', data: coveragePoints.map((point) => point.coverage.function_pct) },
  ];

  const options: ApexOptions = {
    chart: {
      type: 'line',
      toolbar: { show: false },
      animations: { speed: 450 },
    },
    stroke: { curve: 'smooth', width: 3 },
    colors: ['#2563eb', '#0f766e', '#c2410c'],
    markers: { size: 4 },
    xaxis: {
      categories: coveragePoints.map((point) => point.version),
      title: { text: 'Product Version' },
    },
    yaxis: {
      min: 0,
      max: 100,
      labels: {
        formatter: (value) => `${Number(value).toFixed(0)}%`,
      },
    },
    legend: { position: 'top', horizontalAlign: 'left' },
    dataLabels: { enabled: false },
    ...chartThemeOptions(chartTheme),
    tooltip: {
      y: {
        formatter: (value) => `${Number(value).toFixed(1)}%`,
      },
    },
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>Code Coverage</CardTitle>
        <CardDescription>
          Coverage belongs to this product and is calculated per reported product version.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ReactApexChart type="line" height={320} series={series} options={options} />
        <div className="rounded-md border">
          <div className="grid grid-cols-[160px_1fr_220px] gap-3 border-b px-3 py-2 text-xs font-medium text-muted-foreground">
            <span>Version</span>
            <span>Run</span>
            <span>Collected</span>
          </div>
          {coveragePoints.map((point) => (
            <div
              key={`${point.version}-${point.run_name}`}
              className="grid grid-cols-[160px_1fr_220px] gap-3 border-b px-3 py-2 text-sm last:border-b-0"
            >
              <span className="font-medium">{point.version}</span>
              <span className="truncate text-muted-foreground">{point.run_name}</span>
              <span className="text-muted-foreground">{new Date(point.collected_at).toLocaleString()}</span>
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}
