"""report_collect (inline python in entrypoint.sh) must not follow a redirect:
its URL carries the `sig` that authorises the write, and a redirect would take
that signature to whatever host Location names. Every opener under
deploy/runner/ must also carry the no-redirect handler.
"""
import http.server
import pathlib
import re
import subprocess
import sys
import threading

RUNNER = pathlib.Path(__file__).resolve().parents[2] / "runner"
ENTRYPOINT = (RUNNER / "entrypoint.sh").read_text()


def report_collect_snippet():
    body = ENTRYPOINT.split("report_collect() {", 1)[1]
    return body.split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0]


def test_a_collect_report_redirect_is_refused_and_the_signature_does_not_follow(tmp_path):
    elsewhere_paths = []

    class Elsewhere(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            elsewhere_paths.append(self.path)
            self.send_response(200); self.end_headers()
        do_POST = do_GET
        def log_message(self, *args): pass

    elsewhere = http.server.HTTPServer(("127.0.0.1", 0), Elsewhere)

    class Redirect(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.send_response(302)
            self.send_header("Location", "http://127.0.0.1:%d/x?sig=abc" % elsewhere.server_port)
            self.end_headers()
        def log_message(self, *args): pass

    server = http.server.HTTPServer(("127.0.0.1", 0), Redirect)
    for s in (elsewhere, server):
        threading.Thread(target=s.serve_forever, daemon=True).start()
    script = tmp_path / "snippet.py"
    script.write_text(report_collect_snippet())
    try:
        # One retry after a fixed 5 s backoff, so this takes a few seconds.
        done = subprocess.run(
            [sys.executable, "-I", str(script), "tests/a.py", "3"],
            env={"VHP_COLLECT_URL": "http://127.0.0.1:%d/collect?sig=abc" % server.server_port},
            capture_output=True, text=True, timeout=60,
        )
    finally:
        server.shutdown(); elsewhere.shutdown()
    assert done.returncode != 0, done.stdout + done.stderr
    assert elsewhere_paths == [], "the redirect was followed"


def test_every_file_that_builds_an_opener_refuses_redirects():
    sources = {p.name: p.read_text() for p in RUNNER.glob("*.py")}
    sources["entrypoint.sh"] = ENTRYPOINT
    building = {n: t for n, t in sources.items() if "build_opener(" in t}
    assert len(building) >= 3, sorted(building)
    for name, text in building.items():
        assert "class NoRedirect(urllib.request.HTTPRedirectHandler)" in text, name
        assert "return None" in text and "NoRedirect()" in text, name
