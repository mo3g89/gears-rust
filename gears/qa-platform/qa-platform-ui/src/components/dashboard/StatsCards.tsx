import { FileText, PlayCircle, Clock, Calendar } from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';

interface StatsCardsProps {
  totalPlans: number;
  totalRuns: number;
  activeRuns: number;
  totalSchedules: number;
}

export function StatsCards({ totalPlans, totalRuns, activeRuns, totalSchedules }: StatsCardsProps) {
  const stats = [
    {
      title: 'Total Plans',
      value: totalPlans,
      icon: FileText,
      description: 'Test plan configurations',
    },
    {
      title: 'Total Runs',
      value: totalRuns,
      icon: PlayCircle,
      description: 'Workflow executions',
    },
    {
      title: 'Active Runs',
      value: activeRuns,
      icon: Clock,
      description: 'Currently running',
    },
    {
      title: 'Schedules',
      value: totalSchedules,
      icon: Calendar,
      description: 'Automated schedules',
    },
  ];

  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
      {stats.map((stat) => {
        const Icon = stat.icon;
        return (
          <Card key={stat.title}>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <CardTitle className="text-sm font-medium">{stat.title}</CardTitle>
              <Icon className="h-4 w-4 text-muted-foreground" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{stat.value}</div>
              <p className="text-xs text-muted-foreground">{stat.description}</p>
            </CardContent>
          </Card>
        );
      })}
    </div>
  );
}
