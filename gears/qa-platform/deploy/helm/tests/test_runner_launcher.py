"""deploy/runner/launch.py: the runner's platform steps see only the
platform's own environment; the tenant's reaches pytest only. Before the
launcher, a run variable BASH_ENV made bash source a file of its choosing
before the credential fetch ran."""
import ast
import json
import os
import pathlib
import re
import stat
import subprocess
import sys

RUNNER = pathlib.Path(__file__).resolve().parents[2] / "runner"
LAUNCH = RUNNER / "launch.py"
WITH_TENANT_ENV = RUNNER / "with_tenant_env.py"


def launch(tmp_path, env):
    stub = tmp_path / "run.sh"
    seen = tmp_path / "seen.json"
    stub.write_text(
        "python3 -I -c 'import json,os,sys; json.dump(dict(os.environ), open(sys.argv[1], \"w\"))' "
        + str(seen) + "\n"
    )
    tenant_file = tmp_path / "tenant.json"
    base = {"PATH": os.environ["PATH"], "HOME": str(tmp_path)}
    proc = subprocess.run(
        [sys.executable, "-I", str(LAUNCH), str(stub), str(tenant_file)],
        env={**base, **env}, capture_output=True, text=True, timeout=30,
    )
    return proc, (json.loads(seen.read_text()) if seen.exists() else None), tenant_file


def test_bash_env_never_runs(tmp_path):
    marker = tmp_path / "pwned"
    proc, seen, _ = launch(tmp_path, {"BASH_ENV": "$(touch %s)" % marker})
    assert proc.returncode == 0, proc.stderr
    assert not marker.exists(), "BASH_ENV was expanded before the run script"
    assert "BASH_ENV" not in seen


def test_shell_startup_controls_do_not_reach_the_platform_steps(tmp_path):
    proc, seen, _ = launch(tmp_path, {"SHELLOPTS": "xtrace", "PS4": "$(id)", "ENV": "/x",
                                      "PYTHONSTARTUP": "/x", "LD_PRELOAD": "", "MY_VAR": "1"})
    assert proc.returncode == 0, proc.stderr
    for name in ("SHELLOPTS", "PS4", "ENV", "PYTHONSTARTUP", "LD_PRELOAD", "MY_VAR"):
        assert name not in seen, name


def test_platform_names_pass_and_the_tenant_file_is_announced(tmp_path):
    proc, seen, tenant_file = launch(tmp_path, {"QA_CREDENTIALS_URL": "http://gears/x",
                                                "TEST_BUNDLE_URL": "http://gears/b"})
    assert proc.returncode == 0, proc.stderr
    assert seen["QA_CREDENTIALS_URL"] == "http://gears/x"
    assert seen["TEST_BUNDLE_URL"] == "http://gears/b"
    assert seen["QA_RUNNER_TENANT_ENV"] == str(tenant_file)


def test_the_tenant_file_holds_the_whole_environment_and_is_private(tmp_path):
    _, _, tenant_file = launch(tmp_path, {"MY_VAR": "1", "E2E_X": "y"})
    tenant = json.loads(tenant_file.read_text())
    assert tenant["MY_VAR"] == "1" and tenant["E2E_X"] == "y"
    assert stat.S_IMODE(tenant_file.stat().st_mode) == 0o600


def test_with_tenant_env_applies_the_tenant_env_and_the_platform_wins(tmp_path):
    tenant_file = tmp_path / "tenant.json"
    tenant_file.write_text(json.dumps({"MY_VAR": "1", "PYTHONPATH": "/evil", "PATH": "/evil"}))
    out = tmp_path / "out.json"
    proc = subprocess.run(
        [sys.executable, "-I", str(WITH_TENANT_ENV), str(tenant_file), "--",
         sys.executable, "-I", "-c",
         "import json,os,sys; json.dump(dict(os.environ), open(sys.argv[1], 'w'))", str(out)],
        env={"PATH": os.environ["PATH"], "PYTHONPATH": "/platform"},
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0, proc.stderr
    seen = json.loads(out.read_text())
    assert seen["MY_VAR"] == "1"
    assert seen["PYTHONPATH"] == "/platform"
    assert seen["PATH"] == os.environ["PATH"]


def platform_names():
    text = LAUNCH.read_text()
    start = text.index("PLATFORM_NAMES = (")
    return ast.literal_eval(text[start + len("PLATFORM_NAMES = "):text.index(")", start) + 1])


# A direct read (os.environ[...], os.environ.get(...), os.getenv(...)) or an
# indirect one through a helper that takes the name, such as fetch_bundle.py's
# required("NAME").
ENV_READ = re.compile(
    r'(?:os\.environ(?:\.get)?[\[(]|os\.getenv\(|\brequired\()\s*"([A-Z0-9_]+)"')


def names_read(script):
    return set(ENV_READ.findall((RUNNER / script).read_text()))


def test_every_name_the_platform_steps_read_is_passed_to_them():
    names = set(platform_names())
    for script in ("fetch_credentials.py", "fetch_bundle.py", "bundle_layout.py"):
        read = names_read(script)
        assert read <= names, f"{script} reads {sorted(read - names)}, which launch.py does not pass"


def test_indirect_reads_are_collected():
    # fetch_bundle.py reads its URL through required(); a pattern that missed
    # it would let a new required("NAME") go unchecked.
    assert "TEST_BUNDLE_URL" in names_read("fetch_bundle.py")


DOCKERFILE = RUNNER / "runner.Dockerfile"
ENTRYPOINT_SH = RUNNER / "entrypoint.sh"


def test_the_image_runs_the_launcher_and_installs_the_run_script_behind_it():
    text = DOCKERFILE.read_text()
    copies = dict((dest, src) for src, dest in
                  re.findall(r"^COPY\s+(\S+)\s+(\S+)\s*$", text, flags=re.MULTILINE))
    assert copies.get("/entrypoint.sh", "").endswith("deploy/runner/launch.py"), copies
    assert copies.get("/opt/qa-runner/run.sh", "").endswith("deploy/runner/entrypoint.sh"), copies
    entrypoints = re.findall(r"^ENTRYPOINT\s+(.+?)\s*$", text, flags=re.MULTILINE)
    assert entrypoints and entrypoints[-1] == '["/entrypoint.sh"]', entrypoints
    # launch.py execs the run script at the path the image installs it to.
    assert 'RUN_SCRIPT = "/opt/qa-runner/run.sh"' in LAUNCH.read_text()


WRAPPER = 'python3 -I "$RUNNER_DIR/with_tenant_env.py" "$QA_RUNNER_TENANT_ENV" --'


def logical_commands(path):
    """The script's lines with backslash continuations joined and runs of
    whitespace collapsed, comments dropped."""
    commands, pending = [], ""
    for line in path.read_text().splitlines():
        if not pending and line.lstrip().startswith("#"):
            continue
        if line.rstrip().endswith("\\"):
            pending += line.rstrip()[:-1] + " "
            continue
        commands.append(" ".join((pending + line).split()))
        pending = ""
    return commands


def test_every_pytest_run_goes_through_the_tenant_env_wrapper():
    runs = [c for c in logical_commands(ENTRYPOINT_SH)
            if "python3 -m pytest" in c and "--version" not in c]
    assert runs, "entrypoint.sh runs pytest nowhere (moved? update this test)"
    for command in runs:
        assert f"{WRAPPER} python3 -m pytest" in command, (
            f"pytest runs without the tenant environment wrapper: {command}")
