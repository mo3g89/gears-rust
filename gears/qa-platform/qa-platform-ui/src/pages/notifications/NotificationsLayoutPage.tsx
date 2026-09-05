import { useEffect, useState } from 'react';
import { Outlet } from 'react-router-dom';
import { toast } from 'sonner';
import { Loader2 } from 'lucide-react';
import {
  useNotificationLog,
  useNotificationsConfig,
  useUpdateNotificationsConfig,
} from '@/api/hooks';
import {
  NotificationLogSection,
  normalizeNotificationsConfig,
  resolveManagerUiBaseUrl,
  DEFAULT_FORM,
} from './notificationsShared';
import type { NotificationsConfig } from '@/api/types';

export function NotificationsLayoutPage() {
  const { data, isLoading } = useNotificationsConfig();
  const update = useUpdateNotificationsConfig();
  const notificationLog = useNotificationLog();
  const [form, setForm] = useState<NotificationsConfig>(DEFAULT_FORM);

  useEffect(() => {
    if (data) {
      setForm(normalizeNotificationsConfig(data));
    }
  }, [data]);

  const saveNotifications = (channel: 'email' | 'slack') => {
    const managerUiBaseUrl = resolveManagerUiBaseUrl(form.manager_ui_base_url);

    update.mutate(
      {
        ...form,
        manager_ui_base_url: managerUiBaseUrl,
        slack_enabled: true,
        scheduled_run_slack_enabled: true,
        scheduled_run_slack_templates: {
          pending: {
            ...form.scheduled_run_slack_templates.pending,
            enabled: true,
          },
          in_progress: {
            ...form.scheduled_run_slack_templates.in_progress,
            enabled: true,
          },
          succeeded: {
            ...form.scheduled_run_slack_templates.succeeded,
            enabled: true,
          },
          failed: {
            ...form.scheduled_run_slack_templates.failed,
            enabled: true,
          },
          error: {
            ...form.scheduled_run_slack_templates.error,
            enabled: true,
          },
          skipped: {
            ...form.scheduled_run_slack_templates.skipped,
            enabled: true,
          },
        },
      },
      {
        onSuccess: () =>
          toast.success(
            channel === 'email'
              ? 'Email notification settings saved'
              : 'Slack notification settings saved'
          ),
        onError: (err) =>
          toast.error('Failed to save notification settings', { description: String(err) }),
      }
    );
  };

  if (isLoading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <Outlet
        context={{
          form,
          isSaving: update.isPending,
          saveEmailNotifications: () => saveNotifications('email'),
          saveSlackNotifications: () => saveNotifications('slack'),
          setForm,
        }}
      />

      <NotificationLogSection
        entries={notificationLog.data ?? []}
        isLoading={notificationLog.isLoading}
        onRefresh={() => {
          void notificationLog.refetch();
        }}
      />
    </div>
  );
}
