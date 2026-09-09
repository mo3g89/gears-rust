import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Loader2 } from 'lucide-react';
import { useSetScheduleNotifications } from '@/api/hooks';
import { ScheduleInfo, ScheduledRunNotificationEvent } from '@/api/types';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';

const SCHEDULE_EVENT_OPTIONS: Array<{
  value: ScheduledRunNotificationEvent;
  label: string;
}> = [
  { value: 'pending', label: 'Pending' },
  { value: 'in_progress', label: 'In progress' },
  { value: 'succeeded', label: 'Succeeded' },
  { value: 'failed', label: 'Failed' },
  { value: 'error', label: 'Error' },
  { value: 'skipped', label: 'Skipped' },
];

interface EditScheduleSlackDialogProps {
  schedule: ScheduleInfo | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function EditScheduleSlackDialog({
  schedule,
  open,
  onOpenChange,
}: EditScheduleSlackDialogProps) {
  const updateScheduleNotifications = useSetScheduleNotifications();
  const [enabled, setEnabled] = useState(false);
  const [channel, setChannel] = useState('');
  const [events, setEvents] = useState<ScheduledRunNotificationEvent[]>([]);

  useEffect(() => {
    if (!open || !schedule) {
      return;
    }

    setEnabled(schedule.slack_notifications_enabled);
    setChannel(schedule.slack_channel || '');
    setEvents(schedule.slack_notification_events || []);
  }, [open, schedule]);

  const toggleEvent = (event: ScheduledRunNotificationEvent, checked: boolean) => {
    setEvents((current) => {
      if (checked) {
        return current.includes(event) ? current : [...current, event];
      }
      return current.filter((item) => item !== event);
    });
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!schedule) {
      return;
    }

    updateScheduleNotifications.mutate(
      {
        name: schedule.name,
        data: {
          enabled,
          channel: channel.trim() || null,
          events,
        },
      },
      {
        onSuccess: () => {
          toast.success(`Slack settings updated for "${schedule.schedule_id}"`);
          onOpenChange(false);
        },
        onError: (error) => {
          toast.error('Failed to update schedule Slack settings', {
            description: String(error),
          });
        },
      }
    );
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>Edit Schedule Slack Settings</DialogTitle>
            <DialogDescription>
              Configure Slack delivery for <span className="font-medium">{schedule?.name}</span>.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div className="flex items-center justify-between rounded-lg border p-4">
              <div className="space-y-1">
                <Label htmlFor="schedule-slack-enabled" className="text-sm font-medium">
                  Enable Slack notifications
                </Label>
                <p className="text-xs text-muted-foreground">
                  Status-based scheduled-run Slack templates only send when this schedule is enabled.
                </p>
              </div>
              <Switch
                id="schedule-slack-enabled"
                checked={enabled}
                onCheckedChange={setEnabled}
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="schedule-slack-channel">Slack channel override</Label>
              <Input
                id="schedule-slack-channel"
                value={channel}
                onChange={(e) => setChannel(e.target.value)}
                placeholder="#nightly-qa"
              />
              <p className="text-xs text-muted-foreground">
                Leave blank to use the default Slack channel from Notification Settings. If the
                default channel is also blank, Slack uses the webhook destination.
              </p>
            </div>

            <div className="space-y-3 rounded-lg border p-4">
              <div className="space-y-1">
                <Label className="text-sm font-medium">Status notifications</Label>
                <p className="text-xs text-muted-foreground">
                  Choose which scheduled-run statuses this schedule may send. Leave all unchecked
                  to use the global template enablement.
                </p>
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                {SCHEDULE_EVENT_OPTIONS.map((option) => (
                  <label key={option.value} className="flex items-center gap-3 text-sm">
                    <Checkbox
                      checked={events.includes(option.value)}
                      onCheckedChange={(checked) => toggleEvent(option.value, checked === true)}
                    />
                    <span>{option.label}</span>
                  </label>
                ))}
              </div>
            </div>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!schedule || updateScheduleNotifications.isPending}>
              {updateScheduleNotifications.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Saving...
                </>
              ) : (
                'Save Slack Settings'
              )}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
