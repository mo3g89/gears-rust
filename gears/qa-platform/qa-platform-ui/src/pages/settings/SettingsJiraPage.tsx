import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Loader2, Save } from 'lucide-react';
import { useJiraConfig, useJiraPollerConfig, useUpdateJiraConfig, useUpdateJiraPollerConfig } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Label } from '@/components/ui/label';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import type { JiraConfig, JiraPollerConfig } from '@/api/types';

export function SettingsJiraPage() {
  const { data, isLoading } = useJiraConfig();
  const { data: pollerData, isLoading: pollerLoading } = useJiraPollerConfig();
  const update = useUpdateJiraConfig();
  const updatePoller = useUpdateJiraPollerConfig();
  const [form, setForm] = useState<JiraConfig>({
    url: '',
    project_key: '',
    email: '',
    api_token: '',
    issue_type: 'Bug',
    enabled: false,
  });
  const [pollerForm, setPollerForm] = useState<JiraPollerConfig>({
    poll_interval_seconds: 300,
    auto_rerun_on_resolve: true,
  });

  useEffect(() => {
    if (data) {
      setForm(data);
    }
  }, [data]);

  useEffect(() => {
    if (pollerData) {
      setPollerForm(pollerData);
    }
  }, [pollerData]);

  const save = () => {
    if (form.enabled && (!form.url || !form.project_key || !form.email)) {
      toast.error('Please fill in URL, Project Key, and Email to enable JIRA');
      return;
    }

    update.mutate(form, {
      onSuccess: () => toast.success('JIRA settings saved'),
      onError: (err) => toast.error('Failed to save JIRA settings', { description: String(err) }),
    });
  };

  const savePoller = () => {
    updatePoller.mutate(pollerForm, {
      onSuccess: () => toast.success('JIRA poller settings saved'),
      onError: (err) => toast.error('Failed to save JIRA poller settings', { description: String(err) }),
    });
  };

  if (isLoading || pollerLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>JIRA Integration</CardTitle>
          <CardDescription>Configure JIRA Cloud connection for automatic bug tracking.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="flex items-center justify-between">
            <div>
              <Label htmlFor="jira-enabled" className="text-base">Enable JIRA Integration</Label>
              <p className="text-sm text-muted-foreground">Create JIRA tickets for failed tests</p>
            </div>
            <Switch
              id="jira-enabled"
              checked={form.enabled}
              onCheckedChange={(checked) => setForm({ ...form, enabled: checked })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="jira-url">JIRA URL</Label>
            <Input
              id="jira-url"
              value={form.url}
              onChange={(e) => setForm({ ...form, url: e.target.value })}
              placeholder="https://your-domain.atlassian.net"
            />
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="jira-project-key">Project Key</Label>
              <Input
                id="jira-project-key"
                value={form.project_key}
                onChange={(e) => setForm({ ...form, project_key: e.target.value.toUpperCase() })}
                placeholder="KEY"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="jira-issue-type">Issue Type</Label>
              <Input
                id="jira-issue-type"
                value={form.issue_type || ''}
                onChange={(e) => setForm({ ...form, issue_type: e.target.value })}
                placeholder="Bug"
              />
            </div>
          </div>
          <div className="space-y-2">
            <Label htmlFor="jira-email">Email</Label>
            <Input
              id="jira-email"
              type="email"
              value={form.email}
              onChange={(e) => setForm({ ...form, email: e.target.value })}
              placeholder="your-email@company.com"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="jira-api-token">API Token</Label>
            <Input
              id="jira-api-token"
              type="password"
              value={form.api_token}
              onChange={(e) => setForm({ ...form, api_token: e.target.value })}
              placeholder="Enter JIRA API token"
            />
          </div>
          <Button onClick={save} disabled={update.isPending}>
            {update.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : <Save className="h-4 w-4 mr-2" />}
            Save JIRA Settings
          </Button>
        </CardContent>
      </Card>

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
              value={pollerForm.poll_interval_seconds}
              onChange={(e) =>
                setPollerForm({ ...pollerForm, poll_interval_seconds: Number(e.target.value) || 1 })
              }
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label htmlFor="jira-poller-auto-rerun" className="text-base">Auto rerun on resolve</Label>
              <p className="text-sm text-muted-foreground">Trigger a test re-run when linked JIRA issue is resolved.</p>
            </div>
            <Switch
              id="jira-poller-auto-rerun"
              checked={pollerForm.auto_rerun_on_resolve}
              onCheckedChange={(checked) =>
                setPollerForm({ ...pollerForm, auto_rerun_on_resolve: checked })
              }
            />
          </div>
          <Button onClick={savePoller} disabled={updatePoller.isPending}>
            {updatePoller.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : <Save className="h-4 w-4 mr-2" />}
            Save JIRA Poller Settings
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}
