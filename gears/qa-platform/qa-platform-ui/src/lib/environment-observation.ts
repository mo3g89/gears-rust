import { EnvironmentInfo } from '@/api/types';

/**
 * What the environment-observation cycle says about one environment.
 *
 * The three states are read off the two columns `record_observation` writes
 * (`qa-environments/src/infra/storage/environments_sea_repo.rs:305-518`), and the
 * distinction between them matters:
 *
 *  - `version_detected_at` is stamped on **every attempt**, successful or not — both
 *    branches of that match set it. It is "when we last looked", not "when we last
 *    succeeded".
 *  - `version_detect_error` is cleared to NULL on a success and set on a failure, so it
 *    alone says whether the last look worked.
 *  - A failed attempt deliberately leaves `observed_version`, `observed_build` and
 *    its observed attributes untouched ("stale-but-known beats blank"), so a `failed` environment can
 *    still show a version — one that is no longer being confirmed. That is why `failed`
 *    is a distinct state and not simply "no version".
 *
 * This is reachability and version detection. It was **not** cluster health when this
 * comment was first written: nothing in this deployment read nodes, pods or namespaces, so
 * there was no healthy/degraded/unhealthy distinction to draw and none was invented here.
 *
 * That changed at Task 5 and changed again at Task 19: `EnvironmentInfo.health_state` carries a
 * plugin health verdict. `healthDotClass` and `healthLabel` below are
 * the cluster-health half this file's original doc said did not exist; `environmentObservation`
 * above is unchanged and is still the reachability/version half, used as `healthDotClass`'s
 * fallback for exactly the environment this file's other functions describe: one no detection
 * cycle has reported on at all.
 */
export type EnvironmentObservationState = 'observed' | 'failed' | 'never';

export interface EnvironmentObservation {
  state: EnvironmentObservationState;
  /** Short label for a chip or a table cell. */
  label: string;
  /** Full sentence for a `title` tooltip, including the failure text when there is one. */
  detail: string;
}

function formatTimestamp(value: string | null): string | null {
  if (!value) {
    return null;
  }
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

export function environmentObservation(environment: EnvironmentInfo): EnvironmentObservation {
  const error = environment.version_detect_error?.trim();
  const at = formatTimestamp(environment.version_detected_at);

  if (error) {
    return {
      state: 'failed',
      label: 'Detection failed',
      detail: at
        ? `Last detection attempt failed at ${at}: ${error}`
        : `Last detection attempt failed: ${error}`,
    };
  }

  if (environment.version_detected_at) {
    return {
      state: 'observed',
      label: 'Observed',
      detail: `Detection succeeded at ${at}`,
    };
  }

  return {
    state: 'never',
    label: 'Not yet observed',
    detail: 'No detection cycle has reported on this environment yet',
  };
}

/** Tailwind background class for the state's dot, matching legacy's dot palette. */
export function observationDotClass(state: EnvironmentObservationState): string {
  switch (state) {
    case 'observed':
      return 'bg-emerald-500';
    case 'failed':
      return 'bg-red-500';
    default:
      return 'bg-muted-foreground';
  }
}

export interface ObservationCounts {
  total: number;
  observed: number;
  failed: number;
  never: number;
}

export function countObservations(environments: EnvironmentInfo[]): ObservationCounts {
  const counts: ObservationCounts = { total: environments.length, observed: 0, failed: 0, never: 0 };
  environments.forEach((environment) => {
    counts[environmentObservation(environment).state] += 1;
  });
  return counts;
}



/**
 * The dot beside an environment's name: its plugin's health verdict.
 *
 * **Reads `health_state` since Task 19**, which replaced the five `cluster_*`
 * columns. The old function fell back to `environmentObservation`'s reachability
 * dot whenever `cluster` was null, because a build without the cluster-health
 * feature had no verdict at all. There is no such build now -- every
 * observation goes through the product plugin and every plugin returns a
 * verdict -- and `unknown` is that verdict's honest value for an environment
 * nothing has looked at, so the fallback would only ever mask it.
 */
export function healthDotClass(environment: EnvironmentInfo): string {
  switch (environment.health_state) {
    case 'ok':
      return 'bg-emerald-500';
    case 'degraded':
      return 'bg-amber-500';
    case 'down':
      return 'bg-red-500';
    default:
      return observationDotClass(environmentObservation(environment).state);
  }
}

/**
 * The health text beside the dot, and its tooltip.
 *
 * `health_detail` is the plugin's own CLASSIFIED text (decision D12) -- never a
 * formatted error -- so it is safe to surface verbatim. For an environment
 * nothing has observed, the reachability label says more than "unknown" does.
 */
export function healthLabel(environment: EnvironmentInfo): {
  text: string;
  title: string | undefined;
  bad: boolean;
} {
  const detail = environment.health_detail?.trim() || undefined;
  switch (environment.health_state) {
    case 'ok':
      return { text: 'Healthy', title: detail, bad: false };
    case 'degraded':
      return { text: 'Degraded', title: detail, bad: false };
    case 'down':
      return { text: 'Down', title: detail, bad: true };
    default: {
      const observation = environmentObservation(environment);
      return {
        text: observation.label,
        title: detail ?? observation.detail,
        bad: observation.state === 'failed',
      };
    }
  }
}
