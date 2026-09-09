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
* **Secrets travel as references.** `prepare_access` works from `credstore_ref` alone, so the
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

### Confirmation

* `PluginFailure::detail` is `&'static str` in `qa-product-sdk`.
* `assert_no_leak` runs against both shipped plugins.
* `postgres-credstore-plugin`'s `leak_tests.rs` and `sea_orm_trace_exposure.rs` cover the storage
  side, where an ORM trace is the other way material escapes.
