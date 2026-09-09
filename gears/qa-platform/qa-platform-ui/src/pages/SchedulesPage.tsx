import { useMemo, useState } from 'react';
import { toast } from 'sonner';
import {
  useSchedules,
  useSuspendSchedule,
  useResumeSchedule,
  useDeleteSchedule,
  useSetScheduleNotifications,
  usePlans,
  useRunPlan,
  useRunCustomPlan,
  useRunSingleTest,
  isQueued,
  LaunchResponse,
} from '@/api/hooks';
import { ScheduleInfo } from '@/api/types';
import { SchedulesTable } from '@/components/schedules/SchedulesTable';
import { CreateScheduleDialog } from '@/components/schedules/CreateScheduleDialog';
import { EditScheduleSlackDialog } from '@/components/schedules/EditScheduleSlackDialog';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { useConfirm } from '@/components/ui/confirm-dialog';
import { Loader2 } from 'lucide-react';

export function SchedulesPage() {
  const { data: schedules, isLoading, error } = useSchedules();
  const { data: plans } = usePlans();
  const suspendSchedule = useSuspendSchedule();
  const resumeSchedule = useResumeSchedule();
  const deleteSchedule = useDeleteSchedule();
  const setScheduleNotifications = useSetScheduleNotifications();
  const runPlan = useRunPlan();
  const runCustomPlan = useRunCustomPlan();
  const runSingleTest = useRunSingleTest();
  const [editingSlackSchedule, setEditingSlackSchedule] = useState<ScheduleInfo | null>(null);
  const [editingSchedule, setEditingSchedule] = useState<ScheduleInfo | null>(null);
  const confirm = useConfirm();

  const validationPlanIds = useMemo(() => {
    const ids = new Set<string>();
    (plans || []).forEach((plan) => {
      if (plan.plan.validation) {
        ids.add(plan.id);
      }
    });
    return ids;
  }, [plans]);

  const sortedSchedules = useMemo(
    () =>
      (schedules || [])
        .map((schedule, index) => ({ schedule, index }))
        .sort((a, b) => {
          const aValidation = validationPlanIds.has(a.schedule.plan_id);
          const bValidation = validationPlanIds.has(b.schedule.plan_id);
          return Number(bValidation) - Number(aValidation) || a.index - b.index;
        })
        .map(({ schedule }) => schedule),
    [schedules, validationPlanIds]
  );

  const handleSuspend = (name: string) => {
    suspendSchedule.mutate(name);
  };

  const handleResume = (name: string) => {
    resumeSchedule.mutate(name);
  };

  const handleDelete = async (name: string) => {
    const ok = await confirm({
      title: `Delete schedule "${name}"?`,
      description: 'This cron schedule will be removed permanently.',
      confirmText: 'Delete',
      variant: 'destructive',
    });
    if (ok) {
      deleteSchedule.mutate(name);
    }
  };

  const handleRunNow = (schedule: ScheduleInfo) => {
    const platform = schedule.platform?.trim() || undefined;
    const branch = schedule.branch?.trim() || undefined;
    const onSuccess = (data: LaunchResponse) => {
      if (isQueued(data)) {
        toast.success('Run queued', {
          description: 'The environment is busy with an exclusive run. It will start automatically.',
          duration: 8000,
        });
        return;
      }
      toast.success('Run started', {
        duration: 8000,
        action: {
          label: 'Open Run',
          onClick: () => {
            window.location.href = `/runs/${data.workflow_name}`;
          },
        },
      });
    };
    const onError = (err: unknown) =>
      toast.error('Failed to start run', { description: String(err) });

    const scheduleId = schedule.schedule_id || undefined;
    if (schedule.test_file?.trim()) {
      runSingleTest.mutate(
        { planId: schedule.plan_id, testFile: schedule.test_file, platform, branch, scheduleId },
        { onSuccess, onError }
      );
    } else if (schedule.plan_type === 'custom_plan') {
      runCustomPlan.mutate({ id: schedule.plan_id, platform, branch, scheduleId }, { onSuccess, onError });
    } else {
      runPlan.mutate({ planId: schedule.plan_id, platform, branch, scheduleId }, { onSuccess, onError });
    }
  };

  const handleSetSlackNotifications = (schedule: ScheduleInfo, enabled: boolean) => {
    setScheduleNotifications.mutate({
      name: schedule.name,
      data: {
        enabled,
        channel: schedule.slack_channel ?? null,
        events: schedule.slack_notification_events ?? [],
      },
    });
  };

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
        <p className="text-destructive">Failed to load schedules</p>
        <p className="text-sm text-muted-foreground mt-2">{error.message}</p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Schedules</h1>
          <p className="text-muted-foreground">
            Manage automated test execution schedules — scoped to the selected
            product. A schedule that can't be attributed to one product isn't
            listed here: its repository or custom plan has been deleted, or it
            runs a custom plan whose tests span two products or resolve to none.
          </p>
        </div>
        <CreateScheduleDialog />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>All Schedules</CardTitle>
          <CardDescription>Cron-based test plan execution schedules</CardDescription>
        </CardHeader>
        <CardContent>
          <SchedulesTable
            schedules={sortedSchedules}
            onSuspend={handleSuspend}
            onResume={handleResume}
            onDelete={handleDelete}
            onRunNow={handleRunNow}
            onEdit={setEditingSchedule}
            onSetSlackNotifications={handleSetSlackNotifications}
            onEditSlackConfig={setEditingSlackSchedule}
          />
        </CardContent>
      </Card>

      <EditScheduleSlackDialog
        schedule={editingSlackSchedule}
        open={editingSlackSchedule !== null}
        onOpenChange={(open) => {
          if (!open) {
            setEditingSlackSchedule(null);
          }
        }}
      />

      {editingSchedule && (
        <CreateScheduleDialog
          editSchedule={editingSchedule}
          open={editingSchedule !== null}
          onOpenChange={(open) => {
            if (!open) {
              setEditingSchedule(null);
            }
          }}
        />
      )}
    </div>
  );
}
