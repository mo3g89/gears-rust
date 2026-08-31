"""Where in an unpacked bundle does pytest have to be run, and with what.

WHY THIS EXISTS. A bundle is the repository's *content root*, not a flat pile of
test files (`qa-catalog/.../domain/service/bundles.rs`, `build_bundle` — changed
2026-08-27 for exactly this reason). A real suite therefore arrives with its own
`pytest.ini`, `conftest.py`, `requirements.txt` and shared packages, and those
sit at whatever depth the repository put them: the first one to reach this
runner keeps them one level down, in `<bundle>/tests/`, because the repository's
`content_root` is `tests/e2e` and its suite lives in `tests/e2e/tests/`.

Three things follow, and all three were wrong before this file existed:

1. **pytest must run from the config file's directory**, or its `rootdir` is the
   bundle root, the ini file is never read, and every `pytest.mark.<x>` the suite
   uses becomes a `PytestUnknownMarkWarning`.
2. **That directory must be on `sys.path`.** The suite imports its shared code as
   a top-level package (`from lib.monitoring.client import ...`) and `lib/` has
   no `__init__.py`, so it resolves only as a namespace package found on
   `sys.path`. Neither the CWD nor `rootdir` is added to `sys.path` by pytest;
   what is added is the *basedir* of each test module, which for
   `tests/monitoring/test_x.py` (no `__init__.py` in `monitoring/`) is
   `tests/monitoring` and does not contain `lib`.
3. **The requirements the repository declares are next to its config**, not at
   the bundle root.

NOTHING IS HARDCODED TO `tests`. The directory is derived by search, so a bundle
whose root *is* the config directory (the round-1 canary fixture, and the smoke
fixture) resolves to the root and behaves exactly as before.

OUTPUT is `KEY=value` lines for `eval` in the entrypoint, values shell-quoted.
Printing assignments rather than exporting from Python because the caller has to
`cd` and to build a pytest argv, which a child process cannot do for it.
"""

import os
import shlex
import sys

# In pytest's own precedence order (`_pytest/config/findpaths.py`,
# `locate_config`): the first name found in a directory decides that directory's
# config, so a repository with both `pytest.ini` and a `[tool.pytest]` section in
# `pyproject.toml` behaves here as pytest would.
#
# `None` means "the file's mere presence is the config"; a string means the file
# only counts when it contains that section header, which is pytest's rule too --
# a `setup.cfg` with no `[tool:pytest]` is not a pytest config and must not
# decide the rootdir.
CONFIG_FILES = (
    ("pytest.ini", None),
    ("pyproject.toml", "[tool.pytest.ini_options]"),
    ("tox.ini", "[pytest]"),
    ("setup.cfg", "[tool:pytest]"),
)

# Requirements filenames, in the order they are tried. Only the FIRST match in a
# directory is installed: a repository that ships both `requirements.txt` and
# `requirements-test.txt` means the second to be a subset or a superset, and
# installing both is as likely to conflict as to help.
REQUIREMENTS_FILES = ("requirements.txt", "requirements-test.txt", "requirements-dev.txt")

# Directories never worth descending into when looking for a config file. A
# bundle should not contain these at all (qa-catalog excludes them), but a
# `.venv` or `node_modules` that slipped in would otherwise be able to win the
# search with its own `setup.cfg`.
SKIP_DIRS = frozenset(
    (".git", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox", ".venv", "node_modules")
)


def _has_section(path, section):
    if section is None:
        return True
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if line.strip() == section:
                    return True
    except OSError:
        return False
    return False


def find_config(root):
    """`(dir, filename)` of the pytest config to run under, or `(root, None)`.

    THE SHALLOWEST candidate wins, and ties are broken by `CONFIG_FILES` order
    and then by path. Shallowest rather than "nearest the test files" because a
    config file governs everything below it: the outermost one is the one whose
    `markers` and `addopts` the repository intends for the whole suite, and
    choosing an inner one would silently drop the outer one's settings.

    Deterministic by construction -- `os.walk` order is filesystem-dependent, so
    the key includes the path and nothing relies on iteration order.
    """
    best = None
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        present = set(filenames)
        for rank, (name, section) in enumerate(CONFIG_FILES):
            if name not in present:
                continue
            if not _has_section(os.path.join(dirpath, name), section):
                continue
            rel = os.path.relpath(dirpath, root)
            depth = 0 if rel == "." else rel.count(os.sep) + 1
            key = (depth, rank, dirpath)
            if best is None or key < best[0]:
                best = (key, dirpath, name)
            break
    if best is None:
        return root, None
    return best[1], best[2]


def find_requirements(config_dir, root):
    """The requirements file to install, or `None`.

    The config directory first, then the bundle root: a suite's dependencies are
    declared beside the config that governs it, and the root is the fallback for
    a repository whose `content_root` already *is* that directory.
    """
    for directory in (config_dir, root):
        for name in REQUIREMENTS_FILES:
            candidate = os.path.join(directory, name)
            if os.path.isfile(candidate):
                return candidate
    return None


def main(argv):
    if len(argv) != 2:
        sys.stderr.write("usage: bundle_layout.py <bundle-root>\n")
        return 2
    root = os.path.abspath(argv[1])
    config_dir, config_file = find_config(root)
    requirements = find_requirements(config_dir, root)
    rel = os.path.relpath(config_dir, root)
    # `.` is not a useful prefix and must not become one: it would turn a nodeid
    # of `tests/x.py::t` into `./tests/x.py::t` and stop matching the plan's
    # path. Empty means "the config directory IS the bundle root".
    prefix = "" if rel == "." else rel

    print("QA_CONFIG_DIR=%s" % shlex.quote(config_dir))
    print("QA_CONFIG_FILE=%s" % shlex.quote(config_file or ""))
    print("QA_CONFIG_PREFIX=%s" % shlex.quote(prefix))
    print("QA_REQUIREMENTS=%s" % shlex.quote(requirements or ""))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
