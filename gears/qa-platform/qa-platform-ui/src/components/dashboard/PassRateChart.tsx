import { DashboardDailyStatusPoint } from '@/api/types';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import ReactApexChart from 'react-apexcharts';
import type { ApexOptions } from 'apexcharts';
import { chartThemeOptions, useChartTheme } from '@/lib/chart-theme';

interface PassRateChartProps {
  trend: DashboardDailyStatusPoint[];
  days: number;
  onDaysChange: (days: number) => void;
}

const axisDateFormatter = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: '2-digit',
});

export function PassRateChart({ trend, days, onDaysChange }: PassRateChartProps) {
  const chartTheme = useChartTheme();
  const chartData = [...trend]
    .sort((left, right) => left.day.localeCompare(right.day))
    .map((point) => {
      const date = new Date(`${point.day}T00:00:00Z`);
      return {
        day: point.day,
        label: Number.isNaN(date.getTime()) ? point.day : axisDateFormatter.format(date),
        passed: point.passed,
        failed: point.failed,
      };
    });

  if (chartData.length === 0) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Run Tests By Status</CardTitle>
          <CardDescription>Daily passed/failed test counts</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="h-[300px] flex items-center justify-center text-muted-foreground">
            No data available
          </div>
        </CardContent>
      </Card>
    );
  }

  return (
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between gap-3">
            <div>
              <CardTitle>Run Tests By Status</CardTitle>
              <CardDescription>X: days, Y: number of run tests (Passed/Failed)</CardDescription>
            </div>
            <div className="w-[140px]">
              <Select value={String(days)} onValueChange={(value) => onDaysChange(Number(value))}>
                <SelectTrigger>
                  <SelectValue placeholder="Days" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="3">Last 3 days</SelectItem>
                  <SelectItem value="7">Last 7 days</SelectItem>
                  <SelectItem value="14">Last 14 days</SelectItem>
                  <SelectItem value="30">Last 30 days</SelectItem>
                  <SelectItem value="90">Last 90 days</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <ReactApexChart
            type="line"
            height={300}
            series={[
              { name: 'Passed', data: chartData.map((point) => point.passed) },
              { name: 'Failed', data: chartData.map((point) => point.failed) },
            ]}
            options={{
              chart: {
                type: 'line',
                toolbar: { show: false },
                animations: { easing: 'easeinout', speed: 500 },
              },
              colors: ['#16a34a', '#dc2626'],
              stroke: { curve: 'smooth', width: 3 },
              markers: { size: 4 },
              xaxis: {
                categories: chartData.map((point) => point.label),
                title: { text: 'Days' },
              },
              yaxis: {
                min: 0,
                forceNiceScale: true,
                title: { text: 'Run tests' },
              },
              ...chartThemeOptions(chartTheme),
              legend: { position: 'top', horizontalAlign: 'left' },
              dataLabels: { enabled: false },
              tooltip: {
                custom: ({ dataPointIndex }) => {
                  const point = chartData[dataPointIndex];
                  if (!point) {
                    return '';
                  }
                  // The container carries its own colours: ApexCharts wraps custom
                  // tooltip HTML in a `.apexcharts-theme-*` class that sets a background
                  // but no `color`, so uncoloured markup inherits the page foreground and
                  // goes invisible in one of the two themes. See `lib/chart-theme.ts`.
                  return `<div style="padding:8px 10px;${chartTheme.tooltipStyle}">
                    <div style="font-weight:600; margin-bottom:4px;">${point.day}</div>
                    <div>Passed: ${point.passed}</div>
                    <div>Failed: ${point.failed}</div>
                  </div>`;
                },
              },
            } as ApexOptions}
          />
        </CardContent>
      </Card>
  );
}
