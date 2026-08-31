// The precedence between the two observation columns is the thing worth pinning.
// `record_observation` stamps `version_detected_at` on EVERY attempt and clears
// `version_detect_error` only on success (`platforms_sea_repo.rs:290-364`), so
// reading a set `version_detected_at` as "observed" without first checking the
// error would report a failing platform as healthy — and a failed attempt keeps
// the previous `observed_version`, so the stale version would look confirmed.
import { describe, expect, it } from 'vitest';
import {
  clusterDotClass,
  clusterStatusMessage,
  countObservations,
  observationDotClass,
  platformDotClass,
  platformObservation,
} from './platform-observation';
import type { ClusterHealth, NodeCounts, PlatformInfo } from '@/api/types';

function platform(overrides: Partial<PlatformInfo>): PlatformInfo {
  return {
    id: 'p1',
    name: 'sv-test',
    created_at: '2026-08-01T00:00:00Z',
    description: '',
    product_id: null,
    is_default: false,
    version: null,
    build: null,
    namespace: null,
    vhp_base_url: null,
    version_detected_at: null,
    version_detect_error: null,
    default_branch: null,
    cluster: null,
    ...overrides,
  };
}

function counts(overrides: Partial<NodeCounts> = {}): NodeCounts {
  return {
    total: 0, ready: 0,
    control_plane: 0, ready_control_plane: 0,
    worker: 0, ready_worker: 0,
    ...overrides,
  };
}

function cluster(overrides: Partial<ClusterHealth> = {}): ClusterHealth {
  return {
    status: 'Healthy',
    status_message: null,
    nodes: [],
    namespace_count: null,
    counts: counts(),
    checked_at: '2026-08-28T18:27:36Z',
    ...overrides,
  };
}

describe('platformObservation', () => {
  it('is "observed" when an attempt is stamped and no error is stored', () => {
    const observation = platformObservation(
      platform({ version_detected_at: '2026-08-28T10:00:00Z', version: '26.5' })
    );
    expect(observation.state).toBe('observed');
    expect(observation.detail).toContain('succeeded');
  });

  it('is "failed" when an error is stored, even though the attempt is stamped', () => {
    const observation = platformObservation(
      platform({
        version_detected_at: '2026-08-28T10:00:00Z',
        version: '26.5',
        version_detect_error: 'the API server could not be reached',
      })
    );
    expect(observation.state).toBe('failed');
    expect(observation.detail).toContain('the API server could not be reached');
  });

  it('treats a whitespace-only error as no error', () => {
    expect(
      platformObservation(
        platform({ version_detected_at: '2026-08-28T10:00:00Z', version_detect_error: '   ' })
      ).state
    ).toBe('observed');
  });

  it('is "never" for a platform no cycle has reported on', () => {
    const observation = platformObservation(platform({}));
    expect(observation.state).toBe('never');
    expect(observation.detail).toContain('No detection cycle');
  });

  it('still reports a failure that carries no timestamp', () => {
    const observation = platformObservation(platform({ version_detect_error: 'boom' }));
    expect(observation.state).toBe('failed');
    expect(observation.detail).toBe('Last detection attempt failed: boom');
  });

  it('leaves an unparseable timestamp as the raw string rather than "Invalid Date"', () => {
    expect(platformObservation(platform({ version_detected_at: 'not-a-date' })).detail).toBe(
      'Detection succeeded at not-a-date'
    );
  });
});

describe('observationDotClass', () => {
  it('gives each state a distinct dot', () => {
    const classes = (['observed', 'failed', 'never'] as const).map(observationDotClass);
    expect(new Set(classes).size).toBe(3);
    expect(observationDotClass('failed')).toBe('bg-red-500');
  });
});

describe('countObservations', () => {
  it('partitions a list across the three states', () => {
    expect(
      countObservations([
        platform({ id: 'a', version_detected_at: '2026-08-28T10:00:00Z' }),
        platform({ id: 'b', version_detected_at: '2026-08-28T10:00:00Z', version_detect_error: 'x' }),
        platform({ id: 'c' }),
        platform({ id: 'd', version_detected_at: '2026-08-28T11:00:00Z' }),
      ])
    ).toEqual({ total: 4, observed: 2, failed: 1, never: 1 });
  });

  it('counts an empty list as all zeroes', () => {
    expect(countObservations([])).toEqual({ total: 0, observed: 0, failed: 0, never: 0 });
  });
});

describe('clusterDotClass', () => {
  it('uses legacy’s palette, with Degraded and Warning sharing amber', () => {
    expect(clusterDotClass('Healthy')).toBe('bg-emerald-500');
    expect(clusterDotClass('Degraded')).toBe('bg-amber-500');
    expect(clusterDotClass('Warning')).toBe('bg-amber-500');
    expect(clusterDotClass('Unhealthy')).toBe('bg-red-500');
    expect(clusterDotClass('Unreachable')).toBe('bg-red-700');
  });

  it('does not invent a colour for a status it does not know', () => {
    expect(clusterDotClass('Sideways')).toBe('bg-muted-foreground');
  });
});

describe('clusterStatusMessage', () => {
  // The three sentences legacy stores are composed here instead, because
  // they are pure functions of status + counts (spec §4.1).
  it('composes the Degraded sentence from the counts', () => {
    expect(clusterStatusMessage(cluster({ status: 'Degraded', counts: counts({ total: 3, ready: 1 }) })))
      .toBe('1/3 nodes are Ready');
  });
  it('names the zero-node case', () => {
    expect(clusterStatusMessage(cluster({ status: 'Warning', counts: counts({ total: 0, ready: 0 }) })))
      .toBe('Connected, but no nodes were discovered');
  });
  it('names the none-ready case', () => {
    expect(clusterStatusMessage(cluster({ status: 'Unhealthy', counts: counts({ total: 2, ready: 0 }) })))
      .toBe('Connected, but none of the nodes are Ready');
  });
  it('prefers the server’s own message when Unreachable', () => {
    expect(clusterStatusMessage(cluster({ status: 'Unreachable', status_message: 'the API server could not be reached' })))
      .toBe('the API server could not be reached');
  });
  it('says nothing when Healthy', () => {
    expect(clusterStatusMessage(cluster({ status: 'Healthy' }))).toBeNull();
  });
});

describe('platformDotClass', () => {
  it('uses cluster status when health has been checked', () => {
    expect(platformDotClass(platform({ cluster: cluster({ status: 'Healthy' }) }))).toBe('bg-emerald-500');
  });

  // D-CH-6: without this fallback a build with the feature off, or a platform
  // no cycle has reached, would render a manufactured "unhealthy" dot.
  it('falls back to the reachability dot when cluster is null', () => {
    expect(platformDotClass(platform({ cluster: null, version_detected_at: '2026-08-28T10:00:00Z' })))
      .toBe(observationDotClass('observed'));
  });

  // The assertion above is weaker than it looks: `observationDotClass('observed')`,
  // `clusterDotClass('Healthy')` and a hard-coded `'bg-emerald-500'` are all the same
  // string, so it would pass even if the fallback silently routed through
  // `clusterDotClass` instead of `platformObservation`. These two cases pick reachability
  // states whose colour genuinely differs from every `clusterDotClass` output, so a
  // fallback that resolves to the wrong function is the only way to fail them.
  it('falls back to the reachability dot for a failed detection, not a cluster-status colour', () => {
    const failed = platform({
      cluster: null,
      version_detected_at: '2026-08-28T10:00:00Z',
      version_detect_error: 'boom',
    });
    expect(platformDotClass(failed)).toBe(observationDotClass('failed'));
    expect(platformDotClass(failed)).not.toBe(clusterDotClass('Unreachable'));
  });

  it('falls back to the reachability dot for a platform never observed, not a cluster-status colour', () => {
    const never = platform({ cluster: null });
    expect(platformDotClass(never)).toBe(observationDotClass('never'));
    expect(platformDotClass(never)).not.toBe(clusterDotClass('Unreachable'));
  });
});
