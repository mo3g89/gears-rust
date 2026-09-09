import { Info } from 'lucide-react';

interface ProductScopeNoticeProps {
  /** `DashboardStats.unattributable_runs` -- always 0 when no product filter is applied,
   *  so this is the payload's own signal that the caveat below is about the *current*
   *  answer rather than a permanent fact about the feature.
   *
   *  **Not "runs of the selected product that were dropped".** It counts the
   *  custom-plan runs in the page this dashboard read that belong to no product
   *  at all -- a run of another product's custom plan is in here too, because
   *  nothing can tell the two apart. The copy below is worded to match. */
  unattributableRuns: number;
}

/**
 * The disclosed half of the dashboard's product filter, on the page rather
 * than only in the `GET /qa/v1/dashboard` description.
 *
 * Every other run is attributed to a product through its target's repository
 * -- the same rule `productScope.ts` states for the Runs list -- but a
 * custom-plan run has no single repository to test, and **this gear** has no
 * cross-gear read that could expand one into the repositories its tests span
 * (`domain::service::dashboard::DashboardService::stats`'s `# product_id`
 * section records why). So a custom-plan run is absent from every
 * product-scoped number here even though it is present on the deployment-wide
 * dashboard.
 *
 * **The copy says "this gear cannot", not "custom plans cannot be attributed".**
 * The earlier wording -- *"Custom plans span more than one repository, so they
 * cannot be attributed to the selected product"* -- was a general claim about
 * custom plans, and the Runs list one click away disproves it: `productScope.ts`
 * does attribute a custom-plan row, through the repositories its *tests* name,
 * because the browser can read the plan definitions this gear has no port for.
 * Two surfaces of the same product disagreeing is the divergence this work
 * exists to end; saying which side cannot do it is what makes them consistent.
 *
 * **And the count is not "your product's dropped runs".** `unattributable_runs`
 * counts every custom-plan run in the page the dashboard read, whichever
 * product it would belong to -- there is no product to compare it against, which
 * is the whole point. It is `0` on an unscoped request only because nothing was
 * dropped there, not because the runs stop existing. So the copy says these runs
 * are counted under *no* product rather than excluded from *this* one.
 *
 * **Rendered only when `unattributableRuns` is non-zero.** `DashboardPage`
 * mounts exclusively inside `RequireProduct`, which blocks until a product is
 * selected -- so a prop keyed on "is a product selected" is always `true`
 * there and this notice would render permanently, which is worse than not
 * having it: a caveat nobody can ever see turned off stops being read. Keying
 * it on the actual count the current request dropped means it appears only
 * when it is about something, and can name how many.
 */
export function ProductScopeNotice({ unattributableRuns }: ProductScopeNoticeProps) {
  if (unattributableRuns <= 0) return null;

  const runWord = unattributableRuns === 1 ? 'run' : 'runs';

  return (
    <div className="flex items-start gap-2 rounded-md border border-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
      <Info className="h-4 w-4 mt-0.5 shrink-0" aria-hidden="true" />
      <p>
        {unattributableRuns} custom plan {runWord} counted under no product — not
        under this one and not under any other. This gear can't tell which
        product a custom plan belongs to: it has no read into the plan's tests,
        so there is no repository to attribute it through. The Test Runs list
        can, and does — which is why a custom-plan run can appear there and be
        missing from these figures.
      </p>
    </div>
  );
}
