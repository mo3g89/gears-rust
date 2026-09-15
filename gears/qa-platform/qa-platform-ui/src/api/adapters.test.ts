import { describe, expect, it } from 'vitest';

import {
  analyticsOverviewFromDto,
  analyticsPlanId,
  coveragePointsFromDto,
  createEnvironmentReqFromForm,
  dashboardFromDto,
  decodePlanId,
  distinctObservedVersions,
  encodePlanId,
  expandCustomPlanFiles,
  jiraBugFromDto,
  observedFromEnvironments,
  odataLiteral,
  openBugsQuery,
  parseRunLogSse,
  partitionEnvironmentVariables,
  planFromDto,
  environmentFromDto,
  queueEntryFromDto,
  recentResultsQuery,
  repoFromDto,
  launchReqFromForm,
  runFromDto,
  scheduleFromDto,
  scheduleReqFromForm,
  testAnalyticsFromDto,
  testFilesFromPlan,
  testRunResultFromDto,
  unwrapPage,
  updateEnvironmentReqFromForm,
  variableWritePlan,
} from './adapters';

// --- Envelope ------------------------------------------------------------------
//
// The literals here are the gears' real `Page<T>` envelope — `{ items, page_info }`
// (`generated/openapi.d.ts` `PageInfo` + the eight `Page_*` instantiations).
// The plan's Task 10 step list wrote `{ value, count }`, which is OData's envelope
// and appears nowhere in this document; asserting on it would have made the test
// pass against a shape the gears never send.

describe('unwrapPage', () => {
  it('unwraps a Page envelope into the bare array components expect', () => {
    expect(
      unwrapPage({ items: [{ id: 'a' }], page_info: { limit: 1, next_cursor: null, prev_cursor: null } })
    ).toEqual([{ id: 'a' }]);
  });

  it('treats an empty page as an empty array rather than undefined', () => {
    // A component that maps over undefined crashes; over [] it renders "no data".
    expect(unwrapPage({ items: [], page_info: { limit: 0, next_cursor: null, prev_cursor: null } })).toEqual([]);
  });

  it('treats a malformed body with no items as an empty array', () => {
    expect(unwrapPage(undefined)).toEqual([]);
    expect(unwrapPage({} as never)).toEqual([]);
  });
});

// --- Observed environment versions ------------------------------------------------

describe('observedFromEnvironments', () => {
  it('derives observed versions from the environment DTO fields', () => {
    const environments = [
      { id: '1', observed_version: '7.0', observed_build: '7.0.1' },
      { id: '2', observed_version: null, observed_build: null },
    ];
    // Nulls are dropped, not rendered as "null".
    expect(observedFromEnvironments(environments).map((o) => o.version)).toEqual(['7.0']);
  });

  it('keeps the build beside the version without treating it as a branch', () => {
    // CONTRACT-DIFF §5-E: `observed_build` is a build, never a branch. It is carried
    // here so a caller that wants a build label has one, and no branch list is fed
    // from it anywhere.
    expect(observedFromEnvironments([{ id: '1', observed_version: '7.0', observed_build: '7.0.1' }])).toEqual([
      { environment_id: '1', version: '7.0', build: '7.0.1' },
    ]);
  });

  it('keeps one entry per environment, including two environments on the same version', () => {
    const out = observedFromEnvironments([
      { id: 'a', observed_version: '7.1', observed_build: null },
      { id: 'b', observed_version: '7.0', observed_build: null },
      { id: 'c', observed_version: '7.1', observed_build: null },
    ]);
    expect(out.map((o) => o.environment_id)).toEqual(['a', 'b', 'c']);
  });
});

describe('distinctObservedVersions', () => {
  it('dedupes and sorts the versions the dropdown offers', () => {
    expect(
      distinctObservedVersions([
        { id: 'a', observed_version: '7.1', observed_build: null },
        { id: 'b', observed_version: '7.0', observed_build: null },
        { id: 'c', observed_version: '7.1', observed_build: null },
      ])
    ).toEqual(['7.0', '7.1']);
  });

  it('answers an empty list when no environment observes a version', () => {
    // Which is every deployment this plan produces (§9.1). An empty dropdown is a true
    // statement; a hardcoded "unknown" fallback would present a probe value as real data.
    expect(distinctObservedVersions([{ id: 'a', observed_version: null, observed_build: null }])).toEqual([]);
  });
});

// --- OData ---------------------------------------------------------------------

describe('odataLiteral', () => {
  it('doubles an embedded quote rather than producing invalid OData', () => {
    expect(odataLiteral("it's.py")).toBe("'it''s.py'");
  });

  it('leaves a uuid unquoted, because a quoted uuid is a 400', () => {
    // Driven against the live gear during Task 10:
    //   $filter=run_id eq '<uuid>' -> 400 "Type mismatch for field run_id: expected Uuid, got string"
    //   $filter=run_id eq <uuid>   -> 200
    expect(odataLiteral('1a9adedc-83b2-4a63-b353-8cd4b3eb1c9e', 'uuid')).toBe(
      '1a9adedc-83b2-4a63-b353-8cd4b3eb1c9e'
    );
  });
});

describe('recentResultsQuery', () => {
  it('builds an OData query for recent results from file and limit', () => {
    const q = recentResultsQuery({ file: 'tests/a.py', limit: 5 });
    // `limit`, not `$top`: `$top` is declared nowhere on /qa/v1/test-results and is
    // silently ignored (verified live in Task 6 — `?$top=1` and `?$top=0` both
    // returned the full default page). Asserting `$top=5` would pass while the
    // bound had no effect.
    expect(q).toContain('limit=5');
    expect(q).not.toContain('$top');
    expect(q).toContain('tests%2Fa.py');
  });

  it('escapes a quote in the filter value rather than producing invalid OData', () => {
    expect(() => recentResultsQuery({ file: "it's.py", limit: 5 })).not.toThrow();
    // Read the parameter back rather than substring-matching the encoded string: the
    // quote doubling has to survive into the value the gear parses, and `%27%27` in the
    // raw query string is easy to assert on by accident.
    const filter = new URLSearchParams(recentResultsQuery({ file: "it's.py", limit: 5 })).get('$filter');
    expect(filter).toBe("test_file eq 'it''s.py'");
  });

  it('clamps the limit into the range the gear honours', () => {
    expect(recentResultsQuery({ file: 'a.py', limit: 0 })).toContain('limit=1');
    expect(recentResultsQuery({ file: 'a.py', limit: 10_000 })).toContain('limit=500');
  });
});

// --- Plan identity (X6) --------------------------------------------------------

describe('encodePlanId / decodePlanId', () => {
  it('round-trips a (repo_id, path) pair through one URL-safe segment', () => {
    const id = encodePlanId('32656370-6be5-4445-99f3-cfa86195bbb3', 'plans/smoke.yaml');
    expect(id).not.toContain('/');
    expect(id).not.toContain('%');
    expect(decodePlanId(id)).toEqual({
      repo_id: '32656370-6be5-4445-99f3-cfa86195bbb3',
      path: 'plans/smoke.yaml',
    });
  });

  it('round-trips a path with unicode and spaces', () => {
    const id = encodePlanId('r', 'plans/ünïcode plan.yaml');
    expect(decodePlanId(id)).toEqual({ repo_id: 'r', path: 'plans/ünïcode plan.yaml' });
  });

  it('returns null for a string that is not an encoded plan id', () => {
    expect(decodePlanId('')).toBeNull();
    expect(decodePlanId('some-legacy-plan-id')).toBeNull();
  });

  it('encodes an empty path without collapsing to a bare repo id', () => {
    const id = encodePlanId('r', '');
    expect(decodePlanId(id)).toEqual({ repo_id: 'r', path: '' });
  });
});

// --- Runs ----------------------------------------------------------------------

const RUN_DTO = {
  id: 'run-uuid',
  name: 'smoke-2',
  target: { kind: 'plan', repo_id: 'repo-1', path: 'plans/smoke.yaml', test_file: null, custom_plan_id: null },
  environment_id: 'plat-1',
  test_version: 'main',
  app_version: null,
  app_build: null,
  state: 'running',
  resolved_exclusive: true,
  exclusive_tier: 'default',
  is_validation: false,
  parameters: [],
  include_tags: [],
  exclude_tags: [],
  source: 'manual',
  schedule_id: null,
  bundle_ids: [],
  error: 'boom',
  started_at: '2026-08-26T10:00:00Z',
  finished_at: '2026-08-26T10:02:05Z',
  created_at: '2026-08-26T09:59:00Z',
  updated_at: '2026-08-26T10:02:05Z',
} as never;

describe('runFromDto', () => {
  it('maps state onto phase without re-casing it', () => {
    // X4: the gears' set is lowercase and is NOT legacy's capitalised Argo phases.
    // Re-casing would invent a phase legacy understood; `isActiveRun` learns the
    // real set instead.
    expect(runFromDto(RUN_DTO).phase).toBe('running');
  });

  it('folds target{} back into the opaque plan_id components still pass around', () => {
    const run = runFromDto(RUN_DTO);
    expect(decodePlanId(run.plan_id)).toEqual({ repo_id: 'repo-1', path: 'plans/smoke.yaml' });
    expect(run.run_kind).toBe('plan');
  });

  it('renames error to message and environment_id into platform', () => {
    const run = runFromDto(RUN_DTO);
    expect(run.message).toBe('boom');
    expect(run.platform).toBe('plat-1');
  });

  it('derives the duration legacy rendered from started_at and finished_at', () => {
    expect(runFromDto(RUN_DTO).duration).toBe('2m 5s');
  });

  it('leaves duration null while the run has no finish', () => {
    expect(runFromDto({ ...(RUN_DTO as object), finished_at: null } as never).duration).toBeNull();
  });

  it('leaves product_key null rather than filling it', () => {
    // §7.9: the gear sends nothing for this. Bind the field, do not invent a value.
    expect(runFromDto(RUN_DTO).product_key).toBeNull();
  });

  it('reads a custom-plan target through custom_plan_id', () => {
    const run = runFromDto({
      ...(RUN_DTO as object),
      target: { kind: 'custom_plan', custom_plan_id: 'cp-1', repo_id: null, path: null, test_file: null },
    } as never);
    expect(run.plan_id).toBe('cp-1');
    expect(run.run_kind).toBe('custom_plan');
  });

  // Task 10: `RunDto.result` is what lets the run list and detail view show a
  // `succeeded` run's non-zero `skipped` instead of hiding it now that a skip
  // no longer fails the run. Passed straight through, not renamed or dropped.
  it('carries result straight through', () => {
    const result = { passed: 10, failed: 0, skipped: 5, in_progress: 0, total: 15 };
    const run = runFromDto({ ...(RUN_DTO as object), result } as never);
    expect(run.result).toEqual(result);
  });
});

// --- Queue ---------------------------------------------------------------------

describe('queueEntryFromDto', () => {
  it('renames environment_id and run_id back onto the legacy field names', () => {
    const entry = queueEntryFromDto({
      id: 'q1',
      run_id: 'r1',
      environment_id: 'p1',
      run_kind: 'plan',
      source: 'manual',
      exclusive: true,
      state: 'cancelled',
      error: null,
      enqueued_at: 'now',
      dispatched_at: null,
      finished_at: null,
      queue_position: 3,
      ttl_expires_at: null,
      blocked_by: null,
    } as never);
    expect(entry.platform).toBe('p1');
    expect(entry.workflow_name).toBe('r1');
    // X4: `cancelled` with two `l`s on a queue row is deliberate and must NOT be
    // normalised onto the run's one-`l` `canceled`.
    expect(entry.state).toBe('cancelled');
    // `target_id` is a documented substitution, not a passthrough: `QueueEntryDto` has no
    // target of any kind, and this field is the queued row's primary label at
    // `QueuedRunsCard.tsx:144` and in both of its confirm dialogs. The run id stands in —
    // a real identifier for the same row rather than a blank label reading
    // `Cancel queued run ""?`.
    expect(entry.target_id).toBe('r1');
  });
});

// --- Test repositories -----------------------------------------------------------

const REPO_DTO = {
  id: 'repo-1',
  name: 'main-tests',
  url: 'git@example.com:org/repo.git',
  product_id: 'prod-1',
  default_branch: 'main',
  content_root: '',
  has_credential: false,
  last_synced_at: null,
  sync_error: null,
  created_at: '2026-08-01T00:00:00Z',
  updated_at: '2026-08-01T00:00:00Z',
};

describe('repoFromDto', () => {
  it('carries a populated sync_error through with a null last_synced_at (never synced, sync failing)', () => {
    const repo = repoFromDto({
      ...REPO_DTO,
      last_synced_at: null,
      sync_error: "credential 'ae2b5730-…' is not accessible in credstore",
    } as never);
    expect(repo.last_synced_at).toBeNull();
    expect(repo.sync_error).toBe("credential 'ae2b5730-…' is not accessible in credstore");
  });

  it('carries a populated last_synced_at through with a null sync_error (synced, healthy)', () => {
    const repo = repoFromDto({
      ...REPO_DTO,
      last_synced_at: '2026-08-20T12:00:00Z',
      sync_error: null,
    } as never);
    expect(repo.last_synced_at).toBe('2026-08-20T12:00:00Z');
    expect(repo.sync_error).toBeNull();
  });
});

// --- Schedules -----------------------------------------------------------------

const SCHEDULE_DTO = {
  id: 'sched-uuid',
  name: 'nightly',
  cron: '0 2 * * *',
  enabled: false,
  target: { kind: 'plan', repo_id: 'repo-1', path: 'plans/smoke.yaml', test_file: 'tests/a.py' },
  environment_id: 'plat-1',
  branch: 'main',
  include_tags: ['ci', 'smoke'],
  exclude_tags: [],
  exclusive_choice: 'auto',
  parameters: [],
  slack_notifications_enabled: true,
  slack_channel: '#qa',
  slack_notification_events: ['failed'],
  last_fired_tick: '2026-08-26T02:00:00Z',
  created_at: '2026-08-01T00:00:00Z',
  updated_at: '2026-08-26T02:00:00Z',
} as never;

describe('scheduleFromDto', () => {
  it('inverts enabled back into legacy suspended', () => {
    expect(scheduleFromDto(SCHEDULE_DTO).suspended).toBe(true);
    expect(scheduleFromDto({ ...(SCHEDULE_DTO as object), enabled: true } as never).suspended).toBe(false);
  });

  it('joins the tag arrays back into the comma strings the form edits', () => {
    const s = scheduleFromDto(SCHEDULE_DTO);
    expect(s.include_tags).toBe('ci,smoke');
    // An empty array is "no tags", which legacy spelled as null rather than "".
    expect(s.exclude_tags).toBeNull();
  });

  it('maps the three-valued exclusive_choice onto legacy nullable boolean', () => {
    expect(scheduleFromDto(SCHEDULE_DTO).exclusive).toBeNull();
    expect(
      scheduleFromDto({ ...(SCHEDULE_DTO as object), exclusive_choice: 'true' } as never).exclusive
    ).toBe(true);
    expect(
      scheduleFromDto({ ...(SCHEDULE_DTO as object), exclusive_choice: 'false' } as never).exclusive
    ).toBe(false);
  });

  it('carries the gear id in schedule_id so a later write can address the row', () => {
    expect(scheduleFromDto(SCHEDULE_DTO).schedule_id).toBe('sched-uuid');
  });

  it('leaves the absent description-side fields null instead of inventing them', () => {
    const s = scheduleFromDto(SCHEDULE_DTO);
    expect(s.plan_name).toBeNull();
    expect(s.product_key).toBeNull();
    expect(s.product_name).toBeNull();
    expect(s.recent_runs).toEqual([]);
  });
});

describe('scheduleReqFromForm', () => {
  it('splits the comma strings back into arrays and inverts enabled', () => {
    const req = scheduleReqFromForm(
      {
        plan_id: encodePlanId('repo-1', 'plans/smoke.yaml'),
        schedule_id: 'nightly',
        cron_expr: '0 2 * * *',
        include_tags: 'ci, smoke ,',
        exclude_tags: '',
        exclusive: null,
        test_file: 'tests/a.py',
        branch: 'main',
      },
      { platformId: 'plat-1', enabled: true }
    );
    expect(req.include_tags).toEqual(['ci', 'smoke']);
    expect(req.exclude_tags).toEqual([]);
    expect(req.enabled).toBe(true);
    expect(req.exclusive_choice).toBe('auto');
    expect(req.cron).toBe('0 2 * * *');
    // A plan with a test_file is a `test` target, not a `plan` target.
    expect(req.target).toEqual({
      kind: 'test',
      repo_id: 'repo-1',
      path: 'plans/smoke.yaml',
      test_file: 'tests/a.py',
    });
  });

  // Both of the next two assert the SAME correction, on the two functions that carried
  // the same wrong guard. CONTRACT-DIFF row 3 read `Admission::Unqueued` as "never runs";
  // it means "dispatch inline, no queue row" (`launch.rs`'s `Unqueued` arm calls
  // `dispatch_and_report`, and `schedules.rs` says a platformless schedule can "succeed
  // every time"). A null platform is the "Default cluster" choice and must reach the wire.
  it('passes a schedule with no platform through as null rather than refusing it', () => {
    const req = scheduleReqFromForm(
      { plan_id: encodePlanId('r', 'p.yaml'), schedule_id: 's', cron_expr: '* * * * *' },
      { platformId: null, enabled: true }
    );
    expect(req.environment_id).toBeNull();
    // The serialized key is what the gear actually reads; a mutation reverting this
    // field's name back to `platform_id` must fail here, not just at the TS-field level.
    expect(JSON.parse(JSON.stringify(req))).toHaveProperty('environment_id', null);
    expect(JSON.parse(JSON.stringify(req))).not.toHaveProperty('platform_id');
  });

  it('passes a launch with no platform through as null rather than refusing it', () => {
    const req = launchReqFromForm({
      planId: encodePlanId('r', 'p.yaml'),
      platformId: null,
    });
    expect(req.environment_id).toBeNull();
    expect(JSON.parse(JSON.stringify(req))).toHaveProperty('environment_id', null);
    expect(JSON.parse(JSON.stringify(req))).not.toHaveProperty('platform_id');
  });

  it("maps the Default cluster option's empty-string value to a null environment_id", () => {
    // The Combobox option is `{ value: '', label: 'Default cluster' }`, so the empty
    // string — not `null` — is what the dialogs actually hand over.
    const req = launchReqFromForm({
      planId: encodePlanId('r', 'p.yaml'),
      platformId: '',
    });
    expect(req.environment_id).toBeNull();
  });

  it('round-trips a schedule read back into its own replace request', () => {
    // Row 20/21: the PUT is a whole-record replace and `enabled` is required, so a
    // suspend must re-send every field it did not show the user.
    const read = scheduleFromDto(SCHEDULE_DTO);
    const req = scheduleReqFromForm(
      {
        plan_id: read.plan_id,
        schedule_id: read.schedule_id,
        cron_expr: read.schedule,
        branch: read.branch ?? undefined,
        test_file: read.test_file ?? undefined,
        include_tags: read.include_tags ?? undefined,
        exclude_tags: read.exclude_tags ?? undefined,
        exclusive: read.exclusive,
      },
      { platformId: read.platform, enabled: false, name: read.name }
    );
    expect(req.name).toBe('nightly');
    expect(req.cron).toBe('0 2 * * *');
    expect(req.include_tags).toEqual(['ci', 'smoke']);
    expect(req.enabled).toBe(false);
    expect(req.environment_id).toBe('plat-1');
    expect(JSON.parse(JSON.stringify(req))).toHaveProperty('environment_id', 'plat-1');
    expect(JSON.parse(JSON.stringify(req))).not.toHaveProperty('platform_id');
  });
});

// `environmentFromDto` had no test at all until the 2026-08-28 review: this file had 103
// tests and none of them called it, so a regression in the observation mapping would have
// shipped green. What it maps changed at Task 19 -- the five `cluster_*` columns went and
// the plugin's own `observed_attrs` took their place -- and these assert the new shape.
describe('environmentFromDto', () => {
  // The four tests that stood here covered `EnvironmentDto.cluster` -- five
  // `cluster_*` columns Task 19 dropped and user decision U4 declined to
  // replace ("nodes ready" is not health for a SaaS tenant). What took their
  // place is a plugin verdict in `health_state` and a plugin-shaped map in
  // `observed_attrs`, and those are what these assert.
  const dto = {
    id: 'env-1',
    name: 'prod',
    created_at: '2026-09-05T00:00:00Z',
    product_id: 'product-vhp',
    observed_version: '26.5',
    observed_build: '1471',
    observed_attrs: { platformVersion: '26.5', baseDomain: 'https://sv.jele.io' },
    health_state: 'degraded',
    health_detail: 'one node not ready',
    is_default: false,
  } as unknown as Parameters<typeof environmentFromDto>[0];

  it('carries the plugin-shaped observation map through verbatim', () => {
    expect(environmentFromDto(dto).observed_attrs).toEqual({
      platformVersion: '26.5',
      baseDomain: 'https://sv.jele.io',
    });
  });

  it('carries the health verdict and its classified detail', () => {
    const info = environmentFromDto(dto);
    expect(info.health_state).toBe('degraded');
    expect(info.health_detail).toBe('one node not ready');
  });

  it('reads an unobserved environment as an empty map and `unknown`, never undefined', () => {
    // The schema declares `observed_attrs` and `health_state` required — the
    // columns are NOT NULL with defaults, so the gear always sends them. This
    // stays as a defensive assertion about the adapter, not a claim about the
    // wire, which is why the fixture has to go through `unknown` to exist.
    const fresh = { ...dto, observed_attrs: undefined, health_state: undefined,
                    health_detail: undefined } as unknown as typeof dto;
    const info = environmentFromDto(fresh);
    expect(info.observed_attrs).toEqual({});
    expect(info.health_state).toBe('unknown');
    expect(info.health_detail).toBeNull();
  });
});

describe('createEnvironmentReqFromForm', () => {
  // These used to be nine tests about the pre-plugin `kubeconfig` field: base64
  // in, YAML out, and a single-line value routed to `kubeconfig_credstore_ref`
  // instead. **Task 22 stopped sending that pair at all.** The dialog collects
  // the fields the product's plugin declares and the adapter forwards them as
  // `credentials`, the externally tagged map Task 18b Step 2 defined -- so the
  // base64 round trip, the document-versus-reference sniffing and the
  // varchar(1024) limit they were all about are no longer this adapter's
  // problem. `CredentialFields.test.ts` covers what replaced them.
  const baseForm = { name: 'plat-1', product_id: 'product-vhp' };

  it('sends the plugin-shaped credentials map, and no legacy field', () => {
    const req = createEnvironmentReqFromForm({
      ...baseForm,
      credentials: { kubeconfig: { material: 'apiVersion: v1\n' } },
    });

    expect(req.credentials).toEqual({ kubeconfig: { material: 'apiVersion: v1\n' } });
    // The whole point of this task: the pre-plugin pair is not sent.
    expect(req).not.toHaveProperty('kubeconfig');
    expect(req).not.toHaveProperty('kubeconfig_credstore_ref');
  });

  it('carries every declared credential, not just one', () => {
    const req = createEnvironmentReqFromForm({
      ...baseForm,
      credentials: {
        kubeconfig: { material: 'apiVersion: v1\n' },
        api_token: { material: 't0ken' },
      },
    });
    expect(Object.keys(req.credentials ?? {}).sort()).toEqual(['api_token', 'kubeconfig']);
  });

  it('sends the product, which the gear requires since Task 20b', () => {
    const req = createEnvironmentReqFromForm({ ...baseForm, credentials: {} });
    expect(req.product_id).toBe('product-vhp');
  });

  it('defaults is_default to false rather than omitting it', () => {
    const req = createEnvironmentReqFromForm({ ...baseForm, credentials: {} });
    expect(req.is_default).toBe(false);
  });
});

describe('updateEnvironmentReqFromForm', () => {
  it('omits the kubeconfig keys entirely when the form does not mention it', () => {
    const req = updateEnvironmentReqFromForm({ description: 'x' });
    expect('kubeconfig' in req).toBe(false);
    expect('kubeconfig_credstore_ref' in req).toBe(false);
  });

  it('forwards a pasted replacement as `kubeconfig`', () => {
    const yaml = 'apiVersion: v1\nkind: Config\nclusters: []\n';
    const req = updateEnvironmentReqFromForm({ kubeconfig: btoa(yaml) });
    expect(req.kubeconfig).toBe(yaml);
    expect(req.kubeconfig_credstore_ref).toBeUndefined();
  });

  it('forwards a replacement reference as `kubeconfig_credstore_ref`', () => {
    const req = updateEnvironmentReqFromForm({ kubeconfig: 'credstore://team-a/new' });
    expect(req.kubeconfig_credstore_ref).toBe('credstore://team-a/new');
    expect(req.kubeconfig).toBeUndefined();
  });

  it('treats a blank kubeconfig as "unchanged" rather than throwing', () => {
    // An edit form binds a textarea to a string, so an untouched field arrives as `''`.
    // On create that is the required-field refusal; on a PATCH there is nothing to require,
    // and the gear's column is NOT NULL so there is no "clear it" to express either.
    for (const blank of ['', '   ', '\n']) {
      const req = updateEnvironmentReqFromForm({ description: 'x', kubeconfig: blank });
      expect(req.description).toBe('x');
      expect('kubeconfig' in req).toBe(false);
      expect('kubeconfig_credstore_ref' in req).toBe(false);
    }
  });

  it('still refuses an unusable non-blank replacement', () => {
    // The blank exemption above is for "untouched", not for "unclassifiable": a value the
    // caller actually typed still has to be one of the two things the gear accepts.
    expect(() => updateEnvironmentReqFromForm({ kubeconfig: 'x'.repeat(1025) })).toThrow(
      /neither a kubeconfig document nor a credstore reference/
    );
  });
});

// --- Variables -----------------------------------------------------------------

describe('partitionEnvironmentVariables', () => {
  it("keeps only the environment's rows, because the gear filter is additive", () => {
    // Driven live: `GET /qa/v1/variables?environment_id=<p>` answers the global rows
    // PLUS that environment's, so the server filter alone is a superset of what
    // legacy's `/platforms/{name}/variables` returned.
    const rows = [
      { id: 'g', environment_id: null, name: 'GLOBAL', value: 'g' },
      { id: 'p', environment_id: 'plat-1', name: 'P', value: 'p' },
    ];
    expect(partitionEnvironmentVariables(rows, 'plat-1')).toEqual([{ name: 'P', value: 'p' }]);
    expect(partitionEnvironmentVariables(rows, null)).toEqual([{ name: 'GLOBAL', value: 'g' }]);
  });
});

describe('variableWritePlan', () => {
  const previous = [
    { id: 'id-a', environment_id: null, name: 'A', value: '1' },
    { id: 'id-b', environment_id: null, name: 'B', value: '2' },
  ];

  it('issues one upsert per changed row and one delete per removed row', () => {
    const plan = variableWritePlan(previous, [{ name: 'A', value: '9' }], null);
    expect(plan.upserts).toEqual([{ name: 'A', value: '9', environment_id: null }]);
    expect(plan.deletes).toEqual(['id-b']);
  });

  it('does not rewrite an unchanged row', () => {
    const plan = variableWritePlan(previous, [
      { name: 'A', value: '1' },
      { name: 'B', value: '2' },
    ], null);
    expect(plan.upserts).toEqual([]);
    expect(plan.deletes).toEqual([]);
  });

  it('upserts a brand-new row', () => {
    const plan = variableWritePlan([], [{ name: 'C', value: '3' }], 'plat-1');
    expect(plan.upserts).toEqual([{ name: 'C', value: '3', environment_id: 'plat-1' }]);
    expect(plan.deletes).toEqual([]);
  });

  it('treats a rename as a delete plus an upsert, because the key is the name', () => {
    const plan = variableWritePlan([previous[0]], [{ name: 'A2', value: '1' }], null);
    expect(plan.upserts).toEqual([{ name: 'A2', value: '1', environment_id: null }]);
    expect(plan.deletes).toEqual(['id-a']);
  });

  it('ignores a row whose name is blank rather than writing an unnamed variable', () => {
    const plan = variableWritePlan([], [{ name: '   ', value: 'x' }], null);
    expect(plan.upserts).toEqual([]);
  });
});

// --- Custom plans: included_plans -> files ------------------------------------

describe('expandCustomPlanFiles', () => {
  const planA = encodePlanId('repo-1', 'plans/a.yaml');
  const planB = encodePlanId('repo-2', 'plans/b.yaml');

  const source = {
    async standardPlan(planId: string) {
      if (planId === planA) {
        return { repo_id: 'repo-1', plan_path: 'plans/a.yaml', test_files: ['tests/a1.py', 'tests/a2.py'] };
      }
      if (planId === planB) {
        return { repo_id: 'repo-2', plan_path: 'plans/b.yaml', test_files: ['tests/b1.py'] };
      }
      return null;
    },
    async customPlanFiles(planId: string) {
      if (planId === 'cp-nested') {
        return [{ repo_id: 'repo-3', plan_path: 'plans/c.yaml', path: 'tests/c1.py' }];
      }
      if (planId === 'cp-cycle') {
        return null;
      }
      return null;
    },
    async customPlanIncludes(planId: string) {
      if (planId === 'cp-cycle') return ['cp-cycle', planA];
      // A two-id cycle: x includes y, y includes x — and y also pulls in a real plan, so a
      // passing test cannot pass by simply returning nothing.
      if (planId === 'cp-x') return ['cp-y'];
      if (planId === 'cp-y') return ['cp-x', planA];
      return null;
    },
  };

  it('expands an explicitly selected test into a file entry', async () => {
    const files = await expandCustomPlanFiles(
      { tests: [{ plan_id: planA, test_file: 'tests/a1.py' }] },
      source
    );
    expect(files).toEqual([{ repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a1.py' }]);
  });

  it('expands an included whole plan into one file entry per test file', async () => {
    // Without this, the "Whole plans" tab saves cleanly and the plan runs nothing:
    // `UpsertCustomPlanReq` has no `included_plans` field at all.
    const files = await expandCustomPlanFiles({ tests: [], included_plans: [planA, planB] }, source);
    expect(files).toEqual([
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a1.py' },
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a2.py' },
      { repo_id: 'repo-2', plan_path: 'plans/b.yaml', path: 'tests/b1.py' },
    ]);
  });

  it('expands an included custom plan through its already-flat files', async () => {
    const files = await expandCustomPlanFiles({ tests: [], included_plans: ['cp-nested'] }, source);
    expect(files).toEqual([{ repo_id: 'repo-3', plan_path: 'plans/c.yaml', path: 'tests/c1.py' }]);
  });

  it('stops on an include naming the plan being saved (the `expanded` seed guard)', async () => {
    // This one is caught by seeding `expanded` with the plan's own id, *before* `visit`
    // recurses at all — so it covers the seed and NOT the recursion guard. The two-id case
    // below is what covers the recursion guard; review round 1 caught that this test alone
    // asserted nothing about it.
    const files = await expandCustomPlanFiles(
      { id: 'cp-cycle', tests: [], included_plans: ['cp-cycle'] },
      source
    );
    expect(files).toEqual([]);
  });

  it('terminates a two-id cycle and still collects what the cycle reaches', async () => {
    // cp-x -> cp-y -> cp-x. Without the `expanded` check inside `visit` this recurses until
    // the stack dies; with it, the walk closes the loop once and still picks up the real
    // plan cp-y contributes. Asserting the files (not just "it returned") is what makes the
    // test bite: an implementation that bailed out of the cycle too early would answer [].
    const files = await expandCustomPlanFiles({ tests: [], included_plans: ['cp-x'] }, source);
    expect(files).toEqual([
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a1.py' },
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a2.py' },
    ]);
  });

  it('visits each side of a cycle once, so a diamond inside it is not double-counted', async () => {
    const files = await expandCustomPlanFiles(
      { tests: [], included_plans: ['cp-x', 'cp-y', planA] },
      source
    );
    expect(files).toHaveLength(2);
  });

  it('dedupes a diamond where two includes contribute the same file', async () => {
    const files = await expandCustomPlanFiles(
      { tests: [{ plan_id: planA, test_file: 'tests/a1.py' }], included_plans: [planA] },
      source
    );
    expect(files).toEqual([
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a1.py' },
      { repo_id: 'repo-1', plan_path: 'plans/a.yaml', path: 'tests/a2.py' },
    ]);
  });

  it('reports an include it cannot resolve rather than saving a plan that runs nothing', async () => {
    await expect(
      expandCustomPlanFiles({ tests: [], included_plans: ['who-knows'] }, source)
    ).rejects.toThrow(/who-knows/);
  });
});

// --- Dashboard -----------------------------------------------------------------

describe('dashboardFromDto', () => {
  const dto = {
    total_runs: 8,
    active_runs: 1,
    queued_runs: 0,
    recent_runs: [
      {
        run_id: 'r1',
        name: 'smoke-1',
        phase: 'succeeded',
        started_at: '2026-08-26T10:00:00Z',
        duration: '2m 5s',
        environment_id: 'p1',
        product_key: null,
        repo_id: 'repo-1',
        plan_path: 'plans/smoke.yaml',
        app_version: null,
      },
    ],
    active_runs_list: [],
    recent_run_test_trend: [],
    daily_test_status_trend: [],
    failed_recent: [
      {
        test_name: 't',
        test_file: 'tests/a.py',
        run_id: 'r1',
        repo_id: 'repo-1',
        plan_path: 'plans/smoke.yaml',
        environment_id: 'p1',
        finished_at: null,
        jira_key: null,
        launch_id: null,
      },
    ],
    flaky_tests: [
      { test_name: 't', test_file: 'tests/a.py', repo_id: 'repo-1', plan_path: 'plans/smoke.yaml', passed: 1, failed: 1, total: 2 },
    ],
    failed_24h_count: 0,
    failed_prev_24h_count: 0,
    pass_rate_24h: null,
    pass_rate_prev_24h: null,
    quality_vectors_pass_rate: [],
  } as never;

  it('omits the three fields the gear has no source for rather than zeroing them', () => {
    // §8-C3: a plausible-looking 0 is worse than a missing panel, and nothing
    // renders these any more (Task 8a replaced the strip with a labelled notice).
    const stats = dashboardFromDto(dto);
    expect(stats.total_plans).toBeUndefined();
    expect(stats.total_schedules).toBeUndefined();
    expect(stats.platforms_summary).toBeUndefined();
  });

  it('projects the ten-field dashboard run onto the shape the cards read', () => {
    const run = dashboardFromDto(dto).recent_runs[0];
    expect(run.name).toBe('smoke-1');
    expect(run.phase).toBe('succeeded');
    expect(run.duration).toBe('2m 5s');
    expect(run.platform).toBe('p1');
    expect(decodePlanId(run.plan_id)).toEqual({ repo_id: 'repo-1', path: 'plans/smoke.yaml' });
  });

  it('carries the failed and flaky cards over with their plan identity re-folded', () => {
    const stats = dashboardFromDto(dto);
    expect(decodePlanId(stats.failed_recent[0].plan_id)).toEqual({
      repo_id: 'repo-1',
      path: 'plans/smoke.yaml',
    });
    expect(stats.failed_recent[0].workflow_name).toBe('r1');
    // Fix round 2: this fixture's `failed_recent[0]` has always carried
    // `environment_id: 'p1'` (Task 26 fix round 1's field rename), but nothing asserted
    // it — `FailedTestCardDto` once declared a stale `platform_id?: string | null` in
    // the frozen generated schema, so a regression back to reading it would typecheck
    // clean and silently null out every card's environment. Proved by mutation: reverting
    // `failedCardFromDto`'s `dto.environment_id` to `dto.platform_id` survived the whole
    // suite before this line existed.
    expect(stats.failed_recent[0].platform).toBe('p1');
    expect(decodePlanId(stats.flaky_tests[0].plan_id)).toEqual({
      repo_id: 'repo-1',
      path: 'plans/smoke.yaml',
    });
  });
});

// --- Run logs ------------------------------------------------------------------

describe('parseRunLogSse', () => {
  it('joins the data frames of an event stream into the flat text legacy rendered', () => {
    const stream = 'data: {"line":"first"}\n\ndata: {"line":"second"}\n\n';
    expect(parseRunLogSse(stream)).toBe('first\nsecond');
  });

  it('passes a plain-text body through unchanged', () => {
    // Belt and braces: if the gear ever answers text/plain here, do not eat it.
    expect(parseRunLogSse('just some logs')).toBe('just some logs');
  });

  it('answers an empty string for the empty stream a run with no output serves', () => {
    // Driven live: GET /qa/v1/runs/{id}/logs on a finished run with no output
    // answers 200 text/event-stream with a zero-byte body.
    expect(parseRunLogSse('')).toBe('');
  });
});

// --- Plans (the five transforms review round 1 found untested) -------------------

const PLAN_DTO = {
  repo_id: '32656370-6be5-4445-99f3-cfa86195bbb3',
  branch: 'main',
  path: 'plans/nested/smoke.yaml',
  name: 'smoke',
  test_files: ['tests/test_login.py', 'tests/test_logout.py'],
  timeout_seconds: 300,
  tags: ['ci'],
  exclusive: null,
} as never;

const PLAN_CTX = {
  repoName: 'smoke-repo',
  productId: 'prod-1',
  productKey: 'smoke-product',
  productName: 'Smoke Product',
};

describe('planFromDto', () => {
  it('packs (repo_id, path) into the opaque id components pass around', () => {
    expect(decodePlanId(planFromDto(PLAN_DTO).id)).toEqual({
      repo_id: '32656370-6be5-4445-99f3-cfa86195bbb3',
      path: 'plans/nested/smoke.yaml',
    });
  });

  it('derives dir_path as the path minus its last segment', () => {
    expect(planFromDto(PLAN_DTO).dir_path).toBe('plans/nested');
  });

  it('answers an empty dir_path for a plan at the content root', () => {
    expect(planFromDto({ ...(PLAN_DTO as object), path: 'smoke.yaml' } as never).dir_path).toBe('');
  });

  it('omits timeout_seconds entirely when the gear sends null', () => {
    // The defect review round 1 found: this was `?? 0`, and a `0` reads as "times out
    // immediately" to any future consumer. `types.ts`' own comment on
    // `TestPlan.timeout_seconds` names this exact value as the one not to write.
    const plan = planFromDto({ ...(PLAN_DTO as object), timeout_seconds: null } as never);
    expect('timeout_seconds' in plan.plan).toBe(false);
    expect(plan.plan.timeout_seconds).toBeUndefined();
  });

  it('carries a real timeout through unchanged', () => {
    expect(planFromDto(PLAN_DTO).plan.timeout_seconds).toBe(300);
  });

  it('omits the manifest fields the gear does not serve rather than defaulting them', () => {
    const plan = planFromDto(PLAN_DTO);
    expect(plan.plan.validation).toBeUndefined();
    expect(plan.plan.node_selector).toBeUndefined();
    expect(plan.plan.tolerations).toBeUndefined();
    // `versions` has no gear source either; `[]` is "none known", not a count.
    expect(plan.versions).toEqual([]);
  });

  it('leaves the repository and product context null when the caller knows none', () => {
    const plan = planFromDto(PLAN_DTO);
    expect([plan.repo_name, plan.product_id, plan.product_key, plan.product_name]).toEqual([
      null,
      null,
      null,
      null,
    ]);
  });

  it('binds the repository and product context when the caller does know it', () => {
    const plan = planFromDto(PLAN_DTO, PLAN_CTX);
    expect(plan.repo_name).toBe('smoke-repo');
    expect(plan.product_key).toBe('smoke-product');
    // Every repository in this deployment is a git repository — the archive-upload route
    // does not exist (§8-C4) — so this is a deployment fact, not a default.
    expect(plan.source).toBe('git');
  });
});

describe('testFilesFromPlan', () => {
  it('answers one row per test file, all attributed to the same plan', () => {
    const rows = testFilesFromPlan(PLAN_DTO, PLAN_CTX);
    expect(rows.map((row) => row.test_file)).toEqual(['tests/test_login.py', 'tests/test_logout.py']);
    expect(new Set(rows.map((row) => row.plan_id)).size).toBe(1);
    expect(decodePlanId(rows[0].plan_id)).toEqual({
      repo_id: '32656370-6be5-4445-99f3-cfa86195bbb3',
      path: 'plans/nested/smoke.yaml',
    });
    expect(rows[0].plan_name).toBe('smoke');
  });

  it('does NOT borrow the plan tags for the test file', () => {
    // §8-C1: the catalog's per-file metadata has no gear source at all. The plan's tags are
    // the *plan's*; attributing them to each of its files would be the fabrication §8-C1
    // warns about, and it would look exactly like real data.
    const rows = testFilesFromPlan(PLAN_DTO, PLAN_CTX);
    expect(PLAN_DTO as unknown as { tags: string[] }).toHaveProperty('tags', ['ci']);
    expect(rows[0].tags).toEqual([]);
  });

  it('leaves every unsourced metadata column absent', () => {
    const row = testFilesFromPlan(PLAN_DTO, PLAN_CTX)[0];
    expect(row.title).toBeUndefined();
    expect(row.component).toBeUndefined();
    expect(row.description).toBeUndefined();
    expect(row.quality_vectors).toBeUndefined();
    expect(row.versions).toBeUndefined();
    expect(row.loc).toBeUndefined();
  });

  it('answers an empty list for a plan with no test files', () => {
    expect(testFilesFromPlan({ ...(PLAN_DTO as object), test_files: [] } as never)).toEqual([]);
  });
});

describe('coveragePointsFromDto', () => {
  const rows = [
    { product_key: 'mine', version: '7.0', build: 'mine/7.0', coverage: { line_pct: 10, branch_pct: 20, function_pct: 30 } },
    { product_key: 'theirs', version: '8.0', build: 'theirs/8.0', coverage: { line_pct: 90, branch_pct: 90, function_pct: 90 } },
  ] as never[];

  it('keeps only the asked-for product, so one card cannot chart another product', () => {
    // Review round 1: the route answers one point per product for the WHOLE deployment,
    // and the only consumer is a per-product card captioned "Coverage belongs to this
    // product…". Without this filter that caption is false.
    const points = coveragePointsFromDto(rows, { id: 'p-mine', key: 'mine' });
    expect(points.map((p) => p.product_key)).toEqual(['mine']);
    expect(points[0].version).toBe('7.0');
    expect(points[0].coverage.line_pct).toBe(10);
  });

  it('echoes back the product it was asked for rather than inventing one', () => {
    expect(coveragePointsFromDto(rows, { id: 'p-mine', key: 'mine' })[0].product_id).toBe('p-mine');
  });

  it('answers an empty list when the product is not yet resolved', () => {
    // A component that maps over undefined crashes; over [] it renders its empty state.
    expect(coveragePointsFromDto(rows, undefined)).toEqual([]);
  });

  it('answers an empty list for a product with no coverage of its own', () => {
    expect(coveragePointsFromDto(rows, { id: 'p-none', key: 'none' })).toEqual([]);
  });

  it('leaves run_name and collected_at empty rather than fabricating a measurement time', () => {
    // §8-C8 / §11.9: `collected_at` is both sorted on and rendered as a date by
    // `ProductCoverageCard`, so a non-empty collection reads "Invalid Date" there. Filling
    // it with `Date.now()` would turn that visible gap into an invisible lie.
    const point = coveragePointsFromDto(rows, { id: 'p-mine', key: 'mine' })[0];
    expect(point.run_name).toBe('');
    expect(point.collected_at).toBe('');
  });
});

describe('openBugsQuery', () => {
  const planId = encodePlanId('32656370-6be5-4445-99f3-cfa86195bbb3', 'plans/smoke.yaml');

  it('sends both halves of the pair together', () => {
    const params = new URLSearchParams(openBugsQuery(planId));
    expect(params.get('repo_id')).toBe('32656370-6be5-4445-99f3-cfa86195bbb3');
    expect(params.get('plan_path')).toBe('plans/smoke.yaml');
  });

  it('sends neither half when asked for all bugs', () => {
    // Driven live: omitting both is a 200; supplying one alone is a 400 reading
    // "repo_id and plan_path must be supplied together, or not at all".
    expect(openBugsQuery(undefined)).toBe('');
    expect(openBugsQuery(null)).toBe('');
    expect(openBugsQuery('')).toBe('');
  });

  it('sends neither half — not one — for an id it cannot decode', () => {
    // This branch decides between one plan's bugs and ALL of them, so getting it wrong is
    // either a guaranteed 400 or a silently wider answer than the caller asked for.
    expect(openBugsQuery('some-legacy-plan-id')).toBe('');
    expect(openBugsQuery('cfc78140-f6e1-4a8c-906a-2f84de5ee1cf')).toBe('');
  });
});

describe('analyticsPlanId', () => {
  it('unpacks the opaque id to the plan path the analytics routes match on', () => {
    // X6's second half: the parameter keeps the NAME `plan_id` and changes its VALUE space
    // to the plan's path, matched across every repository the caller can see.
    expect(analyticsPlanId(encodePlanId('repo-1', 'plans/smoke.yaml'))).toBe('plans/smoke.yaml');
  });

  it('passes a bare path through unchanged', () => {
    // Driven live: `?plan_id=plans/smoke.yaml` answers rows, so a bare path is a legitimate
    // value for these routes and must not be mangled.
    expect(analyticsPlanId('plans/smoke.yaml')).toBe('plans/smoke.yaml');
  });

  it('passes a custom-plan uuid through rather than dropping it', () => {
    expect(analyticsPlanId('cfc78140-f6e1-4a8c-906a-2f84de5ee1cf')).toBe(
      'cfc78140-f6e1-4a8c-906a-2f84de5ee1cf'
    );
  });
});

// ---------------------------------------------------------------------------
// Environment reads, asserted against fixtures rather than against the type.
//
// These were written when `api/generated/openapi.d.ts` still declared the
// retired `platform_id`/`last_platform(_id)`/`platform` names, which made
// `dto.platform_id` a perfectly well-typed read of a field the gear no longer
// sent — `tsc` could not catch a regression back to it. The schema is
// regenerated now and the type does catch that, so these are no longer the only
// guard. They stay because a fixture shaped like what qa-insights actually
// serves, asserting the environment arrives POPULATED rather than
// null/undefined, outlives any particular generated file: each was confirmed to
// fail against the stale-field mutation it guards before being written.
// ---------------------------------------------------------------------------

describe('testRunResultFromDto', () => {
  it('reads the environment off `environment_id`, which is the field the gear serves', () => {
    const result = testRunResultFromDto({
      run_id: 'run-1',
      repo_id: 'repo-1',
      plan_path: 'plans/smoke.yaml',
      status: 'PASSED',
      run_finished_at: '2026-08-26T10:00:00Z',
      duration: '2m',
      product_version: '26.5',
      branch: 'main',
      environment_id: 'env-1',
    } as never);
    expect(result.platform).toBe('env-1');
  });
});

describe('testAnalyticsFromDto', () => {
  it('reads the environment off `last_environment`, which is the field the gear serves', () => {
    const analytics = testAnalyticsFromDto({
      test_name: 'tests/a.py',
      last_environment: 'env-1',
      last_version: '26.5',
      last_status: 'PASSED',
      last_run_id: 'run-1',
      jira_key: null,
      total_runs: 3,
      pass_count: 2,
      fail_count: 1,
    } as never);
    expect(analytics.last_platform).toBe('env-1');
  });
});

describe('jiraBugFromDto', () => {
  it('reads the environment off `environment_id`, which is the field the gear serves', () => {
    const bug = jiraBugFromDto({
      id: 'bug-1',
      jira_key: 'VHP-1',
      test_name: 'tests/a.py',
      repo_id: 'repo-1',
      plan_path: 'plans/smoke.yaml',
      app_version: '26.5',
      environment_id: 'env-1',
      status: 'open',
      summary: 'boom',
      created_at: '2026-08-26T10:00:00Z',
      resolved_at: null,
    } as never);
    expect(bug.platform).toBe('env-1');
  });
});

describe('analyticsOverviewFromDto', () => {
  const dto = {
    product_id: 'product-vhp',
    version: '26.5',
    scope: 'all',
    plan_id: null,
    branch: null,
    group_by: 'none',
    group_value: null,
    summary: {
      total: 1,
      passed: 1,
      failed: 0,
      not_run: 0,
      passed_pct: 100,
      failed_pct: 0,
      not_run_pct: 0,
      case_total: 1,
      case_passed: 1,
      case_failed: 0,
      case_skipped: 0,
      case_xfail: 0,
      case_xpass: 0,
      case_expected: 1,
    },
    lists: {
      passed: [
        {
          test_file: 'tests/a.py',
          test_name: 'tests/a.py',
          component: null,
          tags: [],
          repo_id: 'repo-1',
          plan_path: 'plans/smoke.yaml',
          plan_name: 'smoke',
          versions: ['26.5'],
          last_status: 'PASSED',
          last_environment: 'env-1',
          last_run_id: 'run-1',
          last_build: '100',
          last_run_finished_at: '2026-08-26T10:00:00Z',
          pass_count: 1,
          fail_count: 0,
          skipped_count: 0,
          total_runs: 1,
          case_status: null,
          case_tickets: [],
        },
      ],
      failed: [],
      not_run: [],
    },
    heatmap: { days: [], rows: [] },
    trend: { points: [] },
    build_distribution: [],
    flaky: [],
    quality_vectors: { items: [], unclassified_tests: 0, total_tests: 0 },
    grouped: {
      component: [],
      tag: [],
      // `GroupedSummariesDto` was itself renamed from a `platform`-keyed field at
      // Task 25 (row `PlatformGroupSummaryDto` -> `EnvironmentGroupSummaryDto`); the
      // schema once called both the array and its two id/name fields by the
      // retired names.
      environment: [
        { environment_id: 'env-1', environment: 'sv-test', total: 1, passed: 1, failed: 0, not_run: 0 },
      ],
    },
  } as never;

  it("reads a list item's environment off `last_environment`, which is the field the gear serves", () => {
    const overview = analyticsOverviewFromDto(dto, {});
    expect(overview.lists.passed[0].last_platform).toBe('env-1');
  });

  it('reads the grouped environment bar off `environment`/`environment_id`, which is the field the gear serves', () => {
    const overview = analyticsOverviewFromDto(dto, {});
    expect(overview.grouped.platform[0].value).toBe('sv-test');
  });
});
