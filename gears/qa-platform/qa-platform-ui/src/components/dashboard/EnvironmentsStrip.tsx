import { Link } from 'react-router-dom';
import { useEnvironments } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { ServerCog } from 'lucide-react';
import { cn } from '@/lib/utils';
import { healthDotClass, healthLabel } from '@/lib/environment-observation';

// `useEnvironments` scopes to the selected product (and to environments with no product), so
// an empty list here is "none for this product", not "none at all" — and it is also
// what shows for the moment before a product is selected. One string, not two: it used
// to be typed out separately in the description and the body and could drift.
const EMPTY_MESSAGE = 'No environments configured for the selected product.';

/**
 * The dashboard's environment strip.
 *
 * It was a labelled-unavailable card while nothing in this deployment observed the
 * cluster behind an environment, then a reachability-only chip once `record_observation`
 * started writing detected version/build/namespace/base URL and whether the last attempt
 * reached the cluster at all. qa-environments now observes cluster health itself (Task 5),
 * so the dot is `healthDotClass`: cluster status (Healthy/Degraded/Unhealthy/Warning/
 * Unreachable) when a cycle has reached the environment, falling back to the reachability dot
 * for the environment a cycle has not reached yet — see that function's own doc for why the
 * fallback matters (D-CH-6).
 *
 * The count line partitions every environment into one of five buckets: the four cluster
 * statuses plus "not yet checked" for `cluster === null`, mirroring legacy's own summary
 * (`manager/src/services/platforms.rs:140-200`) now that there is a real per-node source
 * to draw it from again.
 *
 * There is no `platforms_summary` on the dashboard response to read this from — the gear
 * serves no such field — so the card asks `/qa/v1/environments` itself. That query is already
 * in the cache on most navigations (`SchedulesTable` and the environments page share it).
 */
export function EnvironmentsStrip() {
  const { data: environments, isLoading, error } = useEnvironments();
  const rows = environments || [];

  // **Four buckets, not five, since Task 19.** The plugin contract carries a
  // health verdict with four values, and `unknown` is one of them -- an
  // environment nothing has observed and one whose read failed are both
  // `unknown`, because the plugin cannot distinguish them either. The old
  // `Unreachable` bucket was a fifth *status*; there is no such status now, and
  // inventing one from `version_detect_error` would be the fabrication I-5 was
  // raised about.
  const healthCounts = rows.reduce(
    (acc, environment) => {
      if (environment.health_state === 'ok') acc.healthy += 1;
      else if (environment.health_state === 'degraded') acc.degraded += 1;
      else if (environment.health_state === 'down') acc.down += 1;
      // Anything this line does not recognise folds into `notChecked` with
      // `unknown` itself: a verdict this UI cannot read is not a verdict.
      else acc.notChecked += 1;
      return acc;
    },
    { healthy: 0, degraded: 0, down: 0, notChecked: 0 }
  );

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ServerCog className="h-4 w-4 text-blue-500" />
          Environments
        </CardTitle>
        <CardDescription>
          {rows.length === 0
            ? EMPTY_MESSAGE
            : `${healthCounts.healthy} healthy · ${healthCounts.degraded} degraded · ${healthCounts.down} down · ${healthCounts.notChecked} not yet checked`}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <p className="py-2 text-sm text-muted-foreground">Loading environments…</p>
        ) : error ? (
          <p className="py-2 text-sm text-destructive">Failed to load environments: {error.message}</p>
        ) : rows.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">{EMPTY_MESSAGE}</p>
        ) : (
          <div className="flex flex-wrap gap-2">
            {rows.map((environment) => {
              // The same helper the table uses, so the strip and the table can
              // never disagree about one environment's health.
              const health = healthLabel(environment);
              const statusLabel = health.text;
              const statusDetail = health.title ?? health.text;
              const versionLabel = environment.version
                ? environment.build
                  ? `${environment.version}.${environment.build}`
                  : environment.version
                : null;
              return (
                <Link
                  key={environment.name}
                  to={`/environments/${encodeURIComponent(environment.name)}`}
                  className="inline-flex items-center gap-2 rounded-md border px-2 py-1 text-xs hover:bg-accent/30"
                  title={`${environment.name} — ${statusDetail}`}
                >
                  <span
                    className={cn('h-2 w-2 rounded-full', healthDotClass(environment))}
                    aria-hidden
                  />
                  <span className="text-foreground/80">{environment.name}</span>
                  {versionLabel && (
                    <span className="font-mono text-[10px] text-muted-foreground">
                      {versionLabel}
                    </span>
                  )}
                  <span className="sr-only">{statusLabel}</span>
                </Link>
              );
            })}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
