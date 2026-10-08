"""Every base image is a tag PLUS a digest, and every downloaded binary is a
pinned version PLUS a verified SHA-256 (second review, finding #95).

What this guards, and what it cannot:

  Offline (the default, and what `make helm-tests` runs) -- reads EVERY
  `*Dockerfile` under deploy/ (discovered, not listed: a hand-kept list
  silently skipped qa-platform.Dockerfile's runtime stage) and refuses:
    * a FROM that is not `name:tag@sha256:<64 hex>`;
    * `stable.txt`, or any other floating-version lookup, on a non-comment line;
    * a KUBECTL/HELM/ISTIO version ARG that is not a literal version, or a
      *_SHA256 ARG that is not 64 hex characters;
    * a download (`curl`) whose archive is not followed by `sha256sum -c` on
      that same file against the matching ARG.
  It also renders the chart and refuses any third-party `image:` (anything but
  the first-party cf-gears-qa-platform, cf-gears-qa-platform-ui and
  qa-platform-pytest-runner, which are built and imported locally) that is not
  `repo:tag@sha256:<64 hex>`.
  It cannot tell that a well-formed digest or checksum is WRONG -- that needs
  the network.

  `--online` -- re-resolves every `name:tag` with `docker buildx imagetools
  inspect` and demands the digest recorded beside it, and re-fetches each
  published checksum and demands the recorded one. Run it whenever a pin is
  bumped; it is what proves a bump was not a typo. It is not in `make
  helm-tests` because CI's lint job should not fail on a registry outage.

Bump procedure: the PINS block in runner/runner.Dockerfile and the BASE IMAGES
block in docker/qa-platform-ui.Dockerfile."""
import pathlib
import re
import subprocess
import sys
import urllib.request

DEPLOY = pathlib.Path(__file__).resolve().parent.parent.parent
RUNNER = DEPLOY / "runner" / "runner.Dockerfile"
DOCKERFILES = sorted(DEPLOY.rglob("*Dockerfile"))
CHART = DEPLOY / "helm" / "qa-platform"
FIRST_PARTY = ("cf-gears-qa-platform", "cf-gears-qa-platform-ui", "qa-platform-pytest-runner")
# The six values with no default (see ci.yml's helm lint step).
RENDER_SETS = ["publicOrigin=https://example-pins.invalid",
               "keycloak.adminPassword=pins-fixture-not-a-real-password",
               "bundleDownloadSigningSecret=pins-fixture-not-a-real-bundle-key",
               "collectReportSigningSecret=pins-fixture-not-a-real-collect-key",
               "argo.workflowClientSecret=pins-fixture-not-a-real-workflow-secret",
               "postgres.password=pins-fixture-not-a-real-db-password"]
CHART_IMAGE_RE = re.compile(r"^(?P<name>[^\s:@]+(?::\d+)?/?[^\s:@]*):(?P<tag>[^\s@]+)@(?P<digest>sha256:[0-9a-f]{64})$")
IMAGE_LINE = re.compile(r"^\s*(?:-\s*)?image:\s*[\"']?([^\"'\s]+)[\"']?\s*$")

FROM_RE = re.compile(r"^FROM\s+(?P<name>[^\s:@]+):(?P<tag>[^\s@]+)@(?P<digest>sha256:[0-9a-f]{64})(\s+AS\s+\S+)?\s*$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
VERSION = re.compile(r"^v?\d+\.\d+\.\d+$")
FLOATING = ("stable.txt", "latest.txt", "/latest/", "releases/latest")

# archive filename in the Dockerfile -> (version ARG, checksum ARG, checksum URL)
DOWNLOADS = {
    "kubectl": ("KUBECTL_VERSION", "KUBECTL_SHA256",
                "https://dl.k8s.io/release/{v}/bin/linux/amd64/kubectl.sha256"),
    "helm.tar.gz": ("HELM_VERSION", "HELM_SHA256",
                    "https://get.helm.sh/helm-{v}-linux-amd64.tar.gz.sha256sum"),
    "istioctl.tar.gz": ("ISTIO_VERSION", "ISTIO_SHA256",
                        "https://github.com/istio/istio/releases/download/{v}/"
                        "istioctl-{v}-linux-amd64.tar.gz.sha256"),
}


def code_lines(path):
    """Non-comment lines, with `\\` continuations left as-is."""
    return [(n, l) for n, l in enumerate(path.read_text().splitlines(), 1)
            if not l.lstrip().startswith("#")]


def args_of(path):
    return {m.group(1): m.group(2)
            for _, l in code_lines(path)
            if (m := re.match(r"^ARG\s+(\w+)=(\S+)\s*$", l))}


def chart_images():
    """Every `image:` the rendered chart carries, as (template, reference)."""
    cmd = ["helm", "template", "qa-platform", str(CHART), "--namespace", "qa-platform"]
    for v in RENDER_SETS:
        cmd += ["--set", v]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        return None, r.stderr.strip()
    found, src = [], "?"
    for line in r.stdout.splitlines():
        if m := re.match(r"^# Source: (\S+)", line):
            src = m.group(1).split("/", 1)[-1]
        elif m := IMAGE_LINE.match(line):
            found.append((src, m.group(1)))
    return found, ""


def check_chart(failures, images):
    found, err = chart_images()
    if found is None:
        failures.append(f"chart does not render with the six required values: {err}")
        return
    if not found:
        failures.append("the rendered chart carries no `image:` -- the guard would pass on nothing")
    for src, ref in found:
        repo = ref.split("@")[0].rsplit(":", 1)[0].rsplit("/", 1)[-1]
        if repo in FIRST_PARTY:
            continue
        m = CHART_IMAGE_RE.match(ref)
        if not m:
            failures.append(f"{src}: image `{ref}` is third-party and must be "
                            "`repo:tag@sha256:<64 hex>` (images.<name>.digest in values.yaml)")
        else:
            images.append((pathlib.Path(src), m["name"], m["tag"], m["digest"]))


def check_offline():
    failures, images = [], []
    if not DOCKERFILES:
        failures.append("no *Dockerfile found under deploy/ -- the guard would pass on nothing")
    for df in DOCKERFILES:
        rel = df.relative_to(DEPLOY)
        froms = [(n, l) for n, l in code_lines(df) if l.startswith("FROM ")]
        if not froms:
            failures.append(f"{rel}: no FROM line found -- the guard would pass on nothing")
        for n, l in froms:
            m = FROM_RE.match(l)
            if not m:
                failures.append(f"{rel}: `{l.strip()}` must be `name:tag@sha256:<64 hex>` "
                                "(tag for the reader, digest for the pin)")
            else:
                images.append((rel, m["name"], m["tag"], m["digest"]))
        for n, l in code_lines(df):
            for token in FLOATING:
                if token in l:
                    failures.append(f"{rel}: `{token}` is a floating-version lookup: {l.strip()}")

    check_chart(failures, images)

    args = args_of(RUNNER)
    body = "\n".join(l for _, l in code_lines(RUNNER))
    for archive, (vname, sname, _) in DOWNLOADS.items():
        if not VERSION.match(args.get(vname, "")):
            failures.append(f"runner.Dockerfile: ARG {vname}={args.get(vname)!r} is not a literal version")
        if not HEX64.match(args.get(sname, "")):
            failures.append(f"runner.Dockerfile: ARG {sname}={args.get(sname)!r} is not 64 hex characters")
        verify = re.compile(r'echo\s+"\$\{%s\}\s+%s"\s*\|\s*sha256sum\s+-c\s+-' % (sname, re.escape(archive)))
        if not verify.search(body):
            failures.append(f"runner.Dockerfile: `{archive}` is downloaded but never checked with "
                            f'`echo "${{{sname}}}  {archive}" | sha256sum -c -`')
        # The verification must come after the download and before the install/untar.
        d = body.find(f"-o {archive}") if archive != "kubectl" else body.find("curl -fsSLO")
        v = verify.search(body)
        if d == -1:
            failures.append(f"runner.Dockerfile: no download of `{archive}` found")
        elif v and v.start() < d:
            failures.append(f"runner.Dockerfile: `{archive}` is verified before it is downloaded")
    return failures, images, args


def check_online(images, args):
    failures = []
    for rel, name, tag, digest in images:
        ref = f"{name}:{tag}"
        out = subprocess.run(["docker", "buildx", "imagetools", "inspect", ref],
                             capture_output=True, text=True)
        m = re.search(r"^Digest:\s+(sha256:[0-9a-f]{64})", out.stdout, re.M)
        if out.returncode != 0 or not m:
            failures.append(f"{rel}: could not resolve {ref}: {out.stderr.strip()[:200]}")
        elif m.group(1) != digest:
            failures.append(f"{rel}: {ref} resolves to {m.group(1)}, but the file pins {digest}")
    for archive, (vname, sname, url) in DOWNLOADS.items():
        u = url.format(v=args[vname])
        try:
            published = urllib.request.urlopen(u, timeout=30).read().decode().split()[0]
        except Exception as e:  # noqa: BLE001 - any fetch failure is a guard failure
            failures.append(f"could not fetch the published checksum {u}: {e}")
            continue
        if published != args[sname]:
            failures.append(f"{sname}={args[sname]} but {u} publishes {published}")
    return failures


def main():
    online = "--online" in sys.argv[1:]
    failures, images, args = check_offline()
    if online and not failures:
        failures += check_online(images, args)
    if failures:
        for f in failures:
            print(f"FAIL: {f}")
        return 1
    print(f"PASS: {len(DOCKERFILES)} Dockerfiles and the rendered chart: {len(images)} images are tag@digest, 3 downloads are version + "
          f"sha256sum -c, no floating lookups" + (" (online: all resolved and matched)" if online else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
