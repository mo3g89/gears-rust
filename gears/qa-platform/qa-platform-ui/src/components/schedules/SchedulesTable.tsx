import { ScheduleInfo } from '@/api/types';
import { Button } from '@/components/ui/button';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Bell, BellOff, Filter, MoreVertical, Pause, Pencil, Play, PlayCircle, Trash2 } from 'lucide-react';

function TagFilterBadge({ include, exclude }: { include: string | null; exclude: string | null }) {
  const inc = (include || '').split(',').map((t) => t.trim()).filter(Boolean);
  const exc = (exclude || '').split(',').map((t) => t.trim()).filter(Boolean);
  if (inc.length === 0 && exc.length === 0) return null;
  const parts: string[] = [];
  if (inc.length) parts.push(`only: ${inc.join(', ')}`);
  if (exc.length) parts.push(`skip: ${exc.join(', ')}`);
  // Just the funnel icon — hover for the actual include/exclude tags.
  return (
    <span className="inline-flex items-center" title={`Tag filter — ${parts.join(' · ')}`}>
      <Filter className="h-3.5 w-3.5 text-muted-foreground" />
    </span>
  );
}
import { Link } from 'react-router-dom';
import { useMemo } from 'react';
import { RecentRunsStrip } from '@/components/schedules/RecentRunsStrip';
import { useCustomPlans, usePlans, useEnvironments } from '@/api/hooks';
import { decodePlanId } from '@/api/adapters';
import { displayPlanName } from '@/lib/utils';

/**
 * A readable label for a plan id that neither plan listing resolved.
 *
 * `ScheduleDto` carries no plan name, so `scheduleFromDto` sets `plan_name: null`
 * (`adapters.ts:810`) and this column used to fall through to the raw id — an opaque
 * `{repo_uuid}--{base64(path)}` string, which is what the schedules table actually
 * rendered. Legacy fills its `plan_name` by joining the schedule's plan id against a
 * product-unscoped plan listing (`manager/src/routes/schedules.rs:227-229,296-299`);
 * `SchedulesTable` does the same join client-side, and this is the last resort for the
 * ids that join misses.
 *
 * A repo plan id encodes the plan's own path, so the file's base name is available
 * without any request: `plans/monitoring/grafana.yaml` -> `grafana`. That is not the
 * manifest's `name` and may differ from it, but it names the plan the schedule runs,
 * which the id does not. Anything that is not one of our encoded ids (a custom-plan
 * uuid, a legacy-persisted id) decodes to `null` and is returned unchanged.
 */
export function planLabelFromId(planId: string): string {
  const decoded = decodePlanId(planId);
  const path = decoded?.path?.trim();
  if (!path) {
    return planId;
  }
  const base = path.split('/').filter(Boolean).pop();
  if (!base) {
    return planId;
  }
  const stripped = base.replace(/\.ya?ml$/i, '').trim();
  return stripped || planId;
}

function formatSlackStatuses(statuses: ScheduleInfo['slack_notification_events']): string {
  if (!statuses || statuses.length === 0) {
    return 'Default';
  }

  return statuses
    .map((status) =>
      status
        .split('_')
        .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
        .join(' ')
    )
    .join(', ');
}

function relativeTime(value: string | null): string {
  if (!value) return '';
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return '';
  const diff = Date.now() - d.getTime();
  const m = Math.floor(diff / 60000);
  if (m < 1) return 'just now';
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const days = Math.floor(h / 24);
  return `${days}d ago`;
}

// Turn a 5-field cron expression into a short human-readable cadence. Falls
// back to the raw expression for anything we don't special-case.
function cronLabel(expr: string | null | undefined): string {
  const cron = (expr || '').trim();
  if (!cron) return '—';
  const presets: Record<string, string> = {
    '* * * * *': 'Every minute',
    '*/15 * * * *': 'Every 15 min',
    '*/30 * * * *': 'Every 30 min',
    '0 * * * *': 'Hourly',
    '0 */6 * * *': 'Every 6h',
    '0 */12 * * *': 'Every 12h',
    '0 0 * * *': 'Daily',
    '0 0 * * 1-5': 'Weekdays',
    '0 0 * * 1': 'Weekly',
  };
  if (presets[cron]) return presets[cron];
  const parts = cron.split(/\s+/);
  if (parts.length === 5) {
    const [min, hour, dom, , dow] = parts;
    let m: RegExpMatchArray | null;
    if ((m = min.match(/^\*\/(\d+)$/)) && hour === '*' && dom === '*' && dow === '*') {
      return `Every ${m[1]} min`;
    }
    if (min === '0' && (m = hour.match(/^\*\/(\d+)$/)) && dom === '*' && dow === '*') {
      return `Every ${m[1]}h`;
    }
    if (/^\d+$/.test(min) && /^\d+$/.test(hour) && dom === '*' && dow === '*') {
      return `Daily ${hour.padStart(2, '0')}:${min.padStart(2, '0')}`;
    }
  }
  return cron;
}

function ScheduleStatus({ suspended }: { suspended: boolean }) {
  if (suspended) {
    return (
      <span className="inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <span className="inline-block h-2 w-2 rounded-full bg-muted-foreground" />
        Suspended
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
      <span className="inline-block h-2 w-2 rounded-full bg-emerald-500" />
      Active
    </span>
  );
}

interface SchedulesTableProps {
  schedules: ScheduleInfo[];
  onSuspend?: (name: string) => void;
  onResume?: (name: string) => void;
  onDelete?: (name: string) => void;
  onRunNow?: (schedule: ScheduleInfo) => void;
  onEdit?: (schedule: ScheduleInfo) => void;
  onSetSlackNotifications?: (schedule: ScheduleInfo, enabled: boolean) => void;
  onEditSlackConfig?: (schedule: ScheduleInfo) => void;
}

export function SchedulesTable({
  schedules,
  onSuspend,
  onResume,
  onDelete,
  onRunNow,
  onEdit,
  onSetSlackNotifications,
  onEditSlackConfig,
}: SchedulesTableProps) {
  const { data: environments } = useEnvironments();
  const environmentByName = useMemo(
    () => new Map((environments || []).map((p) => [p.name, p])),
    [environments]
  );

  // Product-unscoped, matching legacy's own `None` product filter: this table shows
  // every schedule in the deployment, so scoping the lookup to the active product
  // would leave other products' schedules showing their raw ids.
  const { data: plans } = usePlans(undefined, null);
  const { data: customPlans } = useCustomPlans();
  const planNameById = useMemo(() => {
    const byId = new Map<string, string>();
    (plans || []).forEach((plan) => {
      const name = plan.plan.name?.trim();
      if (name) {
        byId.set(plan.id, name);
      }
    });
    (customPlans || []).forEach((plan) => {
      const name = plan.name?.trim();
      if (name) {
        byId.set(plan.id, name);
      }
    });
    return byId;
  }, [plans, customPlans]);

  if (schedules.length === 0) {
    return (
      <div className="text-center py-12 text-muted-foreground">
        No schedules configured
      </div>
    );
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Plan</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Environment / ver.</TableHead>
          <TableHead>Branch</TableHead>
          <TableHead>Schedule</TableHead>
          <TableHead>Recent runs</TableHead>
          <TableHead>Last run</TableHead>
          <TableHead className="text-right" title="Total / Pass / In progress / Fail / Skip">
            T / P / IP / F / S
          </TableHead>
          <TableHead>Slack</TableHead>
          <TableHead className="w-[50px]"></TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {schedules.map((schedule) => {
          const latest = schedule.recent_runs?.[0];
          const empty = !latest;
          const lastRunTimestamp = latest?.finished_at || latest?.started_at || schedule.last_scheduled;
          const environment = schedule.platform ? environmentByName.get(schedule.platform) : undefined;
          const environmentVersion = environment?.version
            ? environment.build
              ? `${environment.version} (${environment.build})`
              : environment.version
            : null;
          return (
            <TableRow key={schedule.name}>
              <TableCell>
                <div className="flex items-center gap-1.5">
                  <Link
                    to={schedule.plan_type === 'custom_plan' ? `/plans/custom/${schedule.plan_id}` : `/plans/${schedule.plan_id}`}
                    className="text-foreground/80 hover:text-foreground hover:underline"
                    title={`${schedule.plan_id}${schedule.test_file ? `\nTest file: ${schedule.test_file}` : `\n${schedule.schedule}`}`}
                  >
                    {displayPlanName(
                      schedule.plan_name ||
                        planNameById.get(schedule.plan_id) ||
                        planLabelFromId(schedule.plan_id)
                    )}
                  </Link>
                  <TagFilterBadge include={schedule.include_tags} exclude={schedule.exclude_tags} />
                </div>
              </TableCell>
              <TableCell>
                <ScheduleStatus suspended={schedule.suspended} />
              </TableCell>
              <TableCell className="tabular-nums">
                {schedule.platform ? (
                  <span title={`Environment ${schedule.platform}`}>
                    {schedule.platform}
                    {environmentVersion ? ` / ${environmentVersion}` : ''}
                  </span>
                ) : (
                  <span className="text-muted-foreground">—</span>
                )}
              </TableCell>
              <TableCell className="tabular-nums">
                {schedule.branch?.trim() ? (
                  <span title={`Branch ${schedule.branch}`}>{schedule.branch}</span>
                ) : (
                  <span className="text-muted-foreground">—</span>
                )}
              </TableCell>
              <TableCell>
                {schedule.schedule?.trim() ? (
                  <span className="font-mono text-xs" title={schedule.schedule}>
                    {cronLabel(schedule.schedule)}
                  </span>
                ) : (
                  <span className="text-muted-foreground">—</span>
                )}
              </TableCell>
              <TableCell>
                <RecentRunsStrip runs={schedule.recent_runs || []} visible={2} />
              </TableCell>
              <TableCell className="text-muted-foreground">
                {lastRunTimestamp ? (
                  <span title={lastRunTimestamp}>{relativeTime(lastRunTimestamp)}</span>
                ) : (
                  <span>—</span>
                )}
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {empty ? (
                  <span className="text-muted-foreground">—</span>
                ) : (
                  <span
                    title={`Total ${latest.total} · Pass ${latest.passed} · In progress ${latest.in_progress} · Fail ${latest.failed} · Skip ${latest.skipped} · Expected fail ${latest.xfail} · Unexpected pass ${latest.xpass}`}
                  >
                    <span className="text-muted-foreground">{latest.total}</span>
                    <span className="text-muted-foreground">/</span>
                    <span className="text-emerald-600 dark:text-emerald-400">{latest.passed}</span>
                    <span className="text-muted-foreground">/</span>
                    <span className={latest.in_progress > 0 ? 'text-blue-600 dark:text-blue-400' : 'text-muted-foreground'}>
                      {latest.in_progress}
                    </span>
                    <span className="text-muted-foreground">/</span>
                    <span className={latest.failed > 0 ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'}>
                      {latest.failed}
                    </span>
                    <span className="text-muted-foreground">/</span>
                    <span className={latest.skipped > 0 ? 'text-amber-600 dark:text-amber-400' : 'text-muted-foreground'}>
                      {latest.skipped}
                    </span>
                    {latest.xfail > 0 && (
                      <>
                        <span className="text-muted-foreground">/</span>
                        <span className="text-violet-600 dark:text-violet-400">{latest.xfail}</span>
                      </>
                    )}
                    {latest.xpass > 0 && (
                      <>
                        <span className="text-muted-foreground">/</span>
                        <span className="text-sky-600 dark:text-sky-400">{latest.xpass}</span>
                      </>
                    )}
                  </span>
                )}
              </TableCell>
              <TableCell>
                {schedule.slack_notifications_enabled ? (
                  <span
                    className="inline-flex items-center gap-1.5 text-xs"
                    title={`Channel: ${schedule.slack_channel || 'default'}\nEvents: ${formatSlackStatuses(schedule.slack_notification_events)}`}
                  >
                    <Bell className="h-3.5 w-3.5 text-emerald-600" aria-label="Slack alerts on" />
                    <span className="font-mono text-muted-foreground">
                      {schedule.slack_channel || 'default'}
                    </span>
                  </span>
                ) : (
                  <span
                    className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
                    title="Slack alerts disabled"
                  >
                    <BellOff className="h-3.5 w-3.5" aria-label="Slack alerts off" />
                    off
                  </span>
                )}
              </TableCell>
              <TableCell>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="icon">
                      <MoreVertical className="h-4 w-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onClick={() => onRunNow?.(schedule)}>
                      <PlayCircle className="mr-2 h-4 w-4" />
                      Run now
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => onEdit?.(schedule)}>
                      <Pencil className="mr-2 h-4 w-4" />
                      Edit schedule
                    </DropdownMenuItem>
                    {schedule.suspended ? (
                      <DropdownMenuItem onClick={() => onResume?.(schedule.name)}>
                        <Play className="mr-2 h-4 w-4" />
                        Resume
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem onClick={() => onSuspend?.(schedule.name)}>
                        <Pause className="mr-2 h-4 w-4" />
                        Suspend
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuItem
                      onClick={() =>
                        onSetSlackNotifications?.(
                          schedule,
                          !schedule.slack_notifications_enabled
                        )
                      }
                    >
                      {schedule.slack_notifications_enabled ? (
                        <BellOff className="mr-2 h-4 w-4" />
                      ) : (
                        <Bell className="mr-2 h-4 w-4" />
                      )}
                      {schedule.slack_notifications_enabled
                        ? 'Disable Slack alerts'
                        : 'Enable Slack alerts'}
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => onEditSlackConfig?.(schedule)}>
                      <Pencil className="mr-2 h-4 w-4" />
                      Edit Slack settings
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onClick={() => onDelete?.(schedule.name)}
                      className="text-destructive"
                    >
                      <Trash2 className="mr-2 h-4 w-4" />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
