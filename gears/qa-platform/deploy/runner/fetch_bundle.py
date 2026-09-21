"""Download and unpack this node's test bundle. No credential, by design.

Python's standard library only, on purpose: the runner image needs `pytest` and
nothing else, and every dependency added here is one more thing that can fail
to build on an air-gapped host. `curl`/`jq` are not in `python:3.12-slim` at
all, which is what ruled out the obvious shell version.

THERE IS NO TOKEN EXCHANGE HERE ANY MORE, AND THAT IS THE POINT.

This file used to open with a `token()` function that performed a
`client_credentials` grant against Keycloak from inside this pod, using
TEST_BUNDLE_CLIENT_SECRET, and then presented the resulting bearer token on the
bundle route. That credential was the confidential secret of a deployment-wide
`fullScopeAllowed` service-account client with a HARDCODED tenant_id claim, and
it sat in the environment of a process tree whose whole job is to execute
TENANT-AUTHORED pytest. That test code could read it, mint its own tokens and
call every authenticated route in all four gears. The pod's NetworkPolicy did
not help: it deliberately allow-listed both Keycloak and the gears API, because
this exchange needed both.

What replaces it is already in TEST_BUNDLE_URL: qa-catalog signs a per-bundle
HMAC tag at build time and the Argo adapter renders it into the URL's `?sig=`.
It authorises exactly one bundle -- this tar.gz, which this pod is about to
unpack anyway -- and nothing else. Being hardcoded to one tenant was also a live
bug: every tenant but the seeded one got a 404 on its own bundles. The signature
carries the bundle's own tenant, so that is fixed as a side effect.

BECAUSE THE URL NOW CONTAINS A SECRET, IT IS NEVER LOGGED WHOLE. Every print
below goes through `redacted()`. The log line this script writes is echoed into
the run view, so a URL printed verbatim would publish the tag to everyone who
can read that run.

Usage: fetch_bundle.py <dest-dir>

Environment (all set by the Argo adapter's workflow template):
  TEST_BUNDLE_URL            GET <url> -> the tar.gz bytes. Required. Carries
                             the `?sig=` that authorises the request.
  TEST_BUNDLE_CA_CERT        Path to a PEM bundle to trust for https. Optional;
                             needed only when the URL is https with a
                             private CA.
"""

import io
import os
import ssl
import sys
import tarfile
import urllib.error
import urllib.request


def die(message):
    sys.stderr.write("fetch-bundle: %s\n" % message)
    sys.exit(1)


def required(name):
    value = os.environ.get(name, "").strip()
    if not value:
        die(
            "%s is unset or blank. The Argo adapter sets it from "
            "qa-runs.argo.bundle_base_url; an unset TEST_BUNDLE_URL means that "
            "setting is empty, so this pod was given no way to fetch its tests."
            % name
        )
    return value


def redacted(url):
    """`url` with its query string replaced by a placeholder.

    THE ONLY FORM OF TEST_BUNDLE_URL THAT MAY BE PRINTED. The query string
    carries `sig`, the HMAC tag that is the whole access control on the bundle
    route, and everything this script prints lands in the pod log, which the run
    view renders to anyone who can read that run.

    A placeholder rather than a bare truncation, so a reader can still tell
    "the adapter gave us no signature at all" (no marker) apart from "there is
    one and it is hidden" -- the two produce different failures and the first is
    a misconfiguration this line is the only witness to.
    """
    base, sep, _query = url.partition("?")
    return base + ("?<redacted>" if sep else "")


def ssl_context():
    ca = os.environ.get("TEST_BUNDLE_CA_CERT", "").strip()
    if not ca:
        return None
    if not os.path.isfile(ca):
        die("TEST_BUNDLE_CA_CERT=%s does not exist inside the pod" % ca)
    context = ssl.create_default_context()
    context.load_verify_locations(ca)
    return context


def download(context):
    url = required("TEST_BUNDLE_URL")
    # No Authorization header. The `?sig=` already in `url` is the credential,
    # and it authorises this one bundle -- see this module's header.
    request = urllib.request.Request(url)
    try:
        with urllib.request.urlopen(request, timeout=300, context=context) as response:
            data = response.read()
    except urllib.error.HTTPError as error:
        hint = ""
        if error.code == 403:
            hint = (
                " -- the signature in TEST_BUNDLE_URL did not verify. All three "
                "causes answer with this same status on purpose (so the response "
                "is not a guessing oracle), and qa-catalog's "
                "qa_catalog_bundle_download_total metric is where they are told "
                "apart: (a) the deployment has no qa-catalog."
                "bundle_download_signing_secret set, which fails EVERY download "
                "closed and is by far the most likely cause on a new stand; "
                "(b) that secret was rotated after this run was dispatched but "
                "before this pod started; (c) the URL was truncated or edited."
            )
        elif error.code == 404:
            hint = (
                " -- the bundle id is not known, or the bundle expired. Bundles "
                "are ephemeral; check qa-catalog's retention against how long "
                "the run sat queued."
            )
        elif error.code == 401:
            hint = (
                " -- unexpected: this route is anonymous and presents no bearer "
                "token. A 401 means the request reached something other than "
                "qa-catalog's bundle route (a proxy, or a gears build that still "
                "registers it .authenticated())."
            )
        die(
            "bundle download from %s failed: HTTP %s%s"
            % (redacted(url), error.code, hint)
        )
    except Exception as error:  # noqa: BLE001
        die("bundle download from %s failed: %s" % (redacted(url), error))
    print("fetch-bundle: downloaded %d bytes from %s" % (len(data), redacted(url)))
    return data


def unpack(data, dest):
    # `r:gz` and not `r:*`: the route serves `application/gzip`, and accepting
    # any format would turn "the server sent us an HTML error page with a 200"
    # into an obscure tar error instead of a clear one.
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r:gz")
    except tarfile.TarError as error:
        die(
            "the downloaded bytes are not a gzip tar archive (%s). First 200 "
            "bytes: %r" % (error, data[:200])
        )
    names = archive.getnames()
    for name in names:
        # The archive comes from a trusted service, but path traversal in a tar
        # is cheap to check and expensive to discover: a `../` entry would write
        # outside the work directory, and Python only started refusing that by
        # default in 3.14.
        if name.startswith("/") or ".." in name.split("/"):
            die("archive entry %r escapes the extraction directory" % name)
    archive.extractall(dest)
    archive.close()
    print(
        "fetch-bundle: unpacked %d entries into %s: %s"
        % (len(names), dest, ", ".join(sorted(names)[:20]))
    )
    return names


def main():
    if len(sys.argv) != 2:
        die("usage: fetch_bundle.py <dest-dir>")
    dest = sys.argv[1]
    os.makedirs(dest, exist_ok=True)
    context = ssl_context()
    unpack(download(context), dest)


if __name__ == "__main__":
    main()
