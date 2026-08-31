import { ClusterHealth, PlatformInfo } from '@/api/types';

/**
 * What the platform-observation cycle says about one platform.
 *
 * The three states are read off the two columns `record_observation` writes
 * (`qa-environments/src/infra/storage/platforms_sea_repo.rs:290-364`), and the
 * distinction between them matters:
 *
 *  - `version_detected_at` is stamped on **every attempt**, successful or not — both
 *    branches of that match set it. It is "when we last looked", not "when we last
 *    succeeded".
 *  - `version_detect_error` is cleared to NULL on a success and set on a failure, so it
 *    alone says whether the last look worked.
 *  - A failed attempt deliberately leaves `observed_version`, `observed_build` and
 *    `vhp_base_url` untouched ("stale-but-known beats blank"), so a `failed` platform can
 *    still show a version — one that is no longer being confirmed. That is why `failed`
 *    is a distinct state and not simply "no version".
 *
 * This is reachability and version detection. It was **not** cluster health when this
 * comment was first written: nothing in this deployment read nodes, pods or namespaces, so
 * there was no healthy/degraded/unhealthy distinction to draw and none was invented here.
 *
 * That has since changed (qa-environments Task 5): `PlatformInfo.cluster` now carries a
 * real `ClusterHealth` reading — or `null` when no observation cycle has reached this
 * platform yet. `clusterDotClass`, `clusterStatusMessage` and `platformDotClass` below are
 * the cluster-health half this file's original doc said did not exist; `platformObservation`
 * above is unchanged and is still the reachability/version half, used as `platformDotClass`'s
 * fallback for exactly the platform this file's other functions describe: one no detection
 * cycle has reported on at all.
 */
export type PlatformObservationState = 'observed' | 'failed' | 'never';

export interface PlatformObservation {
  state: PlatformObservationState;
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

export function platformObservation(platform: PlatformInfo): PlatformObservation {
  const error = platform.version_detect_error?.trim();
  const at = formatTimestamp(platform.version_detected_at);

  if (error) {
    return {
      state: 'failed',
      label: 'Detection failed',
      detail: at
        ? `Last detection attempt failed at ${at}: ${error}`
        : `Last detection attempt failed: ${error}`,
    };
  }

  if (platform.version_detected_at) {
    return {
      state: 'observed',
      label: 'Observed',
      detail: `Detection succeeded at ${at}`,
    };
  }

  return {
    state: 'never',
    label: 'Not yet observed',
    detail: 'No detection cycle has reported on this platform yet',
  };
}

/** Tailwind background class for the state's dot, matching legacy's dot palette. */
export function observationDotClass(state: PlatformObservationState): string {
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

export function countObservations(platforms: PlatformInfo[]): ObservationCounts {
  const counts: ObservationCounts = { total: platforms.length, observed: 0, failed: 0, never: 0 };
  platforms.forEach((platform) => {
    counts[platformObservation(platform).state] += 1;
  });
  return counts;
}

/**
 * Tailwind background class for a cluster-health status dot.
 *
 * Legacy's own palette (`manager/src/services/platforms.rs:140-200` derives the same five
 * buckets): Healthy is green, Unhealthy and Unreachable are both red but Unreachable is the
 * darker shade — a cycle that could not even read the cluster is a worse sign than one that
 * read it and found nodes not Ready — and Degraded/Warning share amber because both mean
 * "reachable, not fully healthy" without legacy drawing a third shade between them. A status
 * this function does not recognise gets the same neutral grey `observationDotClass` gives
 * `never`, rather than a guessed colour for a state nobody has named yet.
 */
export function clusterDotClass(status: string): string {
  switch (status) {
    case 'Healthy':
      return 'bg-emerald-500';
    case 'Degraded':
    case 'Warning':
      return 'bg-amber-500';
    case 'Unhealthy':
      return 'bg-red-500';
    case 'Unreachable':
      return 'bg-red-700';
    default:
      return 'bg-muted-foreground';
  }
}

/**
 * The human sentence for a cluster-health reading, or `null` when there is nothing to say
 * (`Healthy`).
 *
 * Legacy stored a status message column for every status; this design deliberately does
 * not (`ClusterHealthDto.status_message`'s own doc: set only for `"Unreachable"`, "never a
 * formatted `kube::Error`"). For every other non-Healthy status the sentence is a **pure
 * function of `status` + `counts`** — "1/3 nodes are Ready" needs nothing the server did
 * not already compute into `counts` — so composing it here client-side avoids storing (and
 * keeping in sync) three sentences that a UI can derive for free. Only `Unreachable` is
 * irreducible: "the API server could not be reached" is not a fact about node counts, it is
 * the observer's own classified failure text, so that one case alone reads the stored
 * `status_message` instead of composing anything.
 *
 * The zero-node and none-ready cases are named explicitly rather than falling through to
 * `"0/3 nodes are Ready"`-style arithmetic: "no nodes were discovered" and "none of the
 * nodes are Ready" are different findings (an empty cluster view vs. a populated one where
 * every node failed), and a reader should not have to do division to tell them apart.
 */
export function clusterStatusMessage(cluster: ClusterHealth): string | null {
  if (cluster.status === 'Unreachable') {
    return cluster.status_message;
  }
  if (cluster.status === 'Healthy') {
    return null;
  }
  const { total, ready } = cluster.counts;
  if (total === 0) {
    return 'Connected, but no nodes were discovered';
  }
  if (ready === 0) {
    return 'Connected, but none of the nodes are Ready';
  }
  return `${ready}/${total} nodes are Ready`;
}

/**
 * The dot class for one platform row (dashboard strip, platforms table): cluster health
 * when it is available, the reachability dot otherwise.
 *
 * The fallback is not decoration (D-CH-6). `platform.cluster` is `null` for a platform no
 * detection cycle has reached yet — a build with the cluster-health feature off, or simply
 * a platform registered since the last cycle ran. Without the fallback there is no cluster
 * status to read and this function would have to invent one; falling back to
 * `platformObservation`'s reachability dot instead means "we have not checked cluster
 * health yet" renders as whatever reachability already knows (observed / failed / never),
 * not as a manufactured "unhealthy".
 */
export function platformDotClass(platform: PlatformInfo): string {
  if (platform.cluster) {
    return clusterDotClass(platform.cluster.status);
  }
  return observationDotClass(platformObservation(platform).state);
}
