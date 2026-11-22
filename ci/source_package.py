"""Setuptools commands for building the native distribution from an sdist.

The release wheels are assembled from CI artifacts. Source installs instead
build the same CLI binaries and three PyO3 libraries through soldr.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from distutils.command.build_scripts import build_scripts
from pathlib import Path

from setuptools import Distribution, Extension
from setuptools.command.build_ext import build_ext

from ci.soldr import cargo_command, rust_tool_command, self_build_env

ROOT = Path(__file__).resolve().parents[1]
NATIVE_LIBRARIES = {
    "zccache._native": ("zccache-cli", "zccache_cli"),
    "zccache.fingerprint._native": ("zccache-fingerprint-py", "zccache_fingerprint"),
    "zccache.watcher._native": ("zccache-watcher-py", "zccache_watcher"),
}


class SourceDistribution(Distribution):
    def has_scripts(self) -> bool:
        return True


class RustBuildExt(build_ext):
    def run(self) -> None:
        env = self_build_env()
        env["PYO3_PYTHON"] = sys.executable
        target_dir = Path(env.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
        env["CARGO_TARGET_DIR"] = str(target_dir)
        info = subprocess.check_output(
            rust_tool_command("rustc", "-vV"), cwd=ROOT, env=env, text=True
        )
        host = next(
            line.removeprefix("host: ")
            for line in info.splitlines()
            if line.startswith("host: ")
        )
        self.artifact_dir = target_dir / host / "release"
        common = ("build", "--release", "--locked", "--target", host)
        subprocess.run(
            cargo_command(
                *common,
                "-p",
                "zccache",
                "--features",
                "zccache-bin,fingerprint-bin",
                "--bin",
                "zccache",
                "--bin",
                "zccache-fp",
            ),
            cwd=ROOT,
            env=env,
            check=True,
        )
        # Separate invocations preserve each PyO3 library's link configuration.
        for module, (package, library) in NATIVE_LIBRARIES.items():
            features = ("--features", "python") if package == "zccache-cli" else ()
            subprocess.run(
                cargo_command(*common, "-p", package, "--lib", *features),
                cwd=ROOT,
                env=env,
                check=True,
            )
            filename = (
                f"{library}.dll"
                if os.name == "nt"
                else f"lib{library}.{'dylib' if sys.platform == 'darwin' else 'so'}"
            )
            destination = Path(self.get_ext_fullpath(module))
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(self.artifact_dir / filename, destination)


class RustBuildScripts(build_scripts):
    def run(self) -> None:
        self.run_command("build_ext")
        native = self.get_finalized_command("build_ext")
        Path(self.build_dir).mkdir(parents=True, exist_ok=True)
        self.outfiles = []
        for name in ("zccache", "zccache-fp"):
            filename = name + (".exe" if os.name == "nt" else "")
            destination = Path(self.build_dir) / filename
            shutil.copy2(native.artifact_dir / filename, destination)
            destination.chmod(0o755)
            self.outfiles.append(str(destination))

    def get_source_files(self) -> list[str]:
        return []


def setup_options() -> dict:
    return {
        "distclass": SourceDistribution,
        "ext_modules": [Extension(name, sources=[]) for name in NATIVE_LIBRARIES],
        "cmdclass": {"build_ext": RustBuildExt, "build_scripts": RustBuildScripts},
    }
