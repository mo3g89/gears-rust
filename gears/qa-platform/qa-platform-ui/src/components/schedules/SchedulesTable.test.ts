// The Plan column used to render `schedule.plan_name || schedule.plan_id`, and
// `plan_name` is unconditionally `null` (`adapters.ts:810`) because `ScheduleDto`
// carries no plan name — so every row showed the opaque encoded id
// `{repo_uuid}--{base64(path)}`. `SchedulesTable` now joins the id against the
// product-unscoped plan and custom-plan listings the way legacy's backend does
// (`manager/src/routes/schedules.rs:227-229`), and falls back to this helper for
// the ids that join misses.
//
// `.test.ts`, not `.test.tsx`: `vitest.config.ts`'s `include` glob is
// `src/**/*.test.ts` only, so a `.tsx` test would silently not run.
import { describe, expect, it } from 'vitest';
import { planLabelFromId } from './SchedulesTable';
import { encodePlanId } from '@/api/adapters';

const REPO = 'e563f683-3b1c-4d05-b20f-83dae5ce2dfd';

describe('planLabelFromId', () => {
  it('names an encoded repo plan by its file base name', () => {
    expect(planLabelFromId(encodePlanId(REPO, 'plans/monitoring/grafana.yaml'))).toBe('grafana');
  });

  it('strips both yaml spellings and keeps a plan with no directory', () => {
    expect(planLabelFromId(encodePlanId(REPO, 'smoke.yml'))).toBe('smoke');
    expect(planLabelFromId(encodePlanId(REPO, 'plans/smoke.yaml'))).toBe('smoke');
  });

  it('keeps a base name that is not a yaml file intact', () => {
    expect(planLabelFromId(encodePlanId(REPO, 'plans/nightly'))).toBe('nightly');
  });

  it('returns a custom-plan uuid unchanged — it does not decode to a path', () => {
    const uuid = '35119e70-f070-4b7c-9efa-83d19df1b89c';
    expect(planLabelFromId(uuid)).toBe(uuid);
  });

  it('returns an id whose encoded path is empty unchanged, rather than an empty label', () => {
    const empty = encodePlanId(REPO, '');
    expect(planLabelFromId(empty)).toBe(empty);
  });

  it('returns an id whose path is only a yaml extension unchanged', () => {
    const bare = encodePlanId(REPO, 'plans/.yaml');
    expect(planLabelFromId(bare)).toBe(bare);
  });
});
