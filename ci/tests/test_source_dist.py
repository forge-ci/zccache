"""Source release contracts, including recovery of wheel-only releases."""

import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest

from ci import release_workflow
from ci.source_dist import MAX_SDIST_BYTES, check_sdist


def test_editable_tooling_install_needs_no_rust(tmp_path: Path) -> None:
    """Repo CI tooling must work before setup-soldr provisions the toolchain."""
    code = (
        "import os,sys; from setuptools.build_meta import build_editable; "
        "os.environ['PATH']=''; build_editable(sys.argv[1])"
    )
    subprocess.run(
        [
            shutil.which("uv"),
            "run",
            "--no-project",
            "--python",
            sys.executable,
            "--with",
            "setuptools",
            "--with",
            "wheel",
            "python",
            "-c",
            code,
            str(tmp_path),
        ],
        cwd=Path(__file__).parents[2],
        check=True,
        timeout=60,
    )
    assert list(tmp_path.glob("*-0.editable-py3-none-any.whl"))


def test_wheels_alone_do_not_complete_pypi(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setattr(
        release_workflow,
        "read_project_meta",
        lambda: ("zccache", "1.15.17", "", "", ""),
    )
    monkeypatch.setattr(
        release_workflow, "check_crates_versions", lambda _: {"zccache"}
    )
    monkeypatch.setattr(
        release_workflow,
        "check_pypi_version",
        lambda *_: release_workflow.expected_pypi_wheel_filenames("zccache", "1.15.17"),
    )
    output = tmp_path / "output"
    monkeypatch.setenv("GITHUB_OUTPUT", str(output))
    release_workflow.command_check_registries(None)
    assert "pypi_complete=false" in output.read_text()


def test_sdist_budget_rejects_oversized_archive(tmp_path: Path) -> None:
    archive = tmp_path / "zccache-1.15.17.tar.gz"
    with archive.open("wb") as stream:
        stream.truncate(MAX_SDIST_BYTES + 1)
    with pytest.raises(SystemExit, match="budget"):
        check_sdist(archive)


def test_sdist_rejects_build_artifacts(tmp_path: Path) -> None:
    archive = tmp_path / "zccache-1.15.17.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        stream.addfile(tarfile.TarInfo("zccache-1.15.17/target/debug/unwanted"))
    with pytest.raises(SystemExit, match="excluded"):
        check_sdist(archive)


def test_release_requires_source_build_verification() -> None:
    workflow = (
        Path(__file__).parents[2] / ".github/workflows/auto-release.yml"
    ).read_text()
    assert "python -m ci.source_dist --verify" in workflow
    assert "needs.build-sdist.result == 'success'" in workflow
    assert "name: pypi-sdist" in workflow


def test_publish_verification_includes_source_archive(
    monkeypatch, tmp_path: Path
) -> None:
    from argparse import Namespace

    wheel = "zccache-1.15.17-py3-none-any.whl"
    source = "zccache-1.15.17.tar.gz"
    for filename in (wheel, source):
        (tmp_path / filename).touch()
    expected = []
    monkeypatch.setattr(
        release_workflow,
        "verify_pypi_publish",
        lambda _project, _version, files, **_kwargs: expected.extend(files),
    )
    release_workflow.command_verify_pypi(
        Namespace(
            wheels_dir=str(tmp_path),
            project="zccache",
            version="1.15.17",
            timeout_s=1,
            poll_interval_s=0,
        )
    )
    assert set(expected) == {wheel, source}
