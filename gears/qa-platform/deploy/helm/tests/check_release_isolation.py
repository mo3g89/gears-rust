"""Two releases of this chart must be able to coexist in one cluster without
selecting each other's pods.

# The trap this guard exists to catch

Every workload here used to select on `app.kubernetes.io/name` +
`app.kubernetes.io/component` only -- no `app.kubernetes.io/instance`. That
is fine for a single release, but it means a SECOND release of this chart
(a different `--set` config, a canary, a second tenant stand) installed
into the same namespace produces Deployments/StatefulSets whose selectors
are IDENTICAL to the first release's. Kubernetes does not care which
release "owns" a pod; it only looks at labels. Two Deployments with the
same selector each believe every pod matching it is theirs, and the last
one to reconcile wins -- pods get deleted and recreated in a fight neither
release's manifest describes, and `kubectl get pods` shows one release's
replica count with the other release's pod spec.

This is also WHY this is a chart major version rather than an in-place
change: a Deployment's/StatefulSet's `spec.selector` is immutable, so
`helm upgrade` of a release that predates this change fails outright with
`field is immutable` the moment `instance` is added to the selector. See
UPGRADING.md for the hand-run migration; this guard only proves the NEW
chart's selectors are correct, not that an old release can reach them
in-place.

THE SAME TRAP, ONE LAYER DOWN: the first cut of this guard checked only
Deployments/StatefulSets and shipped clean while all four Services'
`spec.selector` still carried no `instance` -- a Service's selector is
mutable, so nothing here would ever hit the "immutable field" error that
makes a workload mistake loud. It just quietly routes release A's traffic
(including release A's gears dialing what it thinks is its own Postgres)
to release B's pod too. This guard now checks Services the same way it
checks workloads, which is the only reason it would have caught its own
chart's bug.

# What this checks

1. Every Deployment/StatefulSet's `spec.selector.matchLabels`, and every
   Service's `spec.selector`, carries `app.kubernetes.io/instance` set to
   the release name it was rendered with -- reading the release name back
   out of the rendered object, not hardcoding it a second time, so a
   future edit that renders the label from the wrong scope (e.g. a
   sub-chart's `.Release.Name` shadowed by a `with`/`range`) still fails
   here even though it "looks" instance-scoped.
2. Rendering the SAME chart twice, once per release name, produces
   Deployments/StatefulSets/Services whose selectors are never equal to
   each other's -- the actual coexistence property, not just "the key
   exists".

Standalone script, like its siblings in this directory -- see the
Makefile's `helm-tests` target for why each one is invoked directly rather
than through pytest collection."""
import pathlib
import subprocess
import sys

import yaml

TESTS_DIR = pathlib.Path(__file__).resolve().parent
CHART = TESTS_DIR.parent / "qa-platform"
ORIGIN = "https://example-release-isolation-test.invalid"
WORKLOAD_KINDS = ("Deployment", "StatefulSet")
SELECTOR_KINDS = WORKLOAD_KINDS + ("Service",)


def render(release):
    """Return (docs, stderr, returncode) for `release`. Never asserts --
    callers decide."""
    out = subprocess.run(
        ["helm", "template", release, str(CHART),
         "--namespace", "qa-platform", "--set", f"publicOrigin={ORIGIN}",
         # keycloak.adminPassword has no default (WS3 Task 3) -- any value
         # that is not the literal "admin" satisfies the render.
         "--set", "keycloak.adminPassword=guard-fixture-not-a-real-password",
         # Both signing secrets have no default either (2026-09-21): the
         # per-render `randAlphaNum` fallback became a pod roll on every
         # upgrade once the gears Deployment started hashing the ConfigMap.
         "--set", "bundleDownloadSigningSecret=guard-fixture-not-a-real-bundle-key",
         "--set", "collectReportSigningSecret=guard-fixture-not-a-real-collect-key"],
        capture_output=True, text=True)
    if out.returncode != 0:
        return [], out.stderr, out.returncode
    return [d for d in yaml.safe_load_all(out.stdout) if d], out.stderr, 0


def selectables(docs):
    """Every rendered object that carries a selector this guard cares
    about: the two workload kinds plus Service."""
    return [d for d in docs if d.get("kind") in SELECTOR_KINDS]


def selector_of(doc):
    """A Deployment/StatefulSet's selector lives at
    spec.selector.matchLabels; a Service's is the flat map at
    spec.selector directly -- there is no matchLabels wrapper on a
    Service. Returns {} for anything else so callers can treat a missing
    selector as "no labels" rather than raising."""
    spec = doc.get("spec", {})
    if doc.get("kind") == "Service":
        return spec.get("selector", {}) or {}
    return spec.get("selector", {}).get("matchLabels", {})


def check_selector_carries_instance(failures):
    """Every workload's and every Service's selector carries
    app.kubernetes.io/instance set to the release it was rendered with --
    the property that makes the immutable-selector migration in
    UPGRADING.md necessary in the first place, so a future edit that drops
    this back out has to fail here, not be discovered at the next
    side-by-side install. Services are included on purpose: a Service's
    selector never trips the "field is immutable" error a workload's
    would, so nothing but this check would ever catch it missing
    `instance` there."""
    docs, stderr, rc = render("alpha")
    if rc != 0:
        failures.append(f"FAIL: a default render must succeed:\n{stderr}")
        return

    found_kinds = set()
    for doc in selectables(docs):
        kind = doc["kind"]
        found_kinds.add(kind)
        name = doc["metadata"]["name"]
        sel = selector_of(doc)
        if sel.get("app.kubernetes.io/instance") != "alpha":
            failures.append(
                f"FAIL: {kind}/{name}'s selector = {sel!r} does "
                "not carry app.kubernetes.io/instance: alpha. Without it, "
                "a second release in the same namespace selects this "
                f"{'workload' if kind != 'Service' else 'Service'}'s pods "
                "too.")

        if kind not in WORKLOAD_KINDS:
            # A Service has no pod template of its own to be a subset
            # of -- it selects a workload's pods, it does not carry any.
            continue

        tmpl_labels = (
            doc.get("spec", {}).get("template", {}).get("metadata", {}).get("labels", {})
        )
        missing_from_template = set(sel) - set(tmpl_labels)
        if missing_from_template:
            failures.append(
                f"FAIL: {kind}/{name}'s pod template labels {tmpl_labels!r} "
                f"are missing {sorted(missing_from_template)} that the "
                f"selector requires ({sel!r}). A selector that is not a "
                "subset of its own pod template's labels never matches its "
                "own pods.")

    missing_kinds = set(SELECTOR_KINDS) - found_kinds
    if missing_kinds:
        failures.append(
            f"FAIL: no {', '.join(sorted(missing_kinds))} rendered at all "
            "-- this chart is expected to produce at least gears, ui, "
            "keycloak and postgres as both a workload and a Service.")


def check_two_releases_never_collide(failures):
    """The actual coexistence property: render twice, with two different
    release names, and confirm no workload or Service in one release
    selects the same pods as any workload or Service in the other."""
    docs_a, stderr_a, rc_a = render("alpha")
    docs_b, stderr_b, rc_b = render("beta")
    if rc_a != 0 or rc_b != 0:
        failures.append(
            f"FAIL: a default render must succeed for both release names "
            f"(alpha rc={rc_a}, beta rc={rc_b}):\n{stderr_a}\n{stderr_b}")
        return

    collisions = []
    for doc_a in selectables(docs_a):
        sel_a = selector_of(doc_a)
        for doc_b in selectables(docs_b):
            sel_b = selector_of(doc_b)
            if sel_a and sel_a == sel_b:
                collisions.append(
                    f"{doc_a['kind']}/{doc_a['metadata']['name']} (release "
                    f"alpha) and {doc_b['kind']}/{doc_b['metadata']['name']} "
                    f"(release beta) share selector {sel_a!r}")

    if collisions:
        failures.append(
            "FAIL: two releases select the same pods:\n  " +
            "\n  ".join(collisions))


def main():
    failures = []
    check_selector_carries_instance(failures)
    check_two_releases_never_collide(failures)
    if failures:
        for line in failures:
            print(line, file=sys.stderr)
        return 1
    print(
        "PASS: every Deployment/StatefulSet/Service selector carries "
        "app.kubernetes.io/instance, and two releases never select the "
        "same pods")
    return 0


if __name__ == "__main__":
    sys.exit(main())
