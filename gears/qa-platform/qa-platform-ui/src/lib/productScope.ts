import { decodePlanId } from '@/api/adapters';
import type { CustomPlan } from '@/api/types';
import { resolveCustomPlanTests } from './customPlanTests';

/**
 * Which product does a run or a schedule belong to?
 *
 * **Through its target, never through its environment.** A standard-plan or
 * collect target names a repository, and `TestRepository.product_id` is real
 * (`qa-runs-sdk/src/models.rs:36-90`). The environment is the wrong edge:
 * collect runs have none at all — qa-runs' `launch.rs:1387` says "Collection
 * runs never target a real platform" — so an environment-based rule would
 * hide every collect run the moment a product was selected, including from
 * the chip that is the only way to reach them.
 *
 * **A custom plan carries no product key at all** — not in qa-catalog's
 * model, its DTO, or its stored entity (`CustomPlanDto` has no such field;
 * `customPlanFromDto` sets `CustomPlan.product_id` to `null` unconditionally).
 * Its product is resolved instead through the repositories its *effective*
 * test set names — `resolveCustomPlanTests` (`customPlanTests.ts`), the same
 * expansion `useAttributedRepoIds` already uses.
 *
 * **The standard-plan lookup that expansion can take is deliberately not
 * threaded here, and that is a deletion rather than an omission.** On a plan
 * this resolver can ever see, `included_plans`/`nodes` are always absent —
 * `customPlanFromDto` builds a `CustomPlan` from `CustomPlanDto`, which
 * carries only `files`, and `UpsertCustomPlanReq` has no field for either, so
 * neither round-trips. The expansion therefore never reaches a plan
 * reference, and the standard-plan list it would need there was dead weight:
 * supplying it cost `useRuns` and `useSchedules` a `usePlans(undefined, null)`
 * each — a `GET /plans?repo_id=…` fan-out across *every repository in the
 * deployment*, on every page load, for a branch that cannot be taken.
 * Verified by mutation before removal: hard-coding the list empty left all
 * 263 UI tests passing, because every one of them already passed it empty.
 * The custom-plan list stays threaded — it is the same list this module
 * already holds to find the row's own plan, so a nested custom plan would
 * still expand if one ever did round-trip.
 *
 * Every test's `plan_id` decodes to a `repo_id` (`decodePlanId`); if every
 * decodable test agrees on one repository's product, that product is the
 * answer. Otherwise — no tests, every test's repository deleted, or tests
 * spanning two products' repositories — the plan is unresolvable.
 *
 * `null` means "cannot be attributed", which the caller hides (spec D4).
 * That covers a deleted repository or custom plan, a custom plan with no
 * resolvable tests, and a custom plan whose tests name two different
 * products' repositories (ambiguous — asserting either product would be a
 * false claim, and asserting both would break the one-product-per-row
 * invariant this module exists to establish). A row whose *environment* was
 * deleted still resolves fine.
 */

/** The shape both `WorkflowRun` and `ScheduleInfo` satisfy. */
export interface ProductScopedRow {
  repo_id?: string | null;
  plan_id?: string | null;
  run_kind?: string | null;
}

interface HasProduct {
  id: string;
  product_id: string | null;
}

/** Every decodable test's repository must agree on one product; no tests, no
 *  decodable/resolvable repository at all, or a disagreement, all answer
 *  `null` (spec D4 extended to "ambiguous" — see the file doc above). */
export function productIdOfCustomPlan(
  plan: CustomPlan,
  repos: readonly HasProduct[],
  customPlans: readonly CustomPlan[]
): string | null {
  const productByRepo = new Map(repos.map((r) => [r.id, r.product_id]));
  // No standard plans: see the file doc — the branch that would read them
  // cannot be taken by a plan that came back from the gear.
  const tests = resolveCustomPlanTests(plan, [], [...customPlans]);

  let agreed: string | null = null;
  for (const test of tests) {
    const repoId = decodePlanId(test.plan_id)?.repo_id;
    if (!repoId) continue;
    const productId = productByRepo.get(repoId);
    if (productId == null) continue; // repository deleted, or unknown to this caller
    if (agreed === null) {
      agreed = productId;
    } else if (agreed !== productId) {
      return null; // spans two products' repositories: ambiguous, hidden
    }
  }
  return agreed;
}

export function productIdOfRow(
  row: ProductScopedRow,
  repos: readonly HasProduct[],
  customPlans: readonly CustomPlan[]
): string | null {
  // **`repo_id` is tested first, and the order is what prevents a divergence
  // the id format cannot.** A target carrying *both* a `repo_id` and a
  // `plan_id` would be attributed here through the repository, while the
  // server's `run_repo_id` (`qa-insights`' `dashboard.rs`) matches on the
  // `RunTarget` variant and answers `None` for a `CustomPlan` — so a row the
  // client counted for a product would be one the dashboard did not. No
  // target this UI can build carries both (`planIdFromTarget`,
  // `adapters.ts:339`, fills exactly one), and the two ids are not
  // distinguishable by shape — a custom plan's id and a repository id are
  // both plain UUIDs — so nothing about the *format* rules the collision out.
  // This ordering does, by making the repository the answer whenever there is
  // one, which is the same edge the server reads for every variant that has
  // it. Reversing these two branches would reintroduce the divergence.
  if (row.repo_id) {
    return repos.find((r) => r.id === row.repo_id)?.product_id ?? null;
  }
  // A custom plan spans repositories, so it carries no `repo_id`; its own id is
  // what `planIdFromTarget` puts in `plan_id` (`adapters.ts:339`).
  if (row.plan_id) {
    const plan = customPlans.find((p) => p.id === row.plan_id);
    return plan ? productIdOfCustomPlan(plan, repos, customPlans) : null;
  }
  return null;
}

/**
 * Keep only the rows belonging to `productId`. A `null` `productId` means no
 * product is selected yet and the list is returned untouched — the switcher
 * auto-selects on load, so this is a first-render state, not a mode.
 */
export function keepForProduct<T extends ProductScopedRow>(
  rows: readonly T[],
  productId: string | null | undefined,
  repos: readonly HasProduct[],
  customPlans: readonly CustomPlan[]
): T[] {
  if (!productId) return [...rows];
  return rows.filter((row) => productIdOfRow(row, repos, customPlans) === productId);
}

/**
 * Keep only the custom plans belonging to `productId`, resolved exactly as a
 * custom-plan *run* is — through the repositories the plan's tests name.
 *
 * The Plans page's two tabs are the reason this exists: "Standard Plans" is
 * product-scoped on the server (`usePlans` passes `product_id`) and "Custom
 * Plans" sat beside it listing the whole deployment, so one page gave the
 * switcher two meanings. Spec §4's invariant is over *every* list surface.
 *
 * D4's hide applies here too, and is the same rule the Runs and Schedules
 * lists follow: a plan with no resolvable tests, or with tests spanning two
 * products, is listed under no product.
 */
export function keepPlansForProduct(
  plans: readonly CustomPlan[],
  productId: string | null | undefined,
  repos: readonly HasProduct[]
): CustomPlan[] {
  if (!productId) return [...plans];
  return plans.filter((plan) => productIdOfCustomPlan(plan, repos, plans) === productId);
}
