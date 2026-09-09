// The precedence between the two observation columns is the thing worth pinning.
// `record_observation` stamps `version_detected_at` on EVERY attempt and clears
// `version_detect_error` only on success (`environments_sea_repo.rs:305-518`), so
// reading a set `version_detected_at` as "observed" without first checking the
// error would report a failing environment as healthy — and a failed attempt keeps
// the previous `observed_version`, so the stale version would look confirmed.
import { describe, expect, it } from 'vitest';
import {
  countObservations,
  observationDotClass,
  environmentObservation,
} from './environment-observation';
import type { EnvironmentInfo } from '@/api/types';

function environment(overrides: Partial<EnvironmentInfo>): EnvironmentInfo {
  return {
    id: 'p1',
    name: 'sv-test',
    created_at: '2026-08-01T00:00:00Z',
    description: '',
    product_id: null,
    is_default: false,
    available: true,
    observed_attrs: {},
    health_state: 'unknown',
    health_detail: null,
    version: null,
    build: null,
    version_detected_at: null,
    version_detect_error: null,
    default_branch: null,
    ...overrides,
  };
}



describe('environmentObservation', () => {
  it('is "observed" when an attempt is stamped and no error is stored', () => {
    const observation = environmentObservation(
      environment({ version_detected_at: '2026-08-28T10:00:00Z', version: '26.5' })
    );
    expect(observation.state).toBe('observed');
    expect(observation.detail).toContain('succeeded');
  });

  it('is "failed" when an error is stored, even though the attempt is stamped', () => {
    const observation = environmentObservation(
      environment({
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
      environmentObservation(
        environment({ version_detected_at: '2026-08-28T10:00:00Z', version_detect_error: '   ' })
      ).state
    ).toBe('observed');
  });

  it('is "never" for an environment no cycle has reported on', () => {
    const observation = environmentObservation(environment({}));
    expect(observation.state).toBe('never');
    expect(observation.detail).toContain('No detection cycle');
  });

  it('still reports a failure that carries no timestamp', () => {
    const observation = environmentObservation(environment({ version_detect_error: 'boom' }));
    expect(observation.state).toBe('failed');
    expect(observation.detail).toBe('Last detection attempt failed: boom');
  });

  it('leaves an unparseable timestamp as the raw string rather than "Invalid Date"', () => {
    expect(environmentObservation(environment({ version_detected_at: 'not-a-date' })).detail).toBe(
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
        environment({ id: 'a', version_detected_at: '2026-08-28T10:00:00Z' }),
        environment({ id: 'b', version_detected_at: '2026-08-28T10:00:00Z', version_detect_error: 'x' }),
        environment({ id: 'c' }),
        environment({ id: 'd', version_detected_at: '2026-08-28T11:00:00Z' }),
      ])
    ).toEqual({ total: 4, observed: 2, failed: 1, never: 1 });
  });

  it('counts an empty list as all zeroes', () => {
    expect(countObservations([])).toEqual({ total: 0, observed: 0, failed: 0, never: 0 });
  });
});


