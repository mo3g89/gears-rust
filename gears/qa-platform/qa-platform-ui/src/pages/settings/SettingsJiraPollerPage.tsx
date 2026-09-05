import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Loader2, Save } from 'lucide-react';
import { useJiraPollerConfig, useUpdateJiraPollerConfig } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Label } from '@/components/ui/label';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import type { JiraPollerConfig } from '@/api/types';

export function SettingsJiraPollerPage() {
  const { data, isLoading } = useJiraPollerConfig();
  const update = useUpdateJiraPollerConfig();
  const [form, setForm] = useState<JiraPollerConfig>({
    poll_interval_seconds: 300,
    auto_rerun_on_resolve: true,
  });

  useEffect(() => {
    if (data) {
      setForm(data);
    }
  }, [data]);

  const save = () => {
    update.mutate(form, {
      onSuccess: () => toast.success('JIRA poller settings saved'),
      onError: (err) => toast.error('Failed to save JIRA poller settings', { description: String(err) }),
    });
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>JIRA Poller</CardTitle>
        <CardDescription>Control polling interval and automatic re-run behavior.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="jira-poller-interval">Polling Interval (seconds)</Label>
          <Input
            id="jira-poller-interval"
            type="number"
            min={1}
            value={form.poll_interval_seconds}
            onChange={(e) => setForm({ ...form, poll_interval_seconds: Number(e.target.value) || 1 })}
          />
        </div>
        <div className="flex items-center justify-between">
          <div>
            <Label htmlFor="jira-poller-auto-rerun" className="text-base">Auto rerun on resolve</Label>
            <p className="text-sm text-muted-foreground">Trigger a test re-run when linked JIRA issue is resolved.</p>
          </div>
          <Switch
            id="jira-poller-auto-rerun"
            checked={form.auto_rerun_on_resolve}
            onCheckedChange={(checked) => setForm({ ...form, auto_rerun_on_resolve: checked })}
          />
        </div>
        <Button onClick={save} disabled={update.isPending}>
          {update.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : <Save className="h-4 w-4 mr-2" />}
          Save JIRA Poller Settings
        </Button>
      </CardContent>
    </Card>
  );
}
