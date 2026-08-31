import type { Dispatch, SetStateAction } from 'react';
import { useOutletContext } from 'react-router-dom';
import { RefreshCw } from 'lucide-react';
import type {
  NotificationLogEntry,
  NotificationsConfig,
  ScheduledRunNotificationEvent,
  ScheduledRunSlackTemplate,
  ScheduledRunSlackTemplatesConfig,
} from '@/api/types';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';

function defaultHeaderTemplate() {
  return '{{status_icon}} `{{run_name}}` — *{{status_headline}}*';
}

function defaultStatusIcon(event: ScheduledRunNotificationEvent) {
  switch (event) {
    case 'pending':
      return ':hourglass_flowing_sand:';
    case 'in_progress':
      return ':large_blue_circle:';
    case 'succeeded':
      return ':large_green_circle:';
    case 'failed':
      return ':red_circle:';
    case 'error':
      return ':warning:';
    case 'skipped':
      return ':white_circle:';
  }
}

function defaultSummaryTemplate() {
  return '`{{plan_id}}`{{#if platform}}  ·  {{platform}}{{/if}}{{#if product_key}}  ·  {{product_key}}{{#if version_display}} {{version_display}}{{/if}}{{/if}}{{#if_event pending}}{{#if test_version}}  ·  {{test_version}}{{/if}}{{/if_event}}';
}

function defaultResultsTemplate() {
  return '{{#if results_passed}}:white_check_mark: {{results_passed}}{{/if}}{{#if results_failed}}   :x: {{results_failed}}{{/if}}{{#if results_skipped}}   :fast_forward: {{results_skipped}}{{/if}}{{#if duration}}   :stopwatch: {{duration}}{{/if}}';
}

function defaultBodyTemplate() {
  return '{{#if message}}>{{message}}\n\n{{/if}}{{#if run_url}}<{{run_url}}|Open run>{{/if}}';
}

function legacyDefaultBodyTemplate() {
  return '{{#if message}}>{{message}}{{/if}}';
}

function defaultFooterTemplate() {
  return '{{#if schedule_id}}{{schedule_id}}{{/if}}{{#if repo_name}}  ·  {{repo_name}}{{#if source_ref}} @ {{source_ref}}{{/if}}{{/if}}{{#if started_at}}  ·  Started: {{started_at}}{{/if}}{{#if finished_at}}  ·  Finished: {{finished_at}}{{/if}}';
}

function buildDefaultTemplate(
  event: ScheduledRunNotificationEvent,
  enabled: boolean
): ScheduledRunSlackTemplate {
  return {
    enabled,
    status_icon: defaultStatusIcon(event),
    header: defaultHeaderTemplate(),
    summary: defaultSummaryTemplate(),
    results: defaultResultsTemplate(),
    body: defaultBodyTemplate(),
    footer: defaultFooterTemplate(),
  };
}

export const DEFAULT_SCHEDULED_TEMPLATES: ScheduledRunSlackTemplatesConfig = {
  pending: buildDefaultTemplate('pending', true),
  in_progress: buildDefaultTemplate('in_progress', true),
  succeeded: buildDefaultTemplate('succeeded', true),
  failed: buildDefaultTemplate('failed', true),
  error: buildDefaultTemplate('error', true),
  skipped: buildDefaultTemplate('skipped', true),
};

export const DEFAULT_FORM: NotificationsConfig = {
  slack_webhook_url: '',
  slack_channel: '',
  manager_ui_base_url: '',
  slack_enabled: true,
  notify_on_failure: true,
  notify_on_success: false,
  notify_on_schedule_completion: false,
  scheduled_run_slack_enabled: true,
  scheduled_run_slack_templates: DEFAULT_SCHEDULED_TEMPLATES,
  email_smtp_host: '',
  email_smtp_port: 587,
  email_from: '',
  email_recipients: '',
  email_enabled: false,
};

export const EVENT_OPTIONS: Array<{
  value: ScheduledRunNotificationEvent;
  label: string;
  description: string;
}> = [
  {
    value: 'pending',
    label: 'Pending',
    description: 'Queued by the scheduler and waiting for execution.',
  },
  {
    value: 'in_progress',
    label: 'In progress',
    description: 'Actively running in Argo.',
  },
  {
    value: 'succeeded',
    label: 'Succeeded',
    description: 'Finished successfully.',
  },
  {
    value: 'failed',
    label: 'Failed',
    description: 'Completed with test failures.',
  },
  {
    value: 'error',
    label: 'Error',
    description: 'Workflow ended in an execution error.',
  },
  {
    value: 'skipped',
    label: 'Skipped',
    description: 'Completed with skipped tests surfaced as the final state.',
  },
];

export const TEMPLATE_FIELDS: Array<{
  token: string;
  description: string;
}> = [
  { token: '{{status}}', description: 'Selected status label, such as Failed or In progress.' },
  {
    token: '{{status_icon}}',
    description: 'Configurable Slack emoji token for the selected status, such as :red_circle:.',
  },
  { token: '{{status_headline}}', description: 'Short status headline such as Failed or Running.' },
  { token: '{{run_name}}', description: 'Workflow run name.' },
  { token: '{{plan_id}}', description: 'Plan identifier.' },
  { token: '{{phase}}', description: 'Underlying workflow phase reported by the runner.' },
  { token: '{{platform}}', description: 'Target platform or - when missing.' },
  { token: '{{product_key}}', description: 'Product key or - when missing.' },
  { token: '{{app_version}}', description: 'Application version or - when missing.' },
  { token: '{{app_build}}', description: 'Application build number or - when missing.' },
  { token: '{{test_version}}', description: 'Selected tests version or branch/tag reference.' },
  { token: '{{run_source}}', description: 'Run source, such as scheduled or manual.' },
  { token: '{{schedule_id}}', description: 'Scheduled run identifier when present.' },
  { token: '{{repo_name}}', description: 'Test repository name or - when missing.' },
  { token: '{{source_ref}}', description: 'Repository branch, tag, or commit reference.' },
  { token: '{{duration}}', description: 'Run duration for finished runs.' },
  { token: '{{message}}', description: 'Workflow status message or fallback text.' },
  { token: '{{run_url}}', description: 'Absolute link to the run details page in Manager UI.' },
  { token: '{{version_display}}', description: 'Combined app version and build display text.' },
  { token: '{{results_total}}', description: 'Total result count.' },
  { token: '{{results_summary}}', description: 'Compact passed/failed/skipped/total summary.' },
  { token: '{{results_passed}}', description: 'Passed result count.' },
  { token: '{{results_failed}}', description: 'Failed plus error result count.' },
  { token: '{{results_skipped}}', description: 'Skipped result count.' },
  { token: '{{started_at}}', description: 'Run start timestamp.' },
  { token: '{{finished_at}}', description: 'Run finish timestamp.' },
];

export const TEMPLATE_SECTIONS: Array<{
  syntax: string;
  description: string;
}> = [
  {
    syntax: '{{#if message}}Message: {{message}}{{/if}}',
    description: 'Render the block only when a field has a meaningful value.',
  },
  {
    syntax: '{{#if_event failed}}Investigate failures{{/if_event}}',
    description: 'Render the block only for one scheduled-run status.',
  },
  {
    syntax:
      '{{#if_event failed}}{{#if results_failed}}Failed: {{results_failed}}{{/if}}{{/if_event}}',
    description: 'Sections can be nested for more precise messages.',
  },
];

export const TEMPLATE_FORMATTING: Array<{
  syntax: string;
  description: string;
}> = [
  {
    syntax: '*bold*  _italic_  ~strike~',
    description: 'Slack mrkdwn emphasis works inside the template body section.',
  },
  {
    syntax: '`inline code`  ```multi-line code```',
    description: 'Useful for versions, refs, and compact machine-readable snippets.',
  },
  {
    syntax: '<https://example.com|Open run>',
    description: 'Slack-formatted links are preserved in the rendered message body.',
  },
];

export interface NotificationsFormContextValue {
  form: NotificationsConfig;
  isSaving: boolean;
  saveEmailNotifications: () => void;
  saveSlackNotifications: () => void;
  setForm: Dispatch<SetStateAction<NotificationsConfig>>;
}

export function useNotificationsForm() {
  return useOutletContext<NotificationsFormContextValue>();
}

export function normalizeNotificationsConfig(
  config?: Partial<NotificationsConfig> | null
): NotificationsConfig {
  const templates = config?.scheduled_run_slack_templates;
  const normalizeTemplate = (
    template: ScheduledRunSlackTemplate | undefined,
    fallback: ScheduledRunSlackTemplate
  ): ScheduledRunSlackTemplate => ({
    ...fallback,
    ...template,
    enabled: true,
    status_icon: template?.status_icon ?? fallback.status_icon ?? '',
    header: template?.header ?? fallback.header ?? '',
    summary: template?.summary ?? fallback.summary ?? '',
    results: template?.results ?? fallback.results ?? '',
    body:
      template?.body === legacyDefaultBodyTemplate()
        ? fallback.body ?? ''
        : (template?.body ?? fallback.body ?? ''),
    footer: template?.footer ?? fallback.footer ?? '',
  });

  return {
    ...DEFAULT_FORM,
    ...config,
    slack_enabled: true,
    scheduled_run_slack_enabled: true,
    scheduled_run_slack_templates: {
      pending: normalizeTemplate(templates?.pending, DEFAULT_SCHEDULED_TEMPLATES.pending),
      in_progress: normalizeTemplate(
        templates?.in_progress,
        DEFAULT_SCHEDULED_TEMPLATES.in_progress
      ),
      succeeded: normalizeTemplate(templates?.succeeded, DEFAULT_SCHEDULED_TEMPLATES.succeeded),
      failed: normalizeTemplate(templates?.failed, DEFAULT_SCHEDULED_TEMPLATES.failed),
      error: normalizeTemplate(templates?.error, DEFAULT_SCHEDULED_TEMPLATES.error),
      skipped: normalizeTemplate(templates?.skipped, DEFAULT_SCHEDULED_TEMPLATES.skipped),
    },
  };
}

export function resolveManagerUiBaseUrl(value?: string | null): string {
  const trimmed = value?.trim() ?? '';
  if (trimmed) {
    return trimmed;
  }

  if (typeof window !== 'undefined') {
    return window.location.origin;
  }

  return '';
}

function outcomeBadgeVariant(outcome: string): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (outcome) {
    case 'sent':
      return 'default';
    case 'skipped':
      return 'secondary';
    case 'error':
      return 'destructive';
    default:
      return 'outline';
  }
}

export function NotificationLogSection({
  entries,
  isLoading,
  onRefresh,
}: {
  entries: NotificationLogEntry[];
  isLoading: boolean;
  onRefresh: () => void;
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0">
        <div className="space-y-1">
          <CardTitle className="text-lg">Notification Log</CardTitle>
          <CardDescription>
            Recent notification decisions showing what was sent, skipped, or failed and why.
            Auto-refreshes every 15 seconds.
          </CardDescription>
        </div>
        <Button variant="ghost" size="icon" onClick={onRefresh} disabled={isLoading}>
          <RefreshCw className={`h-4 w-4 ${isLoading ? 'animate-spin' : ''}`} />
        </Button>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">
            {isLoading ? 'Loading…' : 'No notification log entries yet.'}
          </p>
        ) : (
          <div className="max-h-[420px] overflow-auto rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-[160px]">Time</TableHead>
                  <TableHead>Run</TableHead>
                  <TableHead>Channel</TableHead>
                  <TableHead>Event</TableHead>
                  <TableHead>Outcome</TableHead>
                  <TableHead>Detail</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {entries.map((entry) => (
                  <TableRow key={entry.id}>
                    <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                      {new Date(entry.created_at).toLocaleString()}
                    </TableCell>
                    <TableCell className="max-w-[180px] truncate font-mono text-xs">
                      {entry.workflow_name}
                    </TableCell>
                    <TableCell className="text-xs">{entry.channel}</TableCell>
                    <TableCell className="text-xs">{entry.event_type}</TableCell>
                    <TableCell>
                      <Badge variant={outcomeBadgeVariant(entry.outcome)}>{entry.outcome}</Badge>
                    </TableCell>
                    <TableCell className="max-w-[300px] truncate text-xs" title={entry.detail}>
                      {entry.detail}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
