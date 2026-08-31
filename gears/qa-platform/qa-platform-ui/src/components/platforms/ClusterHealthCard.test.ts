// @vitest-environment jsdom
//
// Render coverage for `ClusterHealthCard`. Until this file existed the entire
// UI half of cluster health had none: `PlatformDetailPage.test.ts` explicitly
// disclaims covering it, so two real regressions shipped green —
//
//  1. relaxing `namespace_count === null ? 'not read' : ...` to `|| 'not read'`,
//     which renders a genuinely measured `0` as "not read"; and
//  2. dropping the `Unreachable` count-suppression, which puts "0/0 Ready"
//     beside "Unreachable" and so asserts a measurement that was never taken.
//
// Both are covered below, by name.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so a `.tsx` test silently never runs. That is why
// this file uses `createElement` rather than JSX, following
// `components/runs/RunsTable.test.ts`.
import { createElement } from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { ClusterHealthCard } from './ClusterHealthCard';
import type { ClusterHealth, NodeCounts, NodeSummary } from '@/api/types';

// `render` leaves its markup in the shared jsdom document; without this the
// next test's queries would still see the previous test's card.
afterEach(cleanup);

function counts(overrides: Partial<NodeCounts> = {}): NodeCounts {
  return {
    total: 0,
    ready: 0,
    control_plane: 0,
    ready_control_plane: 0,
    worker: 0,
    ready_worker: 0,
    ...overrides,
  };
}

function node(overrides: Partial<NodeSummary> = {}): NodeSummary {
  return {
    name: 'sv-vhp-jele-io',
    control_plane: true,
    ready: true,
    kubelet_version: 'v1.33.4+k3s1',
    os_image: 'Ubuntu 24.04.3 LTS',
    ...overrides,
  };
}

/** A healthy single-node k3s read — the `sv-test` shape from the design's §7. */
function healthy(overrides: Partial<ClusterHealth> = {}): ClusterHealth {
  return {
    status: 'Healthy',
    status_message: null,
    nodes: [node()],
    namespace_count: 14,
    counts: counts({ total: 1, ready: 1, control_plane: 1, ready_control_plane: 1 }),
    checked_at: '2026-08-28T19:07:36Z',
    ...overrides,
  };
}

/** The D-CH-3 shape: checked, could not be read, so nodes are empty and every
 *  count is a zero that is an artefact rather than a measurement. */
function unreachable(overrides: Partial<ClusterHealth> = {}): ClusterHealth {
  return {
    status: 'Unreachable',
    status_message: 'the API server could not be reached',
    nodes: [],
    namespace_count: null,
    counts: counts(),
    checked_at: '2026-08-28T19:07:36Z',
    ...overrides,
  };
}

function renderCard(cluster: ClusterHealth | null) {
  render(createElement(ClusterHealthCard, { cluster }));
}

describe('ClusterHealthCard — the three states', () => {
  it('renders the never-checked notice when cluster is null, and no status badge', () => {
    renderCard(null);
    expect(screen.queryByText('No cluster health check has run yet')).not.toBeNull();
    // The distinction the whole design turns on: never checked must not render
    // as a status, and above all not as "Unreachable".
    expect(screen.queryByText('Unreachable')).toBeNull();
    expect(screen.queryByText('Healthy')).toBeNull();
    expect(screen.queryByText(/Ready$/)).toBeNull();
  });

  it('renders the status, counts and per-node table for a successful read', () => {
    renderCard(healthy());
    expect(screen.queryByText('Healthy')).not.toBeNull();
    // Two stats read "1/1 Ready" on a single control-plane node (Nodes and
    // Control plane), and "Control plane" is both a stat label and the node's
    // role cell -- hence `queryAllByText`.
    expect(screen.queryAllByText('1/1 Ready')).toHaveLength(2);
    expect(screen.queryByText('sv-vhp-jele-io')).not.toBeNull();
    expect(screen.queryAllByText('Control plane').length).toBeGreaterThan(0);
    expect(screen.queryByText('v1.33.4+k3s1')).not.toBeNull();
    expect(screen.queryByText('Ubuntu 24.04.3 LTS')).not.toBeNull();
    // Healthy carries no status message (the server persists one only for
    // Unreachable, and `clusterStatusMessage` composes none for Healthy).
    expect(screen.queryByText('the API server could not be reached')).toBeNull();
    expect(screen.queryByText('No cluster health check has run yet')).toBeNull();
  });

  it('renders the status and the classified message for an Unreachable read', () => {
    renderCard(unreachable());
    expect(screen.queryByText('Unreachable')).not.toBeNull();
    expect(screen.queryByText('the API server could not be reached')).not.toBeNull();
    expect(screen.queryByText('No cluster health check has run yet')).toBeNull();
  });

  it('shows a zero worker row rather than hiding it (single-node k3s reads 0/0)', () => {
    renderCard(healthy());
    // Design §7: "worker 0 ... is correct, not a bug, and the panel should not
    // hide a zero row." Two stats read "0/0 Ready" on this fixture (workers and
    // — since `counts()` defaults them — nothing else here), so query all.
    expect(screen.queryAllByText('0/0 Ready').length).toBeGreaterThan(0);
    expect(screen.queryByText('Workers')).not.toBeNull();
  });
});

describe('ClusterHealthCard — namespace_count null versus zero', () => {
  it('renders a null namespace_count as "not read"', () => {
    renderCard(healthy({ namespace_count: null }));
    expect(screen.queryByText('not read')).not.toBeNull();
    expect(screen.queryByText('0')).toBeNull();
  });

  it('renders a measured zero as "0", never as "not read"', () => {
    // The regression this pins: `cluster.namespace_count || 'not read'` passes
    // every other test in this file and turns a real measurement of zero into
    // "we could not read it" — two different facts, and the reason the server
    // stores NULL rather than legacy's `unwrap_or(0)`.
    renderCard(healthy({ namespace_count: 0 }));
    expect(screen.queryByText('0')).not.toBeNull();
    expect(screen.queryByText('not read')).toBeNull();
  });

  it('renders a non-zero namespace_count as the number itself', () => {
    renderCard(healthy());
    expect(screen.queryByText('14')).not.toBeNull();
    expect(screen.queryByText('not read')).toBeNull();
  });
});

describe('ClusterHealthCard — Unreachable suppresses the counts', () => {
  it('does not render any count beside an Unreachable status', () => {
    renderCard(unreachable());
    // "0/0 Ready" beside "Unreachable" would state a measurement that was never
    // taken: the zeros are an artefact of the empty node list (D-CH-3), not a
    // reading. Removing the suppression fails exactly here.
    expect(screen.queryAllByText(/Ready$/)).toHaveLength(0);
    expect(screen.queryByText('Nodes')).toBeNull();
    expect(screen.queryByText('Control plane')).toBeNull();
    expect(screen.queryByText('Workers')).toBeNull();
    expect(screen.queryByText('Namespaces')).toBeNull();
    // And the explanation replaces them, rather than the section just vanishing.
    expect(
      screen.queryByText(/Node and namespace counts are not shown/)
    ).not.toBeNull();
  });

  it('suppresses the node table too, even if nodes were somehow present', () => {
    // `nodes` is `[]` for a real Unreachable reading, so a fixture with a node
    // in it is the only way to prove the table is suppressed by the STATUS and
    // not merely by the list being empty.
    renderCard(unreachable({ nodes: [node({ name: 'stale-node' })] }));
    expect(screen.queryByText('stale-node')).toBeNull();
    expect(screen.queryByText('Kubelet')).toBeNull();
  });

  it('still renders the counts for a non-Unreachable status', () => {
    // The mirror: suppression must be conditional, not permanent. A Degraded
    // read is a real measurement and must show its numbers.
    renderCard(
      healthy({
        status: 'Degraded',
        nodes: [node(), node({ name: 'worker-1', control_plane: false, ready: false })],
        counts: counts({
          total: 2,
          ready: 1,
          control_plane: 1,
          ready_control_plane: 1,
          worker: 1,
          ready_worker: 0,
        }),
      })
    );
    expect(screen.queryByText('Degraded')).not.toBeNull();
    expect(screen.queryByText('1/2 Ready')).not.toBeNull();
    // Composed client-side from the status and the counts, not stored.
    expect(screen.queryByText('1/2 nodes are Ready')).not.toBeNull();
    expect(screen.queryByText('worker-1')).not.toBeNull();
    expect(screen.queryByText('Not Ready')).not.toBeNull();
    expect(screen.queryByText(/Node and namespace counts are not shown/)).toBeNull();
  });
});
