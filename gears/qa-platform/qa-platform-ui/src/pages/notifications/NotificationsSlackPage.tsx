import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { ChevronDown, Eye, Loader2, Save, Send } from 'lucide-react';
import {
  usePreviewScheduledRunNotification,
  useTestScheduledRunNotification,
} from '@/api/hooks';
import type {
  ScheduledRunNotificationEvent,
  ScheduledRunSlackTemplate,
  ScheduledRunSlackTemplatesConfig,
} from '@/api/types';
import { SlackMessagePreview } from '@/components/notifications/SlackMessagePreview';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Textarea } from '@/components/ui/textarea';
import { cn } from '@/lib/utils';
import {
  EVENT_OPTIONS,
  TEMPLATE_FIELDS,
  TEMPLATE_FORMATTING,
  TEMPLATE_SECTIONS,
  resolveManagerUiBaseUrl,
  useNotificationsForm,
} from './notificationsShared';

type EditableTemplateSectionKey = 'header' | 'summary' | 'results' | 'footer' | 'body';

const TEMPLATE_SECTION_EDITORS: Array<{
  key: EditableTemplateSectionKey;
  title: string;
  description: string;
  placeholder: string;
  rows: number;
}> = [
  {
    key: 'header',
    title: 'Header',
    description: 'Top line of the message, usually status and run name.',
    placeholder: '{{status_icon}} `{{run_name}}` — *{{status_headline}}*',
    rows: 3,
  },
  {
    key: 'summary',
    title: 'Summary',
    description: 'Compact run metadata shown just under the header.',
    placeholder: '`{{plan_id}}`  ·  {{platform}}  ·  {{product_key}} {{version_display}}',
    rows: 4,
  },
  {
    key: 'results',
    title: 'Results',
    description: 'Short counts line for passed, failed, skipped, and duration.',
    placeholder:
      '{{#if results_passed}}:white_check_mark: {{results_passed}}{{/if}}   :x: {{results_failed}}',
    rows: 4,
  },
  {
    key: 'footer',
    title: 'Footer',
    description: 'Context block for schedule, repository, and timing details.',
    placeholder: '{{schedule_id}}  ·  {{repo_name}} @ {{source_ref}}',
    rows: 4,
  },
  {
    key: 'body',
    title: 'Body',
    description: 'Main detail block for message text, guidance, or callouts.',
    placeholder: '{{#if message}}>{{message}}\n\n{{/if}}{{#if run_url}}<{{run_url}}|Open run>{{/if}}',
    rows: 7,
  },
];

export function NotificationsSlackPage() {
  const { form, isSaving, saveSlackNotifications, setForm } = useNotificationsForm();
  const previewNotification = usePreviewScheduledRunNotification();
  const testScheduledNotification = useTestScheduledRunNotification();
  const [selectedEvent, setSelectedEvent] = useState<ScheduledRunNotificationEvent>('failed');
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewDirty, setPreviewDirty] = useState(true);

  const selectedTemplate = form.scheduled_run_slack_templates[selectedEvent];
  const selectedOption =
    EVENT_OPTIONS.find((option) => option.value === selectedEvent) ?? EVENT_OPTIONS[0];
  const renderedPreview =
    previewNotification.data?.event === selectedEvent ? previewNotification.data : null;
  const resolvedManagerUiBaseUrl = resolveManagerUiBaseUrl(form.manager_ui_base_url);

  const slackPreviewConfig = {
    ...form,
    manager_ui_base_url: resolvedManagerUiBaseUrl,
  };

  const updateScheduledTemplate = (
    event: keyof ScheduledRunSlackTemplatesConfig,
    patch: Partial<ScheduledRunSlackTemplate>
  ) => {
    setForm((current) => ({
      ...current,
      scheduled_run_slack_templates: {
        ...current.scheduled_run_slack_templates,
        [event]: {
          ...current.scheduled_run_slack_templates[event],
          ...patch,
        },
      },
    }));
  };

  const renderPreview = async () => {
    try {
      await previewNotification.mutateAsync({
        config: slackPreviewConfig,
        event: selectedEvent,
      });
      setPreviewError(null);
      setPreviewDirty(false);
    } catch (err) {
      const description = String(err);
      setPreviewError(description);
      toast.error('Failed to render notification preview', { description });
    }
  };

  useEffect(() => {
    setPreviewError(null);
    setPreviewDirty(true);
  }, [
    form.slack_channel,
    form.manager_ui_base_url,
    form.slack_webhook_url,
    selectedEvent,
    selectedTemplate.status_icon,
    selectedTemplate.header,
    selectedTemplate.summary,
    selectedTemplate.results,
    selectedTemplate.body,
    selectedTemplate.footer,
  ]);

  const sendSelectedSlackTest = () => {
    testScheduledNotification.mutate(
      {
        config: slackPreviewConfig,
        event: selectedEvent,
      },
      {
        onSuccess: () => toast.success('Scheduled-run Slack test sent'),
        onError: (err) =>
          toast.error('Failed to send scheduled-run Slack test', { description: String(err) }),
      }
    );
  };

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader className="pb-4">
          <CardTitle>Slack Delivery</CardTitle>
          <CardDescription>
            Configure the destination and Manager UI base URL used for previews and scheduled-run
            Slack notifications.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(280px,0.85fr)]">
          <div className="space-y-2">
            <Label htmlFor="notif-slack-webhook">Slack Webhook URL</Label>
            <Input
              id="notif-slack-webhook"
              value={form.slack_webhook_url}
              onChange={(e) =>
                setForm((current) => ({ ...current, slack_webhook_url: e.target.value }))
              }
              placeholder="https://hooks.slack.com/services/..."
            />
            <p className="text-xs text-muted-foreground">
              Required to render previews and send scheduled-run test messages to Slack.
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="notif-manager-ui-base-url">Manager UI Base URL</Label>
            <Input
              id="notif-manager-ui-base-url"
              value={form.manager_ui_base_url}
              onChange={(e) =>
                setForm((current) => ({ ...current, manager_ui_base_url: e.target.value }))
              }
              placeholder={resolvedManagerUiBaseUrl || 'https://testrunner.example.com'}
            />
            <p className="text-xs text-muted-foreground">
              Used for the <span className="font-mono">{'{{run_url}}'}</span> token. When blank,
              previews use the current browser origin.
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="notif-slack-channel">Default Slack Channel</Label>
            <Input
              id="notif-slack-channel"
              value={form.slack_channel}
              onChange={(e) =>
                setForm((current) => ({ ...current, slack_channel: e.target.value }))
              }
              placeholder="#qa-alerts"
            />
            <p className="text-xs text-muted-foreground">
              Used when a schedule does not override its own destination channel.
            </p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-4">
          <CardTitle>Scheduled Run Slack Templates</CardTitle>
          <CardDescription>
            Edit separate template sections and preview the full rendered Slack message without
            leaving the page.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 xl:grid-cols-[210px_minmax(0,1fr)]">
          <div className="rounded-lg border">
            <ScrollArea className="h-[560px]">
              <div className="space-y-1 p-2">
                {EVENT_OPTIONS.map((option) => {
                  const isSelected = option.value === selectedEvent;

                  return (
                    <button
                      key={option.value}
                      type="button"
                      onClick={() => setSelectedEvent(option.value)}
                      className={cn(
                        'w-full rounded-md px-3 py-2 text-left text-sm font-medium transition-colors',
                        isSelected
                          ? 'bg-accent text-accent-foreground'
                          : 'text-muted-foreground hover:bg-accent/40 hover:text-foreground'
                      )}
                    >
                      {option.label}
                    </button>
                  );
                })}
              </div>
            </ScrollArea>
          </div>

          <div className="grid gap-4 2xl:grid-cols-[minmax(0,1fr)_minmax(360px,0.95fr)]">
            <div className="space-y-4">
              <div className="rounded-lg border p-4">
                <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_220px]">
                  <div className="space-y-1">
                    <h3 className="text-lg font-semibold">{selectedOption.label}</h3>
                    <p className="text-sm text-muted-foreground">{selectedOption.description}</p>
                  </div>

                  <div className="space-y-2">
                    <Label htmlFor="scheduled-template-status-icon">Status Icon</Label>
                    <Input
                      id="scheduled-template-status-icon"
                      value={selectedTemplate.status_icon ?? ''}
                      onChange={(e) =>
                        updateScheduledTemplate(selectedEvent, {
                          status_icon: e.target.value,
                        })
                      }
                      placeholder=":red_circle:"
                      className="font-mono text-xs"
                    />
                    <p className="text-xs text-muted-foreground">
                      Used by <span className="font-mono">{'{{status_icon}}'}</span> in the
                      template sections.
                    </p>
                  </div>
                </div>
              </div>

              <div className="grid gap-4 xl:grid-cols-2">
                {TEMPLATE_SECTION_EDITORS.map((section) => {
                  const rawValue = selectedTemplate[section.key];
                  const value = typeof rawValue === 'string' ? rawValue : '';
                  const isBody = section.key === 'body';

                  return (
                    <div
                      key={section.key}
                      className={cn('rounded-lg border p-4', isBody ? 'xl:col-span-2' : null)}
                    >
                      <div className="space-y-1">
                        <Label htmlFor={`scheduled-template-${section.key}`}>{section.title}</Label>
                        <p className="text-xs text-muted-foreground">{section.description}</p>
                      </div>
                      <Textarea
                        id={`scheduled-template-${section.key}`}
                        rows={section.rows}
                        value={value}
                        onChange={(e) =>
                          updateScheduledTemplate(selectedEvent, {
                            [section.key]: e.target.value,
                          } as Partial<ScheduledRunSlackTemplate>)
                        }
                        className="mt-3 font-mono text-xs leading-5"
                        placeholder={section.placeholder}
                      />
                      {isBody ? (
                        <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
                          <p className="text-xs text-muted-foreground">
                            Supports Slack mrkdwn, template fields, and conditional sections.
                          </p>
                          <Button onClick={saveSlackNotifications} disabled={isSaving}>
                            {isSaving ? (
                              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                            ) : (
                              <Save className="mr-2 h-4 w-4" />
                            )}
                            Save Slack Templates
                          </Button>
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>

              <details className="rounded-lg border bg-muted/10 px-4 py-3">
                <summary className="flex cursor-pointer list-none items-center justify-between gap-3 text-sm font-medium">
                  Template Help
                  <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground transition-transform details-open:rotate-180" />
                </summary>
                <p className="mt-2 text-xs text-muted-foreground">
                  Available fields, conditional blocks, and Slack formatting reference.
                </p>

                <div className="mt-4 grid gap-4 xl:grid-cols-3">
                  <ReferenceSection
                    description="Available values injected into each selected template section."
                    items={TEMPLATE_FIELDS}
                    title="Template Fields"
                  />
                  <ReferenceSection
                    description="Conditionally render content for fields or specific scheduled-run events."
                    items={TEMPLATE_SECTIONS}
                    title="Conditional Sections"
                  />
                  <ReferenceSection
                    description="Slack mrkdwn examples preserved in the rendered message preview."
                    items={TEMPLATE_FORMATTING}
                    title="Slack Formatting"
                  />
                </div>
              </details>
            </div>

            <div className="space-y-4">
              <div className="rounded-lg border p-4">
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    variant="outline"
                    onClick={() => void renderPreview()}
                    disabled={previewNotification.isPending}
                  >
                    {previewNotification.isPending ? (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    ) : (
                      <Eye className="mr-2 h-4 w-4" />
                    )}
                    Render Preview
                  </Button>
                  <Button
                    onClick={sendSelectedSlackTest}
                    disabled={testScheduledNotification.isPending || !form.slack_webhook_url.trim()}
                  >
                    {testScheduledNotification.isPending ? (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    ) : (
                      <Send className="mr-2 h-4 w-4" />
                    )}
                    Send Preview to Slack
                  </Button>
                </div>
                <p className="mt-3 text-xs text-muted-foreground">
                  {previewDirty
                    ? 'Preview is out of date. Render again to validate the latest template edits.'
                    : 'Preview matches the current template state.'}
                </p>
              </div>

              <SlackMessagePreview
                blocks={renderedPreview?.blocks}
                channel={form.slack_channel}
                error={previewError}
                eventLabel={selectedOption.label}
                fallbackText={renderedPreview?.fallback_text}
                isDirty={previewDirty}
                isLoading={previewNotification.isPending}
              />

              <details className="rounded-lg border bg-muted/10 px-4 py-3">
                <summary className="cursor-pointer text-sm font-medium">Block Kit Payload</summary>
                <Textarea
                  readOnly
                  rows={14}
                  value={
                    renderedPreview
                      ? JSON.stringify(renderedPreview.blocks, null, 2)
                      : 'The generated Slack payload will appear here after preview rendering.'
                  }
                  className="mt-3 font-mono text-xs"
                />
              </details>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function ReferenceSection({
  description,
  items,
  title,
}: {
  description: string;
  items: Array<{ description: string; syntax?: string; token?: string }>;
  title: string;
}) {
  return (
    <div className="rounded-lg border bg-background p-4">
      <div className="space-y-1">
        <div className="text-sm font-medium">{title}</div>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>

      <div className="mt-3 space-y-2">
        {items.map((item) => {
          const value = item.token ?? item.syntax ?? '';
          return (
            <div key={value} className="rounded-md border bg-muted/20 p-3">
              <div className="whitespace-pre-wrap font-mono text-xs">{value}</div>
              <div className="mt-1 text-xs text-muted-foreground">{item.description}</div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
