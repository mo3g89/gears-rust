// @vitest-environment jsdom
//
// UI casing fix: `styleFor` compared `phase` against Title-Case strings
// ('Succeeded', 'Running', ...) but `WorkflowRun.phase` is qa-runs' own
// lowercase `RunState` set — `runFromDto` sets it to `dto.state` "without
// re-casing" (`adapters.ts`, decision X4; `RunState::as_str`,
// qa-runs-sdk/src/models.rs:271-284). Every real run therefore fell through
// to the `default` branch and never got its intended per-state colour. This
// suite renders the actual badge against real lowercase phase values and
// asserts both the visible label and the state-specific dot colour, so a
// regression back to Title Case fails here.
//
// `.test.ts`, not `.test.tsx` — `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so `createElement` stands in for JSX exactly as
// `RunsTable.test.ts` and `RunDetailPage.test.ts` already do.
import { createElement } from 'react';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { RunPhaseBadge } from './RunPhaseBadge';

afterEach(cleanup);

describe('RunPhaseBadge — real (lowercase) RunState values', () => {
  it('renders the emerald "Succeeded" variant for phase "succeeded"', () => {
    render(createElement(RunPhaseBadge, { phase: 'succeeded' }));
    const badge = screen.getByText('Succeeded');
    expect(badge.className).toContain('text-emerald-700');
  });

  it('renders the blue, pulsing "Running" variant for phase "running"', () => {
    render(createElement(RunPhaseBadge, { phase: 'running' }));
    const badge = screen.getByText('Running');
    expect(badge.className).toContain('text-blue-700');
  });

  it('renders the red "Failed" variant for phase "failed"', () => {
    render(createElement(RunPhaseBadge, { phase: 'failed' }));
    const badge = screen.getByText('Failed');
    expect(badge.className).toContain('text-red-700');
  });

  it('does not fall through to the muted default for a real terminal phase', () => {
    render(createElement(RunPhaseBadge, { phase: 'succeeded' }));
    const badge = screen.getByText('Succeeded');
    expect(badge.className).not.toContain('text-muted-foreground');
  });
});
