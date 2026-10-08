"""fetch_bundle.unpack() must not let a bundle write outside its destination.

The archives below are built in memory from two or three tiny entries. They are
deliberately finite: the point is the symlink, not the size.
"""
import importlib.util
import io
import pathlib
import re
import tarfile

import pytest

RUNNER = pathlib.Path(__file__).resolve().parents[2] / "runner"

_spec = importlib.util.spec_from_file_location(
    "fetch_bundle", RUNNER / "fetch_bundle.py"
)
fetch_bundle = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fetch_bundle)


def _bundle(*entries):
    """entries: (name, kind, payload) with kind in file|symlink|hardlink."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, kind, payload in entries:
            info = tarfile.TarInfo(name)
            if kind in ("symlink", "hardlink"):
                info.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                info.linkname = payload
                tar.addfile(info)
            else:
                data = payload.encode()
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def test_a_benign_bundle_still_unpacks(tmp_path):
    dest = tmp_path / "dest"
    dest.mkdir()
    fetch_bundle.unpack(_bundle(("tests/test_a.py", "file", "x = 1\n")), str(dest))
    assert (dest / "tests" / "test_a.py").read_text() == "x = 1\n"


@pytest.mark.parametrize("target", ["../outside", "/tmp/outside-target"])
def test_a_symlink_escaping_the_directory_is_refused(tmp_path, target):
    dest = tmp_path / "dest"
    dest.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    # `link` points out of `dest` (relative or absolute); `link/pwned` would then
    # be written THROUGH it. Every entry name is itself clean, so the name check
    # in unpack() passes: only the linkname is wrong.
    evil = _bundle(
        ("link", "symlink", "../outside" if target.startswith("..") else str(outside)),
        ("link/pwned", "file", "owned\n"),
    )
    with pytest.raises(SystemExit):
        fetch_bundle.unpack(evil, str(dest))
    assert not (outside / "pwned").exists()


@pytest.mark.parametrize("target", ["../../etc/passwd", "/etc/passwd"])
def test_a_hardlink_escaping_the_directory_is_refused(tmp_path, target):
    dest = tmp_path / "dest"
    dest.mkdir()
    # A hardlink member names an existing file to link to; one pointing out of
    # `dest` (relative or absolute) would expose that file inside the tree. The
    # entry name is clean, so only the linkname is wrong, as for the symlinks.
    evil = _bundle(("link", "hardlink", target))
    with pytest.raises(SystemExit):
        fetch_bundle.unpack(evil, str(dest))
    assert not (dest / "link").exists()


def test_the_runner_base_image_supports_extraction_filters():
    """`filter=` exists from 3.12.0 (and some 3.9-3.11 backports); the image tag
    is what decides whether unpack() runs with it, so pin the tag's floor."""
    first = next(
        line
        for line in (RUNNER / "runner.Dockerfile").read_text().splitlines()
        if line.startswith("FROM ")
    )
    match = re.match(r"FROM python:(\d+)\.(\d+)", first)
    assert match, first
    assert (int(match.group(1)), int(match.group(2))) >= (3, 12), first
