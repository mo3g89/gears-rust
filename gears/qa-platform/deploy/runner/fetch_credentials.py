"""Fetch this pod's environment credentials and write them into tmpfs.

Finding #94: credentials used to arrive as Kubernetes Secret volumes that
qa-environments wrote into the Argo namespace. Now the pod asks for them
itself, with its projected ServiceAccount token (audience
qa-platform-runner-credentials). qa-environments verifies that token with a
TokenReview, checks the pod's uid, asks qa-runs whether this pod's run is live,
and returns the files the product plugin's prepare_run_access names.

Runs before fetch_bundle.py, so before any tenant-authored code. Writes only
under QA_CREDENTIAL_DIRS, which are memory-backed emptyDirs qa-runs rendered
for exactly these paths. Never prints the token or any file content.

Python's standard library only, as in fetch_bundle.py.

Environment:
  QA_CREDENTIALS_URL         POST target; unset means this run mounts nothing.
  QA_CREDENTIALS_TOKEN_FILE  the projected token's path.
  QA_CREDENTIAL_DIRS         ':'-separated directories files may land in.
  TEST_BUNDLE_CA_CERT        optional CA bundle, shared with fetch_bundle.py.
"""

import base64
import binascii
import json
import os
import ssl
import sys
import tempfile
import time
import urllib.error
import urllib.request

BACKOFF_SECONDS = (2, 5, 10)
# 403 is retried, within the same bounded backoff: qa-runs records a run's
# execution_ref only after the Argo submit returns, so a fast-starting pod can
# ask before its run is resolvable. 401 (no or unusable token) is final.
RETRYABLE = {403, 502, 503, 504}


def die(message):
    sys.stderr.write("fetch-credentials: %s\n" % message)
    sys.exit(1)


def ssl_context():
    ca = os.environ.get("TEST_BUNDLE_CA_CERT", "").strip()
    return ssl.create_default_context(cafile=ca) if ca else None


def request_files(url, token):
    attempts = len(BACKOFF_SECONDS) + 1
    for attempt in range(attempts):
        request = urllib.request.Request(url, data=b"", method="POST", headers={"X-QA-Runner-Token": token})
        try:
            with urllib.request.urlopen(request, timeout=60, context=ssl_context()) as response:
                body = response.read()
        except urllib.error.HTTPError as error:
            if error.code == 401 or (error.code == 403 and attempt == attempts - 1):
                die("credentials refused: HTTP %s after %d attempt(s). The pod's token, the pod or its run "
                    "was not accepted; the gears log (target qa_environments.runner_credentials) says which."
                    % (error.code, attempt + 1))
            if error.code not in RETRYABLE or attempt == attempts - 1:
                die("credentials request failed: HTTP %s" % error.code)
        except (urllib.error.URLError, OSError) as error:
            if attempt == attempts - 1:
                die("credentials request failed: %s" % type(error).__name__)
        else:
            # The body is credential material: a parse failure is reported by
            # its shape, never by echoing what was received.
            try:
                files = json.loads(body.decode("utf-8"))["files"]
            except (ValueError, KeyError, TypeError):
                die("credentials response is not the expected JSON object")
            if not isinstance(files, list):
                die("credentials response is not the expected JSON object")
            return files
        time.sleep(BACKOFF_SECONDS[attempt])
    die("credentials request failed")


def inside(path, directories):
    real = os.path.realpath(path)
    return any(os.path.dirname(real) == os.path.realpath(d) for d in directories)


def checked(entry, directories):
    """Validate one response entry without touching the filesystem.

    Returns (path, mode, content). Every refusal names the path at most, never
    the content.
    """
    if not isinstance(entry, dict):
        die("credentials response holds an entry that is not an object")
    path = entry.get("path")
    if not isinstance(path, str) or not path:
        die("credentials response holds an entry with no path")
    if not os.path.isabs(path) or not inside(path, directories):
        die("refusing to write %s: not inside QA_CREDENTIAL_DIRS" % path)
    mode = entry.get("mode")
    if mode is not None and (isinstance(mode, bool) or not isinstance(mode, int) or not 0 <= mode <= 0o7777):
        die("refusing to write %s: its mode is not a permission integer" % path)
    content_b64 = entry.get("content_b64")
    if not isinstance(content_b64, str):
        die("refusing to write %s: it has no content_b64 string" % path)
    try:
        content = base64.b64decode(content_b64, validate=True)
    except (binascii.Error, ValueError):
        die("refusing to write %s: its content is not valid base64" % path)
    return path, mode, content


def write(path, mode, content):
    directory = os.path.dirname(path)
    fd, temporary = tempfile.mkstemp(dir=directory, prefix=".credential-")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(content)
        if mode is not None:
            os.chmod(temporary, mode)
        os.replace(temporary, path)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


def check_one_per_directory(paths, directories):
    """Each declared directory is one credential mount, so it must receive
    exactly one file: a missing file would start the suite without a
    credential it was promised, and a second one has no mount to belong to."""
    for directory in directories:
        real = os.path.realpath(directory)
        count = sum(1 for path in paths if os.path.dirname(os.path.realpath(path)) == real)
        if count != 1:
            die("expected exactly one credential file in %s, the response has %d" % (directory, count))


def main():
    url = os.environ.get("QA_CREDENTIALS_URL", "").strip()
    if not url:
        print("fetch-credentials: no QA_CREDENTIALS_URL -- this run mounts no credentials")
        return
    token_file = os.environ.get("QA_CREDENTIALS_TOKEN_FILE", "").strip()
    if not token_file:
        die("QA_CREDENTIALS_TOKEN_FILE is unset")
    try:
        with open(token_file, encoding="utf-8") as handle:
            token = handle.read().strip()
    except OSError:
        die("cannot read the projected token at %s" % token_file)
    directories = [d for d in os.environ.get("QA_CREDENTIAL_DIRS", "").split(":") if d]
    files = request_files(url, token)
    # Everything is validated before anything is written, so a refused
    # response leaves the directories empty rather than half-filled.
    entries = [checked(entry, directories) for entry in files]
    check_one_per_directory([path for path, _, _ in entries], directories)
    for path, mode, content in entries:
        write(path, mode, content)
    print("fetch-credentials: wrote %d file(s)" % len(entries))


if __name__ == "__main__":
    main()
