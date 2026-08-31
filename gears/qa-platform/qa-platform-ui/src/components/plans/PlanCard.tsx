import { Link } from 'react-router-dom';
import { useState } from 'react';
import { TestPlanInfo } from '@/api/types';
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { CreateScheduleDialog } from '@/components/schedules/CreateScheduleDialog';
import { RunPlanDialog } from './RunPlanDialog';
import { PlayCircle, FileCode, Calendar } from 'lucide-react';

interface PlanCardProps {
  plan: TestPlanInfo;
}

export function PlanCard({ plan }: PlanCardProps) {
  const [runDialogOpen, setRunDialogOpen] = useState(false);

  return (
    <Card className="hover:shadow-lg transition-shadow">
      <CardHeader>
        <div className="flex items-start justify-between">
          <div className="flex-1">
            <CardTitle>
              <Link to={`/plans/${plan.id}`} className="hover:underline">
                {plan.plan.name}
              </Link>
            </CardTitle>
            <CardDescription className="mt-1">
              {plan.plan.description}
            </CardDescription>
          </div>
        </div>
        <div className="mt-2">
          {plan.source === 'repo' && plan.repo_name ? (
            <Badge variant="outline" className="text-xs">
              Repo: {plan.repo_name}
            </Badge>
          ) : (
            <Badge variant="outline" className="text-xs text-muted-foreground">
              Local
            </Badge>
          )}
        </div>
      </CardHeader>
      
      <CardContent className="space-y-3">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <FileCode className="h-4 w-4" />
          <span>{plan.test_files.length} test file{plan.test_files.length !== 1 ? 's' : ''}</span>
        </div>

        {plan.plan.tags.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {plan.plan.tags.map((tag) => (
              <Badge key={tag} variant="secondary" className="text-xs">
                {tag}
              </Badge>
            ))}
          </div>
        )}
      </CardContent>

      <CardFooter className="flex gap-2">
        <Button 
          onClick={() => setRunDialogOpen(true)}
          className="flex-1"
        >
          <PlayCircle className="mr-2 h-4 w-4" />
          Run
        </Button>
        <CreateScheduleDialog 
          initialPlanId={plan.id}
          trigger={
            <Button
              variant="outline"
              className="flex-1"
            >
              <Calendar className="mr-2 h-4 w-4" />
              Schedule
            </Button>
          }
        />
      </CardFooter>
      
      <RunPlanDialog
        plan={plan}
        open={runDialogOpen}
        onOpenChange={setRunDialogOpen}
      />
    </Card>
  );
}
