import { useState, useEffect } from 'react';
import { toast } from 'sonner';
import { useJiraConfig, useUpdateJiraConfig } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { Loader2, Save } from 'lucide-react';
import type { JiraConfig } from '@/api/types';

export function SettingsPage() {
  const { data: config, isLoading } = useJiraConfig();
  const updateConfig = useUpdateJiraConfig();

  const [formData, setFormData] = useState<JiraConfig>({
    url: '',
    project_key: '',
    email: '',
    api_token: '',
    issue_type: 'Bug',
    enabled: false,
  });

  useEffect(() => {
    if (config) {
      setFormData(config);
    }
  }, [config]);

  const handleSave = () => {
    if (formData.enabled && (!formData.url || !formData.project_key || !formData.email)) {
      toast.error('Please fill in URL, Project Key, and Email to enable JIRA');
      return;
    }
    updateConfig.mutate(formData, {
      onSuccess: () => toast.success('JIRA settings saved'),
      onError: (err) => toast.error('Failed to save settings', { description: String(err) }),
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
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-semibold">Settings</h1>
        <p className="text-muted-foreground">Configure integrations and system settings</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>JIRA Integration</CardTitle>
          <CardDescription>
            Configure JIRA Cloud connection for automatic bug tracking.
            When enabled, JIRA tickets will be created for failed tests.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="flex items-center justify-between">
            <div>
              <Label htmlFor="enabled" className="text-base">Enable JIRA Integration</Label>
              <p className="text-sm text-muted-foreground">Create JIRA tickets for failed tests</p>
            </div>
            <Switch
              id="enabled"
              checked={formData.enabled}
              onCheckedChange={(checked) => setFormData({ ...formData, enabled: checked })}
            />
          </div>

          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="url">JIRA URL</Label>
              <Input
                id="url"
                value={formData.url}
                onChange={(e) => setFormData({ ...formData, url: e.target.value })}
                placeholder="https://your-domain.atlassian.net"
              />
            </div>

            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="project_key">Project Key</Label>
                <Input
                  id="project_key"
                  value={formData.project_key}
                  onChange={(e) => setFormData({ ...formData, project_key: e.target.value.toUpperCase() })}
                  placeholder="KEY"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="issue_type">Issue Type</Label>
                <Input
                  id="issue_type"
                  value={formData.issue_type || ''}
                  onChange={(e) => setFormData({ ...formData, issue_type: e.target.value })}
                  placeholder="Bug"
                />
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="email">Email</Label>
              <Input
                id="email"
                type="email"
                value={formData.email}
                onChange={(e) => setFormData({ ...formData, email: e.target.value })}
                placeholder="your-email@company.com"
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="api_token">API Token</Label>
              <Input
                id="api_token"
                type="password"
                value={formData.api_token}
                onChange={(e) => setFormData({ ...formData, api_token: e.target.value })}
                placeholder="Enter JIRA API token"
              />
              <p className="text-xs text-muted-foreground">
                Generate a token at https://id.atlassian.com/manage-profile/security/api-tokens
              </p>
            </div>
          </div>

          <Button onClick={handleSave} disabled={updateConfig.isPending}>
            {updateConfig.isPending ? <Loader2 className="h-4 w-4 animate-spin mr-2" /> : <Save className="h-4 w-4 mr-2" />}
            Save Settings
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}
