"""Every login pin must derive from ONE value: .Values.publicOrigin.

The pins, and what breaks when one drifts:
  * Keycloak KC_HOSTNAME      -> tokens carry an `iss` the gears reject (401)
  * gears issuer_pattern      -> same, from the other side
  * realm webOrigins          -> "Invalid parameter: redirect_uri"
  * certificate SAN           -> a browser refusal MID-REDIRECT, which reads as
                                 a broken login rather than a certificate warning
The UI bundle's VITE_OIDC_ISSUER is the fifth pin and is NOT checkable here --
vite inlines it at image build. verify-k8s.sh greps the deployed bundle for it."""
import subprocess
import sys
import pathlib
import yaml

CHART = pathlib.Path(__file__).resolve().parent.parent / "qa-platform"
ORIGIN = "https://example-pin-test.invalid"
HOST = "example-pin-test.invalid"


def render(origin):
    out = subprocess.run(
        ["helm", "template", "qa-platform", str(CHART),
         "--namespace", "qa-platform", "--set", f"publicOrigin={origin}"],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    return [d for d in yaml.safe_load_all(out.stdout) if d]


def main():
    docs = render(ORIGIN)
    failures = []

    kc = [d for d in docs if d["kind"] == "Deployment" and "keycloak" in d["metadata"]["name"]][0]
    env = {e["name"]: e.get("value") for e in kc["spec"]["template"]["spec"]["containers"][0]["env"]}
    if env.get("KC_HOSTNAME") != ORIGIN:
        failures.append(f"KC_HOSTNAME={env.get('KC_HOSTNAME')!r}, expected {ORIGIN}")
    if "KC_HTTP_RELATIVE_PATH" in env:
        failures.append("KC_HTTP_RELATIVE_PATH must not be set -- entrypoint.sh:137 refuses "
                        "a PUBLIC_ISSUER_ORIGIN with a path, and the SPA owns /auth/callback")
    if any(k.startswith("KC_HTTPS_") for k in env):
        failures.append(f"Keycloak must not terminate TLS: {[k for k in env if k.startswith('KC_HTTPS_')]}")

    # PUBLIC_HOST is set on the gen-cert initContainer (the container that
    # actually calls gen-cert.sh) -- the Job's only regular container
    # (apply-secret) just runs `kubectl apply` and carries no env at all.
    certs = [d for d in docs if d["kind"] == "Job" and "cert" in d["metadata"]["name"]][0]
    cenv = {e["name"]: e.get("value")
            for e in certs["spec"]["template"]["spec"]["initContainers"][0]["env"]}
    if cenv.get("PUBLIC_HOST") != HOST:
        failures.append(f"cert PUBLIC_HOST={cenv.get('PUBLIC_HOST')!r}, expected {HOST}")

    gears = [d for d in docs if d["kind"] == "Deployment" and "gears" in d["metadata"]["name"]][0]
    genv = {e["name"]: e.get("value")
            for e in gears["spec"]["template"]["spec"]["containers"][0]["env"]}
    # entrypoint.sh:137 accepts ONLY a bare origin and appends /realms/qa-platform
    # itself. Anything with a path here is a refusal to start, not a 401.
    issuer = genv.get("PUBLIC_ISSUER_ORIGIN", "")
    if issuer != ORIGIN:
        failures.append(f"gears PUBLIC_ISSUER_ORIGIN={issuer!r}, expected the bare origin "
                        f"{ORIGIN} (entrypoint.sh:137 refuses any path)")

    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        return 1
    # THREE positive pins (KC_HOSTNAME, the certs Job's PUBLIC_HOST, the gears'
    # PUBLIC_ISSUER_ORIGIN) plus TWO negative assertions (no
    # KC_HTTP_RELATIVE_PATH, no KC_HTTPS_*). The old wording said "all four
    # renderable pins", which matched neither count and made the docstring's
    # four-row table look like the thing being checked -- it is not: the realm
    # webOrigins row is checked by verify-k8s.sh, not here.
    print(f"PASS: 3 renderable pins derive from publicOrigin={ORIGIN}, "
          f"and 2 negative assertions hold (no KC_HTTP_RELATIVE_PATH, no KC_HTTPS_*)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
