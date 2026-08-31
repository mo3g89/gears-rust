import { Link } from 'react-router-dom';
import { usePlatforms } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { ServerCog } from 'lucide-react';
import { cn } from '@/lib/utils';
import { platformDotClass, platformObservation } from '@/lib/platform-observation';

// `usePlatforms` scopes to the selected product (and to platforms with no product), so
// an empty list here is "none for this product", not "none at all" — and it is also
// what shows for the moment before a product is selected. One string, not two: it used
// to be typed out separately in the description and the body and could drift.
const EMPTY_MESSAGE = 'No platforms configured for the selected product.';

/**
 * The dashboard's platform strip.
 *
 * It was a labelled-unavailable card while nothing in this deployment observed the
 * cluster behind a platform, then a reachability-only chip once `record_observation`
 * started writing detected version/build/namespace/base URL and whether the last attempt
 * reached the cluster at all. qa-environments now observes cluster health itself (Task 5),
 * so the dot is `platformDotClass`: cluster status (Healthy/Degraded/Unhealthy/Warning/
 * Unreachable) when a cycle has reached the platform, falling back to the reachability dot
 * for the platform a cycle has not reached yet — see that function's own doc for why the
 * fallback matters (D-CH-6).
 *
 * The count line partitions every platform into one of five buckets: the four cluster
 * statuses plus "not yet checked" for `cluster === null`, mirroring legacy's own summary
 * (`manager/src/services/platforms.rs:140-200`) now that there is a real per-node source
 * to draw it from again.
 *
 * There is no `platforms_summary` on the dashboard response to read this from — the gear
 * serves no such field — so the card asks `/qa/v1/platforms` itself. That query is already
 * in the cache on most navigations (`SchedulesTable` and the platforms page share it).
 */
export function PlatformsStrip() {
  const { data: platforms, isLoading, error } = usePlatforms();
  const rows = platforms || [];

  const clusterCounts = rows.reduce(
    (acc, platform) => {
      const { cluster } = platform;
      if (!cluster) {
        acc.notChecked += 1;
        return acc;
      }
      if (cluster.status === 'Healthy') acc.healthy += 1;
      else if (cluster.status === 'Unhealthy') acc.unhealthy += 1;
      else if (cluster.status === 'Unreachable') acc.unreachable += 1;
      // `Warning` (and anything this line does not otherwise recognise) folds into
      // `degraded`, mirroring `clusterDotClass`'s shared amber dot for the two — a
      // *checked* platform must never land in `notChecked` just because its status
      // string is not one of the four named buckets.
      else acc.degraded += 1;
      return acc;
    },
    { healthy: 0, degraded: 0, unhealthy: 0, unreachable: 0, notChecked: 0 }
  );

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ServerCog className="h-4 w-4 text-blue-500" />
          Platforms
        </CardTitle>
        <CardDescription>
          {rows.length === 0
            ? EMPTY_MESSAGE
            : `${clusterCounts.healthy} healthy · ${clusterCounts.degraded} degraded · ${clusterCounts.unhealthy} unhealthy · ${clusterCounts.unreachable} unreachable · ${clusterCounts.notChecked} not yet checked`}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <p className="py-2 text-sm text-muted-foreground">Loading platforms…</p>
        ) : error ? (
          <p className="py-2 text-sm text-destructive">Failed to load platforms: {error.message}</p>
        ) : rows.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">{EMPTY_MESSAGE}</p>
        ) : (
          <div className="flex flex-wrap gap-2">
            {rows.map((platform) => {
              const observation = platformObservation(platform);
              // Cluster status when a cycle has reached this platform, the reachability
              // detail otherwise — same fallback `platformDotClass` uses for the dot,
              // kept in sync here so the tooltip never contradicts the colour.
              const statusLabel = platform.cluster ? platform.cluster.status : observation.label;
              const statusDetail = platform.cluster
                ? `${platform.cluster.status}${
                    platform.cluster.status_message ? `: ${platform.cluster.status_message}` : ''
                  }`
                : observation.detail;
              const versionLabel = platform.version
                ? platform.build
                  ? `${platform.version}.${platform.build}`
                  : platform.version
                : null;
              return (
                <Link
                  key={platform.name}
                  to={`/platforms/${encodeURIComponent(platform.name)}`}
                  className="inline-flex items-center gap-2 rounded-md border px-2 py-1 text-xs hover:bg-accent/30"
                  title={`${platform.name} — ${statusDetail}`}
                >
                  <span
                    className={cn('h-2 w-2 rounded-full', platformDotClass(platform))}
                    aria-hidden
                  />
                  <span className="text-foreground/80">{platform.name}</span>
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
