"""Fetch a bearer token by client credentials, then download and unpack a bundle.

Python's standard library only, on purpose: the runner image needs `pytest` and
nothing else, and every dependency added here is one more thing that can fail
to build on an air-gapped host. `curl`/`jq` are not in `python:3.12-slim` at
all, which is what ruled out the obvious shell version.

WHAT THIS DOES NOT DO: it does not read a Kubernetes Secret. The client secret
arrives as an environment variable that the KUBELET resolved from a
`secretKeyRef` the workflow named -- qa-runs never opens that Secret, which is
what keeps `run_executor.rs:85-86` true of the bundle path (see
`config.rs`'s `BundleAuthConfig`).

Usage: fetch_bundle.py <dest-dir>

Environment (all set by the Argo adapter's workflow template):
  TEST_BUNDLE_URL            GET <url> -> the tar.gz bytes. Required.
  TEST_BUNDLE_TOKEN_URL      OAuth2 token endpoint. Required.
  TEST_BUNDLE_CLIENT_ID      client_credentials client id. Required.
  TEST_BUNDLE_CLIENT_SECRET  its secret, from the secretKeyRef. Required.
  TEST_BUNDLE_CA_CERT        Path to a PEM bundle to trust for https. Optional;
                             needed only when either URL is https with a
                             private CA.
"""

import io
import json
import os
import ssl
import sys
import tarfile
import urllib.error
import urllib.parse
import urllib.request


def die(message):
    sys.stderr.write("fetch-bundle: %s\n" % message)
    sys.exit(1)


def required(name):
    value = os.environ.get(name, "").strip()
    if not value:
        die(
            "%s is unset or blank. The Argo adapter sets it from "
            "qa-runs.argo.bundle_base_url / bundle_auth; an unset "
            "TEST_BUNDLE_CLIENT_SECRET usually means the Kubernetes Secret "
            "named by bundle_auth.client_secret_secret has the wrong key."
            % name
        )
    return value


def ssl_context():
    ca = os.environ.get("TEST_BUNDLE_CA_CERT", "").strip()
    if not ca:
        return None
    if not os.path.isfile(ca):
        die("TEST_BUNDLE_CA_CERT=%s does not exist inside the pod" % ca)
    context = ssl.create_default_context()
    context.load_verify_locations(ca)
    return context


def token(context):
    url = required("TEST_BUNDLE_TOKEN_URL")
    body = urllib.parse.urlencode(
        {
            "grant_type": "client_credentials",
            "client_id": required("TEST_BUNDLE_CLIENT_ID"),
            "client_secret": required("TEST_BUNDLE_CLIENT_SECRET"),
        }
    ).encode("ascii")
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30, context=context) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        # The IdP's own error body is the only thing that distinguishes a wrong
        # secret (invalid_client) from a client with service accounts turned
        # off (unauthorized_client), so it is printed rather than swallowed. It
        # contains no credential.
        detail = ""
        try:
            detail = error.read().decode("utf-8", "replace")[:500]
        except Exception:  # noqa: BLE001 - diagnostics must not mask the cause
            pass
        die("token request to %s failed: HTTP %s %s" % (url, error.code, detail))
    except Exception as error:  # noqa: BLE001
        die("token request to %s failed: %s" % (url, error))

    access = payload.get("access_token")
    if not access:
        die("token response from %s carried no access_token" % url)
    # Deliberately NOT logging the token. The `expires_in` is logged instead so
    # a suite longer than the token's lifetime is diagnosable -- the download
    # happens once, up front, so that is a future problem and not this one.
    print(
        "fetch-bundle: got a token from %s (expires_in=%s)"
        % (url, payload.get("expires_in"))
    )
    return access


def download(access, context):
    url = required("TEST_BUNDLE_URL")
    request = urllib.request.Request(
        url, headers={"Authorization": "Bearer %s" % access}
    )
    try:
        with urllib.request.urlopen(request, timeout=300, context=context) as response:
            data = response.read()
    except urllib.error.HTTPError as error:
        hint = ""
        if error.code in (401, 403):
            hint = (
                " -- the token was rejected. 401 means the gears did not accept "
                "it (audience or issuer); 403/404 usually means its tenant_id "
                "claim names a tenant that does not own this bundle."
            )
        elif error.code == 404:
            hint = (
                " -- the bundle id is not known, or the bundle expired. Bundles "
                "are ephemeral; check qa-catalog's retention against how long "
                "the run sat queued."
            )
        die("bundle download from %s failed: HTTP %s%s" % (url, error.code, hint))
    except Exception as error:  # noqa: BLE001
        die("bundle download from %s failed: %s" % (url, error))
    print("fetch-bundle: downloaded %d bytes from %s" % (len(data), url))
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
    unpack(download(token(context), context), dest)


if __name__ == "__main__":
    main()
