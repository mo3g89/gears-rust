import { describe, expect, it } from 'vitest';
import { encodePlanId } from '@/api/adapters';
import type { CustomPlan } from '@/api/types';
import { productIdOfRow, keepForProduct } from './productScope';

const repos = [
  { id: 'repo-a', product_id: 'prod-1' },
  { id: 'repo-b', product_id: 'prod-2' },
];

/** A saved custom plan's own `plan_id` values are always `encodePlanId(repo_id, path)` --
 *  matches `customPlanFromDto`. `id`/`test_file` beyond that are never read by the
 *  resolver, so kept short. */
function customPlan(id: string, repoIds: string[]): CustomPlan {
  return {
    id,
    name: id,
    tests: repoIds.map((repoId, i) => ({ plan_id: encodePlanId(repoId, `plan-${i}.yaml`), test_file: `t${i}.py` })),
    product_id: null,
    created_at: '2026-01-01T00:00:00Z',
  };
}

const customPlans = [customPlan('cp-1', ['repo-b'])];

describe('productIdOfRow', () => {
  it('resolves a plan or test run through its repository', () => {
    expect(productIdOfRow({ repo_id: 'repo-a' }, repos, customPlans)).toBe('prod-1');
  });

  // The case the first design sketch got wrong: a collect run has NO environment
  // (qa-runs' launch.rs:1387), so an environment-based rule hid every one of them.
  // It does name a repository, which is why the target is the right edge.
  it('resolves a collect run through its repository, since it has no environment', () => {
    expect(
      productIdOfRow({ repo_id: 'repo-b', run_kind: 'collect' }, repos, customPlans)
    ).toBe('prod-2');
  });

  it('resolves a custom-plan run through its (only) test’s repository', () => {
    expect(
      productIdOfRow({ repo_id: null, plan_id: 'cp-1', run_kind: 'custom_plan' }, repos, customPlans)
    ).toBe('prod-2');
  });

  it('returns null when the repository is gone', () => {
    expect(productIdOfRow({ repo_id: 'repo-deleted' }, repos, customPlans)).toBeNull();
  });

  it('returns null when nothing identifies a target', () => {
    expect(productIdOfRow({ repo_id: null, plan_id: null }, repos, customPlans)).toBeNull();
  });

  // Round 2 review finding: `CustomPlan.product_id` does not exist in the gear at all
  // (it is always `null` -- see `customPlanFromDto`), so resolution has to go through
  // the plan's *effective* tests instead. These three cover the edges the fix added.
  describe('a custom-plan run, resolved through its tests’ repositories', () => {
    it('is ambiguous -- ans so unresolvable -- when its tests span two products’ repositories', () => {
      const spanning = [customPlan('cp-spanning', ['repo-a', 'repo-b'])];
      expect(
        productIdOfRow({ repo_id: null, plan_id: 'cp-spanning' }, repos, spanning)
      ).toBeNull();
    });

    it('is unresolvable when it has no tests at all', () => {
      const empty = [customPlan('cp-empty', [])];
      expect(productIdOfRow({ repo_id: null, plan_id: 'cp-empty' }, repos, empty)).toBeNull();
    });

    it('is unresolvable when every one of its tests names a since-deleted repository', () => {
      const allDeleted = [customPlan('cp-all-deleted', ['repo-gone-1', 'repo-gone-2'])];
      expect(
        productIdOfRow({ repo_id: null, plan_id: 'cp-all-deleted' }, repos, allDeleted)
      ).toBeNull();
    });

    it('still resolves when only some tests name a deleted repository but the rest agree', () => {
      const mostlyDeleted = [customPlan('cp-mostly-deleted', ['repo-gone', 'repo-b', 'repo-b'])];
      expect(
        productIdOfRow({ repo_id: null, plan_id: 'cp-mostly-deleted' }, repos, mostlyDeleted)
      ).toBe('prod-2');
    });
  });
});

describe('keepForProduct', () => {
  const rows = [
    { id: 'r1', repo_id: 'repo-a' },
    { id: 'r2', repo_id: 'repo-b' },
    { id: 'r3', repo_id: 'repo-deleted' },
  ];

  it('keeps only the selected product and drops the others', () => {
    const kept = keepForProduct(rows, 'prod-1', repos, customPlans);
    expect(kept.map((r) => r.id)).toEqual(['r1']);
  });

  // The assertion that matters: a filter that does nothing passes a
  // "the expected row is present" check, and fails this one.
  it('drops a foreign product row rather than merely including the wanted one', () => {
    const kept = keepForProduct(rows, 'prod-1', repos, customPlans);
    expect(kept.some((r) => r.repo_id === 'repo-b')).toBe(false);
  });

  it('drops an unresolvable row (spec D4: hidden, not shown with a marker)', () => {
    const kept = keepForProduct(rows, 'prod-1', repos, customPlans);
    expect(kept.some((r) => r.repo_id === 'repo-deleted')).toBe(false);
  });

  it('is a no-op when no product is selected', () => {
    expect(keepForProduct(rows, null, repos, customPlans)).toHaveLength(3);
  });
});
