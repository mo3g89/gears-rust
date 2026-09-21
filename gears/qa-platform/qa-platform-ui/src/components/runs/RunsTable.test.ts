// @vitest-environment jsdom
//
// Task 10 review, finding 1 (Critical): the succeeded-with-skips marker's own
// condition compared `run.phase` against `'Succeeded'`, but `WorkflowRun.phase`
// is qa-runs' lowercase state set — `runFromDto` sets it to `dto.state`
// "without re-casing" (`adapters.ts:378-383`, decision X4). So the marker was
// dead code: it never rendered for any real run, including the exact
// "succeeded, 68 skipped" case Task 10 exists to make visible. This suite
// renders the actual table with a lowercase-`'succeeded'` fixture and asserts
// the marker text is present, so a regression back to Title Case fails here
// rather than only in a manual check.
//
// `.test.ts`, not `.test.tsx`, matching `hooks.test.ts`'s and
// `useRunLogStream.test.ts`'s existing pattern of using `createElement`
// instead of JSX in a test file — `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so a `.tsx` test would silently not run at all.
import { createElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { RunsTable } from './RunsTable';
import type { WorkflowRun } from '@/api/types';

// This suite is the first in the repo to use `render` (rather than
// `renderHook`), so nothing elsewhere unmounts between tests automatically -
// without this, the second test's query would still see the first test's
// markup still sitting in the shared jsdom document.
afterEach(cleanup);

function run(overrides: Partial<WorkflowRun>): WorkflowRun {
  return {
    name: 'smoke-1',
    plan_id: 'plan-1',
    phase: 'succeeded',
    started_at: '2026-08-28T10:00:00Z',
    finished_at: '2026-08-28T10:02:00Z',
    duration: '2m',
    message: null,
    app_version: null,
    app_build: null,
    test_version: null,
    platform: null,
    product_key: null,
    ...overrides,
  };
}

function renderTable(runs: WorkflowRun[]) {
  render(createElement(MemoryRouter, null, createElement(RunsTable, { runs })));
}

describe('RunsTable — the succeeded-with-skips marker', () => {
  it('renders for a run whose real (lowercase) phase is succeeded and skipped > 0', () => {
    renderTable([
      run({ phase: 'succeeded', result: { passed: 10, failed: 0, skipped: 5, in_progress: 0, xfail: 0, xpass: 0, total: 15 } }),
    ]);
    expect(screen.queryByText('5 skipped')).not.toBeNull();
  });

  it('does not render for a succeeded run with no skips', () => {
    renderTable([
      run({ phase: 'succeeded', result: { passed: 10, failed: 0, skipped: 0, in_progress: 0, xfail: 0, xpass: 0, total: 10 } }),
    ]);
    expect(screen.queryByText(/skipped$/)).toBeNull();
  });

  it('does not render for a failed run, even with a non-zero skip count', () => {
    // A skip alongside a failure is a `Failed` verdict on its own merits
    // (`counts.failed > 0`); the marker is specifically for a run that reads
    // `succeeded` while having skipped something, not a general skip counter.
    renderTable([
      run({ phase: 'failed', result: { passed: 9, failed: 1, skipped: 5, in_progress: 0, xfail: 0, xpass: 0, total: 15 } }),
    ]);
    expect(screen.queryByText(/skipped$/)).toBeNull();
  });
});

describe('RunsTable — the expected-failure and unexpected-pass buckets', () => {
  // The defect this closes: XFAIL and XPASS used to reach `total` and no
  // bucket, so the row read `15/10/0/0/4` against a total of 15 and the
  // missing ones were unaccounted for. XFAIL got a bucket first and XPASS did
  // not, which left the same hole on a suite containing an unexpected pass.
  // The tooltip is the assertion because it names every bucket
  // unconditionally, where the inline numbers are rendered only when non-zero.
  it('names both counts in the breakdown, and the buckets sum to the total', () => {
    const result = { passed: 10, failed: 0, skipped: 3, in_progress: 0, xfail: 1, xpass: 1, total: 15 };
    expect(
      result.passed +
        result.failed +
        result.skipped +
        result.in_progress +
        result.xfail +
        result.xpass,
    ).toBe(result.total);

    renderTable([run({ phase: 'succeeded', result })]);

    const summary = screen.getByTitle(/Expected fail 1 · Unexpected pass 1/);
    expect(summary.textContent).toBe('15/10/0/0/3/1/1');
  });

  // The case that used to leave the row short: an unexpected pass and nothing
  // else unusual. Before XPASS had a bucket this rendered `11/10/0/0/0`
  // against a total of 11.
  it('accounts for an unexpected pass on its own', () => {
    const result = { passed: 10, failed: 0, skipped: 0, in_progress: 0, xfail: 0, xpass: 1, total: 11 };
    expect(
      result.passed +
        result.failed +
        result.skipped +
        result.in_progress +
        result.xfail +
        result.xpass,
    ).toBe(result.total);

    renderTable([run({ phase: 'succeeded', result })]);

    const summary = screen.getByTitle(/Unexpected pass 1/);
    expect(summary.textContent).toBe('11/10/0/0/0/1');
  });

  it('omits both numbers from the row when they are zero', () => {
    renderTable([
      run({
        phase: 'succeeded',
        result: { passed: 10, failed: 0, skipped: 0, in_progress: 0, xfail: 0, xpass: 0, total: 10 },
      }),
    ]);
    // Still named in the breakdown, so "zero expected failures" stays legible,
    // but they add no sixth and seventh number to the row itself.
    const summary = screen.getByTitle(/Expected fail 0 · Unexpected pass 0/);
    expect(summary.textContent).toBe('10/10/0/0/0');
  });
});
