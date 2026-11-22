"""Build, budget and install-test the source release independently of wheels."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

import tomllib

MAX_SDIST_BYTES = 20 * 1024 * 1024
ROOT = Path(__file__).resolve().parents[1]


def check_sdist(archive: Path) -> int:
    size = archive.stat().st_size
    print(
        f"sdist: {archive.name}: {size} bytes ({size / 1024 / 1024:.2f} MiB); budget 20 MiB",
        flush=True,
    )
    if size > MAX_SDIST_BYTES:
        raise SystemExit("sdist exceeds the 20 MiB budget")
    with tarfile.open(archive) as stream:
        members = stream.getmembers()
        names = {Path(*Path(member.name).parts[1:]) for member in members}
        # Explicit benchmark targets are validated even in CLI-only builds.
        # Keep their small Rust sources, without the benchmark fixtures.
        for member in members:
            if not member.isfile() or Path(member.name).name != "Cargo.toml":
                continue
            manifest = Path(*Path(member.name).parts[1:])
            with stream.extractfile(member) as source:
                data = tomllib.loads(source.read().decode())
            for bench in data.get("bench", []):
                path = manifest.parent / bench.get(
                    "path", f"benches/{bench['name']}.rs"
                )
                if path not in names and path.with_suffix("") / "main.rs" not in names:
                    raise SystemExit(f"sdist missing manifest benchmark target: {path}")
    excluded = {
        "target",
        "fixtures",
        "perf",
        "bench",
        "docs",
        "_vender",
        ".git",
        "__pycache__",
    }
    for name in names:
        if excluded.intersection(name.parts) or name.suffix in {
            ".pdb",
            ".so",
            ".pyd",
            ".dll",
            ".png",
            ".jpg",
        }:
            raise SystemExit(f"sdist includes excluded artifact: {name}")
        if (
            len(name.parts) > 2
            and name.parts[0] == "crates"
            and name.parts[2] in {"tests", "python"}
        ):
            raise SystemExit(f"sdist includes excluded test fixture: {name}")
    for required in (
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "setup.py",
        "ci/source_package.py",
        "crates/zccache/src/lib.rs",
        "crates/zccache-protocol/proto/zccache_v1.proto",
    ):
        if Path(required) not in names:
            raise SystemExit(f"sdist missing source build input: {required}")
    return size


def verify_sdist(archive: Path) -> None:
    env = dict(os.environ)
    env.pop("PYTHONPATH", None)
    with tempfile.TemporaryDirectory(prefix="zccache-sdist-") as scratch:
        root = Path(scratch)
        venv = root / "venv"
        subprocess.run(
            ["uv", "venv", "--python", sys.executable, str(venv)],
            cwd=root,
            env=env,
            check=True,
        )
        bin_dir = venv / ("Scripts" if os.name == "nt" else "bin")
        python = bin_dir / ("python.exe" if os.name == "nt" else "python")
        subprocess.run(
            [
                "uv",
                "--no-cache",
                "pip",
                "install",
                "--python",
                str(python),
                "--no-binary",
                "zccache",
                str(archive),
            ],
            cwd=root,
            env=env,
            check=True,
        )
        subprocess.run(
            [
                str(python),
                "-I",
                "-c",
                "import zccache; import zccache._native; import zccache.fingerprint._native; import zccache.watcher._native",
            ],
            cwd=root,
            env=env,
            check=True,
        )
        subprocess.run(
            [
                str(bin_dir / ("zccache.exe" if os.name == "nt" else "zccache")),
                "--version",
            ],
            cwd=root,
            env=env,
            check=True,
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    output = ROOT / "dist" / "sdist"
    subprocess.run(
        [
            "uv",
            "build",
            "--python",
            sys.executable,
            "--sdist",
            "--out-dir",
            str(output),
        ],
        cwd=ROOT,
        check=True,
    )
    from ci.release_checks import read_workspace_version

    archive = output / f"zccache-{read_workspace_version()}.tar.gz"
    check_sdist(archive)
    if args.verify:
        verify_sdist(archive)


if __name__ == "__main__":
    main()
