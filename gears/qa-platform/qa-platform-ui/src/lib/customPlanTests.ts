import { useMemo } from 'react';
import { CustomPlan, CustomPlanTest, PlanNode, TestPlanInfo } from '@/api/types';

/** The subset of `CustomPlan` that `resolveCustomPlanTests` actually reads —
 *  narrow enough to also accept an in-progress editor draft that hasn't been
 *  saved (and so has no `id` / `created_at` yet). `id` is optional for that
 *  reason, but should be passed whenever known (e.g. an already-saved plan)
 *  so a self-referencing `included_plans`/`nodes` entry is guarded the same
 *  way the server guards it — see `resolvePlanRef`. */
export interface CustomPlanTestSource {
  id?: string;
  tests: CustomPlanTest[];
  included_plans?: string[];
  nodes?: PlanNode[];
}

/**
 * Client-side counterpart of the manager's `resolve_plan_id_tests` /
 * `resolve_node_tests` run-time resolution (see
 * `manager/src/routes/custom_plans.rs`): a custom plan's *effective* test
 * set is not just its own `tests` array — a "whole plan" (`included_plans`)
 * entry or a "dependencies" (`nodes`) node can point at another plan, whose
 * tests get pulled in too, and that referenced plan may itself be composed
 * of further `included_plans`/`nodes` (a custom plan nested inside a custom
 * plan) — mirrored recursively here via `resolvePlanRef`.
 *
 * `expanded` is a plan-id dedup set threaded through the whole walk: a plan
 * id is only ever expanded once, so a reference cycle (A includes B includes
 * A) or a diamond (B and C both include D) both resolve correctly — a cycle
 * stops instead of recursing forever, a diamond doesn't double-count D.
 * Results are deduped by `(plan_id, test_file)`. Used purely for display
 * (list/detail counts).
 */
function collectTests(
  source: CustomPlanTestSource,
  standardById: Map<string, TestPlanInfo>,
  customById: Map<string, CustomPlan>,
  expanded: Set<string>,
  push: (test: CustomPlanTest) => void
): void {
  if (source.nodes && source.nodes.length > 0) {
    for (const node of source.nodes) {
      if (node.tests && node.tests.length > 0) {
        for (const test of node.tests) push(test);
      } else if (node.plan) {
        resolvePlanRef(node.plan, standardById, customById, expanded, push);
      }
    }
    return;
  }

  for (const test of source.tests) push(test);
  for (const includedId of source.included_plans ?? []) {
    resolvePlanRef(includedId, standardById, customById, expanded, push);
  }
}

/** Resolve a plan id (standard or custom) referenced by an `included_plans`
 *  entry or a DAG node's `plan` field to its contributed tests — a standard
 *  plan contributes its `test_files`. A custom plan contributes its own
 *  *fully-resolved* set (own `tests` plus its own `included_plans`/`nodes`,
 *  recursively), matching the server's `resolve_plan_id_tests`. `expanded`
 *  is checked/marked up front so a plan already being (or already fully)
 *  expanded elsewhere in this walk is not visited again. */
function resolvePlanRef(
  planId: string,
  standardById: Map<string, TestPlanInfo>,
  customById: Map<string, CustomPlan>,
  expanded: Set<string>,
  push: (test: CustomPlanTest) => void
): void {
  if (expanded.has(planId)) {
    return;
  }
  expanded.add(planId);

  const standard = standardById.get(planId);
  if (standard) {
    for (const testFile of standard.test_files) {
      push({ plan_id: planId, test_file: testFile });
    }
    return;
  }

  const custom = customById.get(planId);
  if (!custom) {
    return;
  }
  collectTests(custom, standardById, customById, expanded, push);
}

/** Resolve a custom plan's effective (deduped) test list for display. */
export function resolveCustomPlanTests(
  plan: CustomPlanTestSource,
  standardPlans: TestPlanInfo[],
  customPlans: CustomPlan[]
): CustomPlanTest[] {
  const standardById = new Map(standardPlans.map((p) => [p.id, p]));
  const customById = new Map(customPlans.map((p) => [p.id, p]));

  const seen = new Set<string>();
  const out: CustomPlanTest[] = [];
  const push = (test: CustomPlanTest) => {
    const key = `${test.plan_id}\u0000${test.test_file}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(test);
    }
  };

  // Seed with the plan's own id (when known) so a self-referencing
  // `included_plans`/`nodes` entry is caught by the same guard as any other
  // cycle, instead of being expanded a second time via `resolvePlanRef`.
  const expanded = new Set<string>(plan.id ? [plan.id] : []);
  collectTests(plan, standardById, customById, expanded, push);
  return out;
}

/** Effective test count for a custom plan — see `resolveCustomPlanTests`. */
export function customPlanTestCount(
  plan: CustomPlanTestSource,
  standardPlans: TestPlanInfo[],
  customPlans: CustomPlan[]
): number {
  return resolveCustomPlanTests(plan, standardPlans, customPlans).length;
}

/**
 * Repos a selected plan (standard or custom) is actually attributed to —
 * shared by `RunCustomPlanDialog` and `CreateScheduleDialog`, which both use
 * this to decide whether a branch must be pinned before submitting (only a
 * genuinely multi-repo custom plan requires one).
 *
 * A standard plan is tied to exactly one repo (`repo_id`). A custom plan's
 * tests can span any of the product's repos — especially a "whole plan" /
 * "dependencies" plan pulling from a different repo than the first one
 * registered — so this resolves its actual effective test set (via
 * `resolveCustomPlanTests`) and takes the union of every contributing repo.
 *
 * `standardPlans` should be a **branch-unscoped** lookup (e.g.
 * `usePlans(undefined, productId)`) — which repo a plan lives in doesn't
 * change per branch, and scoping this to the currently-selected branch would
 * silently drop a plan that doesn't exist on that branch from the
 * attribution, under-counting a genuinely multi-repo plan as single-repo.
 *
 * Returns `[]` while `standardPlans` is still loading, or for an
 * empty/unsaved custom plan — callers should treat that as "not yet known"
 * rather than "single-repo", so a plan isn't briefly gated on (or freed
 * from) a branch requirement during load.
 */
export function useAttributedRepoIds(
  standardPlan: { repo_id?: string | null } | null | undefined,
  customPlan: CustomPlanTestSource | null | undefined,
  standardPlans: TestPlanInfo[] | undefined,
  customPlans: CustomPlan[] | undefined
): string[] {
  return useMemo(() => {
    if (standardPlan?.repo_id) return [standardPlan.repo_id];
    if (customPlan) {
      const allPlans = Array.isArray(standardPlans) ? standardPlans : [];
      const repoIdByPlanId = new Map<string, string>();
      for (const p of allPlans) {
        if (p.repo_id) repoIdByPlanId.set(p.id, p.repo_id);
      }
      const effectiveTests = resolveCustomPlanTests(customPlan, allPlans, customPlans ?? []);
      const ids = new Set<string>();
      for (const test of effectiveTests) {
        const repoId = repoIdByPlanId.get(test.plan_id);
        if (repoId) ids.add(repoId);
      }
      return [...ids];
    }
    return [];
  }, [standardPlan?.repo_id, customPlan, standardPlans, customPlans]);
}
