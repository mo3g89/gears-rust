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
* Bad, because **a compromised gears process can still read any Secret in the Argo namespace, and
  the runner-`Secret` writer is contained only where the cluster supports admission policies**
  (second review, finding #94).

  The writer is qa-environments' runner-`Secret` writer (labelled `D4` in code comments). It needs `create` and `patch` on Secrets in
  the Argo namespace, which also holds Argo's own Secrets. RBAC cannot narrow that grant: `create`
  ignores `resourceNames`, and the names `patch` would need are derived per tenant and per
  credential at runtime.

  What changed is who holds the grant:
  - It used to belong to `qa-platform-gears`, the ambient ServiceAccount of every gear in the pod,
    qa-runs included, even though qa-runs never needs a Secret.
  - It now belongs to a single-purpose `qa-platform-secret-writer` ServiceAccount, whose token only
    the D4 writer is configured to read. Its requests are distinguishable in the API server's audit
    log, and its token Secret can be deleted to revoke it on its own.

  That token is mounted into the same gears container, so a compromised gears process holds the
  writer's grant too. What the grant allows depends on the cluster:
  - Where `admissionregistration.k8s.io/v1` ValidatingAdmissionPolicy is served (Kubernetes 1.30
    and later), the chart's `qa-platform-secret-writer-guard-<release namespace>` policy admits only
    `Opaque` Secrets whose names start with the runner prefix `qa-platform-`. The writer can still
    create such a Secret, or overwrite any tenant's runner-credential Secret by name, because they
    all share that prefix. It cannot touch any other Secret, and cannot read, list or delete one.
  - Where the API is not served, the policy does not render, and the writer can create any Secret
    in the namespace or overwrite any existing one by name. That includes minting a
    `kubernetes.io/service-account-token` Secret for any ServiceAccount there, Argo's controller and
    server accounts included, which the token controller then fills with a working token.

  **The remaining gap is `qa-platform-gears`' own `workflows` `create`.** qa-runs needs that grant to
  submit runs, so it stays with the pod's identity. A compromised gears process can use it to submit
  a Workflow whose pod mounts any Secret in the Argo namespace and runs as any ServiceAccount there.
  Through that Workflow it can read every Secret in the namespace, including any token the writer
  minted. This change does not close that. Closing it needs admission control over the Workflows
  qa-runs submits (a pinned `serviceAccountName`, and Secret volumes limited to the runner prefix),
  or the writer and executor in separate processes with separate identities.

  Accepted, and recorded here for the same reason as the entry above.

  *(Added 2026-09-30, second review, finding #94.)*

### Confirmation

* `PluginFailure::detail` is `&'static str` in `qa-product-sdk`.
* `assert_no_leak` runs against both shipped plugins.
* `postgres-credstore-plugin`'s `leak_tests.rs` and `sea_orm_trace_exposure.rs` cover the storage
  side, where an ORM trace is the other way material escapes.
