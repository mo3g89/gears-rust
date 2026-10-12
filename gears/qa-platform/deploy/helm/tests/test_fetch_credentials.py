"""fetch_credentials.main() writes only where qa-runs said it may, and never
prints the token (finding #94).

Each test serves a finite, scripted list of responses from a local HTTP server.
"""
import base64, http.server, importlib.util, json, os, pathlib, stat, threading
import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[2] / "runner"
spec = importlib.util.spec_from_file_location("fetch_credentials", RUNNER / "fetch_credentials.py")
fc = importlib.util.module_from_spec(spec); spec.loader.exec_module(fc)


def serve(responses, seen):
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            # The headers object itself, not dict(self.headers): it looks names
            # up case-insensitively, and urllib sends `X-qa-runner-token`.
            seen.append(self.headers)
            code, body = responses.pop(0)
            self.send_response(code); self.send_header("Content-Type", "application/json"); self.end_headers()
            self.wfile.write(json.dumps(body).encode())
        def log_message(self, *args): pass
    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, "http://127.0.0.1:%d/qa/v1/runner-credentials" % server.server_port


def env(tmp_path, url, dirs):
    token = tmp_path / "token"; token.write_text("TOKEN-CANARY\n")
    return {"QA_CREDENTIALS_URL": url, "QA_CREDENTIALS_TOKEN_FILE": str(token), "QA_CREDENTIAL_DIRS": ":".join(map(str, dirs))}


def file(path, mode, content):
    return {"path": str(path), "mode": mode, "content_b64": base64.b64encode(content).decode()}


def test_writes_each_file_with_its_mode_and_sends_the_token_header(tmp_path, monkeypatch, capsys):
    d = tmp_path / "kube"; d.mkdir()
    seen = []
    server, url = serve([(200, {"files": [file(d / "kubeconfig", 0o400, b"KUBE")]})], seen)
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    fc.main()
    assert (d / "kubeconfig").read_bytes() == b"KUBE"
    assert stat.S_IMODE((d / "kubeconfig").stat().st_mode) == 0o400
    assert seen[0]["X-QA-Runner-Token"] == "TOKEN-CANARY"
    captured = capsys.readouterr()
    assert "TOKEN-CANARY" not in captured.out + captured.err
    server.shutdown()


def test_refuses_a_path_outside_the_declared_directories(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    server, url = serve([(200, {"files": [file(tmp_path / "elsewhere", None, b"x")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    assert not (tmp_path / "elsewhere").exists()
    server.shutdown()


def test_refuses_dot_dot_escapes(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    server, url = serve([(200, {"files": [file(str(d) + "/../escape", None, b"x")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit):
        fc.main()
    assert not (tmp_path / "escape").exists()
    server.shutdown()


def test_retries_503_then_succeeds(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    monkeypatch.setattr(fc, "BACKOFF_SECONDS", (0, 0, 0))
    server, url = serve([(503, {}), (200, {"files": [file(d / "k", None, b"ok")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    fc.main()
    assert (d / "k").read_bytes() == b"ok"
    server.shutdown()


def test_does_not_retry_a_401(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    monkeypatch.setattr(fc, "BACKOFF_SECONDS", (0, 0, 0))
    seen = []
    server, url = serve([(401, {}), (200, {"files": []})], seen)
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    assert len(seen) == 1
    server.shutdown()


def test_retries_a_403_up_to_the_attempt_limit(tmp_path, monkeypatch, capsys):
    # A pod can start before qa-runs records the run's execution_ref, and get
    # a transient 403 for it; a lasting one still ends the run.
    d = tmp_path / "kube"; d.mkdir()
    monkeypatch.setattr(fc, "BACKOFF_SECONDS", (0, 0, 0))
    seen = []
    server, url = serve([(403, {})] * 5, seen)
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    assert len(seen) == len(fc.BACKOFF_SECONDS) + 1
    assert "403" in capsys.readouterr().err
    server.shutdown()


def test_a_transient_403_then_success_writes_the_file(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    monkeypatch.setattr(fc, "BACKOFF_SECONDS", (0, 0, 0))
    server, url = serve([(403, {}), (200, {"files": [file(d / "k", None, b"ok")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    fc.main()
    assert (d / "k").read_bytes() == b"ok"
    server.shutdown()


def test_refuses_fewer_files_than_declared_directories(tmp_path, monkeypatch, capsys):
    a = tmp_path / "a"; a.mkdir()
    b = tmp_path / "b"; b.mkdir()
    server, url = serve([(200, {"files": [file(a / "k", None, b"SECRET-CONTENT")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [a, b]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    captured = capsys.readouterr()
    assert "SECRET-CONTENT" not in captured.out + captured.err
    server.shutdown()


def test_refuses_two_files_in_one_directory(tmp_path, monkeypatch):
    d = tmp_path / "kube"; d.mkdir()
    server, url = serve([(200, {"files": [file(d / "k1", None, b"x"), file(d / "k2", None, b"y")]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    server.shutdown()


@pytest.mark.parametrize("entry", [
    {"mode": None, "content_b64": "eA=="},                       # no path
    {"path": "PATH", "mode": None},                               # no content
    {"path": "PATH", "mode": "0400", "content_b64": "eA=="},     # mode not an int
    {"path": "PATH", "mode": True, "content_b64": "eA=="},       # bool is not a mode
    "not-an-object",
])
def test_a_malformed_entry_is_a_clean_refusal(tmp_path, monkeypatch, capsys, entry):
    d = tmp_path / "kube"; d.mkdir()
    if isinstance(entry, dict) and entry.get("path") == "PATH":
        entry = dict(entry, path=str(d / "k"))
    server, url = serve([(200, {"files": [entry]})], [])
    monkeypatch.setattr(os, "environ", env(tmp_path, url, [d]))
    with pytest.raises(SystemExit) as exit_info:
        fc.main()
    assert exit_info.value.code == 1
    captured = capsys.readouterr()
    assert "Traceback" not in captured.err
    assert "eA==" not in captured.out + captured.err
    assert not (d / "k").exists()
    server.shutdown()


def test_no_url_means_nothing_to_fetch(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(os, "environ", {})
    fc.main()
    assert "no QA_CREDENTIALS_URL" in capsys.readouterr().out
