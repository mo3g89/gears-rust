---
status: accepted
date: 2026-09-08
---
# No value derived from credential material is ever formatted

**ID**: `cpt-cf-qa-adr-credential-containment`

## Context and Problem Statement

The subsystem handles credentials that are valid against real customer infrastructure: kubeconfigs
containing client private keys, SSH private keys, administrator passwords, JIRA API tokens, Slack
webhooks. They pass through plugin code, connector code and error paths written by several people.

A credential that reaches a log line is unrecoverable — it is in whatever the cluster ships logs
to, and rotating it is the only remedy. Conventions ("remember not to log secrets") do not survive
contact with an error path someone adds later. What rule can be enforced rather than remembered?

## Decision Drivers

* The failure is silent: nothing breaks when a secret is logged, so nothing catches it.
* Error paths are where it happens, and error paths are written under time pressure.
* The rule has to hold for code that does not exist yet.
* Genuine diagnostics still have to be possible, or the rule will be worked around.

## Considered Options

* Forbid formatting anything derived from credential material, enforced by a test
* Redact at the logging layer with a pattern matcher
* A wrapper type whose `Debug`/`Display` print a placeholder

## Decision Outcome

Chosen option: **forbid the formatting, and enforce it**. No value derived from credential
material is rendered — not through `Display`, not through `Debug`, not into a message, a log line
or a DTO.

Three mechanisms make it structural rather than advisory:

* **`PluginFailure::detail` is `&'static str`.** It is a compile-time constant by type, so it
  *cannot* be built from runtime bytes. An error that wants to explain itself picks from a fixed
  vocabulary.
* **`assert_no_leak`**, in `qa-product-sdk`'s test support, drives every plugin with planted
  credential material and fails the build if any of that material reaches a published surface.
  A new plugin inherits the check by existing.
* **Secrets travel as references.** `prepare_run_access` works from `credstore_ref` alone, so the
  dispatching process never holds plaintext to leak in the first place.

The one sanctioned exception is **text a remote sent back**. A remote's own error message is not
derived from our credential material, and suppressing it would leave operators debugging blind.

### Consequences

* Good, because the guarantee is a build failure rather than a review comment.
* Good, because it holds for plugins nobody has written yet.
* Good, because the `&'static str` choice makes the safe path the only path that compiles.
* Bad, because diagnostics are coarser: an error says which credential field was rejected, not what
  was wrong with its value.
* Bad, because the remote-text exception is a judgement call — a remote that echoes a credential
  back would defeat it, and nothing mechanical catches that.
* Bad, because `assert_no_leak` proves the absence of *planted* material on *covered* surfaces; it
  is a strong test, not a proof.
* Bad, because **the two HMAC roots remain single points of compromise, and that residue is
  accepted rather than closed** (second review, finding #62). `collect_report_signing_secret` and
  `bundle_download_signing_secret` are each one deployment-wide value from which every tenant's key
  is HKDF-derived (`collect::derive_signing_key`, and the bundle tag's equivalent). Whoever holds
  one can derive any tenant's key and sign for a tenant of their choosing, because the `tenant_id`
  inside the signed tuple is chosen by whoever signs. The derivation buys something real and
  narrower: a derived key is not the operational credential an operator typed into configuration,
  so nothing downstream — a log line, a metrics label, an error payload — can leak a value that
  works for a different tenant. Closing the rest needs a per-tenant secret in the credential store
  and a config surface that is no longer one value, which is a feature. Recorded here, in the
  repository, because until this entry the acceptance lived only in an untracked planning note.

  **The acceptance has a condition, and it is a precondition rather than a "some day".** The owner
  ruled on 2026-09-29 that a single deployment-wide root is accepted only while the platform serves
  no tenant other than the operator's own. Before it serves any other tenant, each root must become
  per-tenant and be held in the credential store. It was demonstrated live on 2026-09-29 (after the
  roots moved into a Secret): an agent holding the root minted a valid signature for a tenant it
  had never authenticated as. Moving the
  roots out of the ConfigMap into a Secret closed their plaintext placement and nothing about
  their scope, and the HKDF derivation narrows the damage of a leaked per-tenant key, not of a
  leaked root.

  *(Added 2026-09-29, second review, finding #62; condition stated 2026-09-30.)*
* Bad, because **a runner pod's credentials are fetched, not mounted, and residues remain** (listed
  under "What remains" below)
  (second review, finding #94; **decision amendment, 2026-10-09**).

  **Decision amendment (2026-10-09, finding #94).** Gears write no Kubernetes Secrets at all: a
  runner pod fetches its credentials when it starts, proving who it is with a ServiceAccount token
  bound to that pod, and a mandatory admission policy limits the Workflows `qa-platform-gears` may
  submit to the shape qa-runs generates. This replaces the entry of 2026-09-30, which accepted that a
  compromised gears process could read any Secret in the Argo namespace through the runner-`Secret`
  writer (labelled `D4` in code comments) and through `workflows: create`. That writer needed
  `create` and `patch` on Secrets in a namespace that also holds Argo's own, and RBAC could not narrow
  the grant (`create` ignores `resourceNames`).

  How it works:
  - qa-runs renders a projected `serviceAccountToken` volume (audience
    `qa-platform-runner-credentials`, `expirationSeconds: 600`) into every runner pod, plus a
    memory-backed `emptyDir` on the parent directory of each credential mount, and stamps the
    `qa-platform/run-id` and `qa-platform/tenant-id` labels on each node template.
    `QA_CREDENTIALS_URL`, `QA_CREDENTIALS_TOKEN_FILE` and `QA_CREDENTIAL_DIRS` are set only for runs
    whose plugin access has credential mounts, and all three are reserved names: no run parameter,
    pipeline variable or environment variable may set them.
  - Before the tenant's code runs, the runner POSTs the token in `X-QA-Runner-Token` to
    `POST /qa/v1/runner-credentials`, an internal route: not registered in the gateway and not
    proxied by the UI nginx (which answers 404 for it), so reachable in-cluster only. It is
    anonymous at the toolkit layer
    (a bearer header would be re-validated by the auth middleware and a ServiceAccount token
    refused). It writes the returned files into the directories it declared.
  - qa-environments runs a `TokenReview` for that audience and accepts only the pinned runner
    ServiceAccount carrying the pod-name and pod-uid claims, then reads the pod and requires the
    same uid and a non-terminal phase. The pod's workflow is its controller `ownerReference`
    (exactly one, `argoproj.io` `Workflow`), which the Argo controller sets and no `Workflow` spec
    can; a pod without one is refused. The `workflows.argoproj.io/workflow` label is not used: Argo
    applies template metadata labels after its own, so a `Workflow` author could overwrite it. The
    tenant and run labels only say where to look. qa-environments then asks qa-runs
    (`resolve_runner_pod`) for that run, and qa-runs answers only when the run is live, its owning
    tenant equals the pod's tenant label, and its execution handle (`execution_ref`) equals the
    owner workflow's name. *(Superseded 2026-10-12: the binding is the Workflow's uid, not its name;
    see the amendment below.)*
  - Only the mounts that the environment's plugin returns from `prepare_run_access` are resolved
    through credstore, and only refs among the environment's own stored credentials are served. A
    nil pod tenant, a foreign ref, or a call over its 30 s deadline is refused. Every 403 has one
    body, and each grant or denial is logged on `qa_environments.runner_credentials` with a file
    count, never refs or material.

  Deleted: the `RunnerSecretWriter` port and adapter, the `runner-secret` feature, the
  `qa-platform-secret-writer` ServiceAccount, Role, token Secret and admission policy, the writer's
  kubeconfig and mounts, qa-runs' `secret` volumes and `secretKeyRef` path, and the config keys
  `argo.secret_prefix`, `argo.secret_key`, `argo.kubeconfig_path` and qa-runs'
  `secret_name_prefix` and `secret_key`. The refusal of `argo.namespace` equal to the release
  namespace went with them, because its only reason was the writer. `qa-platform-gears` now holds no
  rights on Secrets and `tokenreviews: create` through its own ClusterRole.

  The Workflow guard is the chart's `qa-platform-workflow-guard-<release namespace>`
  ValidatingAdmissionPolicy and binding, `failurePolicy: Fail`, matching only
  `qa-platform-gears` in the Argo namespace. It is an allowlist, not a denylist, because Argo has
  many fields that widen rights and later releases add more. On CREATE it admits only the spec and
  template keys qa-runs emits (one source of truth in qa-runs, checked against the chart's CEL by a
  Helm test), the pinned `argo.workflowServiceAccount`, `runAsNonRoot`, only `emptyDir` and
  audience-pinned projected volumes, and containers with no `envFrom` or `valueFrom` and no
  privilege escalation. The shape rules apply to CREATE only: the Argo controller adds
  `spec.arguments` and template `inputs` and `outputs` after creation (measured on the stand), so an
  UPDATE may change `spec.shutdown` and nothing else, which is all the cancel path writes. `status`
  is the Argo controller's alone: a CREATE may carry none and an UPDATE must leave it unchanged (the
  cancel merge-patch leaves it equal; the controller writes status as its own ServiceAccount, which
  the policy does not match). The chart refuses to render where the API is not served.

  What remains:
  * Bad, because tenant test code can re-request its own run's credentials while its pod is alive.
    This is not a widening: the same files are already in the pod's tmpfs.
  * Bad, because the guard does not restrict the runner image or its command. *(Narrowed
    2026-10-12: it now allowlists the container and pod securityContext; see the amendment.)*
  * Bad, because Kubernetes 1.30 or later is required (ValidatingAdmissionPolicy v1).
  * Bad, because `qa-platform-gears` can create a `TokenReview` for any token. That tells it whether
    a token is valid, and nothing more.
  * Bad, because a product plugin's `access.service_account` override would now be denied by the
    pinned-ServiceAccount rule. No in-tree plugin uses it.
  * Bad, because the guard matches `qa-platform-gears` only, and the Workflow-name binding was
    defeated by delete-and-recreate. *(Superseded 2026-10-12: the binding is now the Workflow uid
    and a second policy keeps other principals off qa-runs Workflows, runner pods and runner
    tokens; see the amendment.)*
  * Bad, because credentials and the runner's token now cross the pod network in plain HTTP to the
    gears Service. Before, a mounted Secret travelled API server to kubelet over TLS. The runner
    NetworkPolicy limits who the runner talks to, not who can observe that traffic on the node or
    the CNI. *(2026-10-12: this is an open owner decision, recorded in DESIGN §3.14 "Accepted
    residues and open owner decisions".)*
  * Bad, because a product whose `RunnerSpec` sets a custom image or `command` that bypasses
    `entrypoint.sh` must run `fetch_credentials.py` itself, or its credential directories stay
    empty (documented on `RunnerSpec`). *(2026-10-12: the image's entrypoint is now the launcher
    `launch.py`; such a product bypasses it and must also do its own scrub, running any credential
    fetch with the platform's names only. See the last amendment.)*
  *(Added 2026-09-30, second review, finding #94.)*

### Amendment 2026-10-12: re-verification of #94

An independent re-verification of the branch found these gaps. Each is closed as follows.

* **Binding by uid.** The earlier text said a pod "can never" name the Workflow qa-runs recorded.
  That was false for delete-and-recreate: a Workflow deleted and resubmitted under the same name
  passed the name comparison. qa-runs now records the Workflow's uid when it starts a run
  (`qa_runs.execution_uid`, migration `m20261012_000007`), and `resolve_runner_pod` answers only
  when the pod's controller `ownerReference` uid equals the recorded one. An empty uid and a run
  with no recorded uid are refused. The name stays as a first filter.
* **The runner guard.** The chart's `qa-platform-runner-guard-<release namespace>`
  ValidatingAdmissionPolicy lets only `qa-platform-gears` and the Argo workflow-controller
  (`argo.controllerServiceAccount`, default `argo-workflows-workflow-controller`) write qa-runs
  Workflows, or Workflows that name the runner ServiceAccount (also through `templateDefaults`,
  `executor`, `podSpecPatch` and inline templates). Only the controller may create or update runner
  pods; the garbage collector may update (finalizers, ownerReferences). Nobody may add an ephemeral
  container to a runner pod. No new ServiceAccount-token Secret may name the runner
  ServiceAccount, so its tokens come only from kubelets (the projected volume) once any
  pre-existing token Secret naming it has been deleted (UPGRADING.md, "2026-10-12 (second):
  runner environment and guards", item 4). This
  changes what operators can do: a cluster-admin, the Argo UI's stop, retry and resubmit, and
  `kubectl label` or `kubectl debug` on runner pods or qa-runs Workflows are now denied. Deleting
  still works. Known residue: a foreign Workflow that reaches the runner ServiceAccount through a
  WorkflowTemplate, ClusterWorkflowTemplate or CronWorkflow, or through an escaped `podSpecPatch`,
  is not matched. Its pods get only the runner ServiceAccount's own RBAC and no credentials,
  because their controlling Workflow's uid is not one qa-runs recorded.
* **The trusted identities are guarded.** The runner guard's exemptions are only as strong as who
  can act as the exempt principals: anyone who could run a pod as the controller's ServiceAccount,
  or mint its token, could create a runner pod with a forged ownerReference (a live Workflow's name
  and uid are readable). The guard's `trustedServiceAccounts` (decided at render time) lists the
  controller's ServiceAccount when it runs in `argo.namespace`, and `qa-platform-gears` when
  `argo.namespace` is the release namespace. For each, no new ServiceAccount-token Secret may name
  it, so its tokens come only from kubelets (the projected volume) once any pre-existing token
  Secret naming it has been deleted (same UPGRADING item); only the
  ReplicaSet controller (`system:serviceaccount:kube-system:replicaset-controller`, or
  `system:kube-controller-manager` where kube-controller-manager does not use per-controller
  credentials) may create a pod running as it, only the garbage collector may update one, and
  nobody may add an ephemeral container to one. On the stand the controller pod's managedFields
  show the ReplicaSet controller's create and only status writes after it.
* **The securityContext allowlist.** The Workflow guard also allowlists container keys, a
  container `securityContext` of `allowPrivilegeEscalation` and `capabilities: {drop: [ALL]}`, and
  a pod `securityContext` with `runAsUser` above 0, `fsGroup` above 0 if set, and seccomp
  `RuntimeDefault`, plus the isolation label. The guard therefore restricts capabilities; it still
  does not restrict the runner image or its command.
* **Fetchers ignore proxies; `PYTEST_ADDOPTS` is reserved.** `fetch_credentials.py` and
  `fetch_bundle.py` use an opener with an empty `ProxyHandler`, cap responses (1 MiB and 1 GiB),
  and roll back partial writes. The entrypoint runs them, and the collect report, with
  `env -u LOCALDOMAIN -u RES_OPTIONS -u HOSTALIASES python3 -I`, and unsets `PYTEST_ADDOPTS`. The
  resolver variables matter because the gears' in-cluster name has fewer dots than `ndots:5`, so
  glibc tries the search list first, and `LOCALDOMAIN` replaces it: a run variable
  `LOCALDOMAIN=evil.example` would otherwise send the token and the credentials to that host.
  `PYTEST_ADDOPTS` joins the reserved names (16 in all at the time; the floor has since grown, see
  the next amendment), because pytest reads it as extra options
  that could deselect failing tests; stored variables with a reserved name are dropped with a WARN.
  Proxy and resolver variables are not reserved: a suite may need them to reach its targets, and
  only the platform's own fetches must ignore them (`test_runner_fetch_resolver_env.sh`).

What remains:

* Bad, because tenant code can re-request its own run's credentials while its pod lives. The same
  files are already in the pod's tmpfs.
* Bad, because the runner image and command are not restricted.
* Bad, because a foreign Workflow can still reach the runner ServiceAccount through a
  WorkflowTemplate, ClusterWorkflowTemplate or CronWorkflow indirection, or an escaped
  `podSpecPatch`. Its pods get only the runner ServiceAccount's own RBAC and no credentials: their
  controlling Workflow's uid is not one qa-runs recorded.
* Bad, because a principal that can create or patch a Deployment or ReplicaSet whose pod template
  runs as the Argo controller's ServiceAccount, or exec into the controller pod, can still act as
  the controller, and so create a runner pod with a forged ownerReference and fetch that run's
  credentials. The runner guard does not match Deployments or ReplicaSets. Run the controller in
  its own namespace (`argo.controllerNamespace`), so that write access to `argo.namespace` does not
  reach it; the guard then lists no controller in `trustedServiceAccounts`, and that namespace's
  own RBAC is what protects it.
* Bad, because principals with `pods/exec` or `pods/attach` in the Argo namespace can read a live
  runner pod's files. They are cluster operators by this ADR's trust boundary, and blocking exec
  would only hamper debugging.
* Bad, because credentials and the runner's token cross the pod network in plain HTTP to the
  gears Service (unchanged from above). This is an open owner decision, recorded in DESIGN §3.14
  "Accepted residues and open owner decisions": the api-gateway system gear has no TLS listener,
  and TLS needs a sidecar or a gateway change, which is a feature.
* Bad, because runs submitted before `m20261012_000007` are refused credentials after the upgrade
  (fail closed). A runner fetches once, at start, so only a pod that starts after the upgrade under
  a pre-upgrade submit is affected.

### Amendment 2026-10-12: the trust boundary after a further review

A further independent review of the branch, after the amendment above, found that the runner's
environment, its results and its tokens still had paths around the guards, and that this ADR left
some of the trust boundary unstated. Each item is either closed as described or recorded as part of
the boundary.

* **Runner environment.** The image's `/entrypoint.sh` is now `deploy/runner/launch.py`, run with
  `python3 -I`. It saves the pod's whole environment to a private file and runs the bash run script
  with the platform's own names only, so the credential fetch, the bundle fetch and `pip install`
  never see a run variable; `with_tenant_env.py` applies the tenant environment to pytest alone.
  The fix had to sit outside the script: bash sources `BASH_ENV` before line one of any script, so
  no check inside it can come first. A name allowlist was not an option, because the PRD says run
  variables reach tests verbatim. `pip` running platform-only means no run variable chooses the
  package index. The reserved floor now names classes as well as instances: the prefixes in
  `qa_runs::domain::params::RESERVED_PREFIXES` (`GIT_`, `LD_`, `PIP_`, `PYTEST_`, `PYTHON`,
  `QA_CREDENTIAL`, `QA_RUNNER_`, `TEST_BUNDLE_`) and exact names for bash's startup and expansion
  controls, other interpreters' code-loading controls and module paths, programs that tools run on
  demand (`BROWSER`, `SSH_ASKPASS`), and the platform's `COLLECT_ONLY` and `VHP_COLLECT_URL`. git is
  in the runner image and honours many command-running variables, so `GIT_` is reserved as a
  prefix rather than name by name. The floor bounds what the tenant environment can do to the test
  processes, which still see it.
* **Results.** The qa-runs log drain follows only pods whose one controller `ownerReference` carries
  the Workflow uid the run recorded and that run as the Workflow's ServiceAccount; a pod that only
  wears the Workflow's label is not read, so, with the chart's runner guard, it cannot print a
  passing result into the run. The account is the one the Workflow names, which every qa-runs
  Workflow does when deployed with the chart, which sets the runner account. A Workflow that names
  none is checked against the configured runner account (`qa-runs.argo.workflow_service_account`),
  and with neither, no pod's log is read. The verdict is bound to the uid too: when the Workflow under the run's name is no longer the one it
  submitted, the watch ends without a verdict. Residue: a run submitted before `m20261012_000007`
  has no recorded uid, and its watcher pins the uid of the live Workflow it first sees. Failing
  closed instead would erase the results of every run in flight at the upgrade. A delete-and-recreate
  under the same name before that first sight is therefore not caught, for those runs only.
* **Token Secrets.** The runner guard refuses a ServiceAccount-token Secret that names the runner
  ServiceAccount or a trusted one, so the token controller cannot be asked for a long-lived token
  outside any kubelet. An UPDATE of a token Secret that already named the same account (the token
  controller filling `data`) is not a new mint and is admitted, and the upgrade deletes nothing.
  Residue: a token Secret created before the upgrade keeps a working long-lived token until an
  operator deletes it. Once every such Secret is gone, a guarded account's tokens come only from
  kubelets, through the projected volume (UPGRADING.md, "2026-10-12 (second): runner environment
  and guards", item 4).
* **Impersonation is a cluster-operator right.** Principals who may `impersonate` ServiceAccounts
  in the Argo namespace are cluster operators, like holders of `pods/exec` and `pods/attach`.
  Admission sees the impersonated name, not the impersonator, so no guard can tell them from the
  account they act as. The built-in `edit` and `admin` roles carry that verb; `UPGRADING.md` tells
  operators not to grant either in the Argo namespace to non-operators.
* **`workflowtaskresults`.** Any runner pod may create or patch any `WorkflowTaskResult` in the
  namespace. RBAC cannot narrow `create` by name, and Argo names results by node id at run time;
  Argo's own executor Role has the same shape. Recorded as a residue.
* **`system:kube-controller-manager` is trusted** for creates and garbage-collector updates of pods
  running as a trusted ServiceAccount. On clusters where kube-controller-manager does not use
  per-controller credentials, that identity is every built-in controller.
* **Mounts are resolved at fetch, not at submit.** The route asks the environment's plugin for its
  mounts when the runner fetches, while the pod's credential directories were fixed at submit. If
  the environment's mounts changed in between, the response no longer matches those directories
  and the fetch fails closed (`check_one_per_directory` in `deploy/runner/fetch_credentials.py`). A
  credential replaced under the same mount is served as it stands at fetch time.
* **Denial of service.** The credential route reads the token's claims, unverified, before it takes
  a `TokenReview` permit (`plausible` in `qa-environments/qa-environments/src/infra/pod_identity.rs`),
  so junk is shed without costing one, and a plausible request waits up to 5 s for a permit. A
  plausible-but-forged flood still competes for the 16 permits (`MAX_CONCURRENT_REVIEWS`). The
  runner's fetchers and the collect report refuse redirects.
* **Plain HTTP** is an open owner decision, recorded in DESIGN §3.14 "Accepted residues and open
  owner decisions".

### Confirmation

* `PluginFailure::detail` is `&'static str` in `qa-product-sdk`.
* `assert_no_leak` runs against both shipped plugins.
* `postgres-credstore-plugin`'s `leak_tests.rs` and `sea_orm_trace_exposure.rs` cover the storage
  side, where an ORM trace is the other way material escapes.
* `deploy/helm/tests/check_workflow_guard.py`: the chart refuses to render without the
  ValidatingAdmissionPolicy API, renders the guard and binding, gives `qa-platform-gears` no rights
  on Secrets, and keeps the CEL allowlist equal to qa-runs' set.
* `deploy/remote/verify-k8s.sh` `k8s 3/4`: as `qa-platform-gears`, a legitimate Workflow is
  admitted and a `secret` volume, `podSpecPatch`, another ServiceAccount, a `script` template, a
  CREATE carrying `status`, and an UPDATE of `volumes` or of `status` are denied (the two status
  cases only while the Workflow CRD has no status subresource); no new runner Secret appears in the
  Argo namespace. Check 6b's follow-up: the public origin answers 404 for
  `POST /qa/v1/runner-credentials`.
* `deploy/helm/tests/test_nginx_template.sh` pins the UI nginx's `location ^~
  /qa/v1/runner-credentials { return 404; }`.
* qa-environments' `pod_identity` tests: the workflow name comes from the one controller
  `argoproj.io` `Workflow` ownerReference, and a pod with none, a non-controller one, the wrong
  kind or group, two, or only the label is refused. qa-runs'
  `a_pod_claiming_another_tenant_is_not_found_even_under_a_covering_scope` pins the tenant
  comparison.
* qa-runs' `every_rendered_key_is_inside_the_admission_allowlist` renders three synthetic run
  shapes (a two-node DAG with a credential mount, a single node, and a collect-shaped run with no
  mounts) with the runner ServiceAccount configured, and checks every spec, template, DAG-task and
  volume key against the allowlist and that `spec.serviceAccountName` is present. It does not render
  the shipped plugins' own specs.
* qa-environments' `runner_credentials_tests`: every row of the error table, only the plugin's
  mounts returned, and no material in any error or log.
