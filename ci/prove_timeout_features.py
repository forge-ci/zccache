"""Seed raw waits in an isolated source snapshot and prove product/OS coverage.

Run with an empty directory created by safe-mktemp as the sole argument.
The caller retains the snapshot and logs as evidence, then uses safe-rm.
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

from ci import lint

PROBE = """
#[allow(dead_code)]
fn timeout_policy_probe_1967() {
    drop(std::process::Command::new("never-executed").status());
}
"""


def groups(root: Path) -> list[list[str]]:
    manifest = tomllib.loads((root / "crates/zccache/Cargo.toml").read_text())
    binaries = [
        "crates/zccache/" + target["path"]
        for target in manifest["bin"]
        if target.get("required-features")
        and "test-support" not in target["required-features"]
    ]
    return [
        ["crates/zccache-cli-core/src/download_client/mod.rs",
         "crates/zccache-cli-core/src/download_daemon/mod.rs"],
        ["crates/zccache-cli/src/lib.rs"],
        binaries,
    ]


def main() -> int:
    snapshot = Path(sys.argv[1]).resolve()
    if not snapshot.is_dir() or any(snapshot.iterdir()):
        raise SystemExit("snapshot must be an existing empty scratch directory")
    root = lint.SCRIPT_DIR
    names = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root
    ).decode().split("\0")
    for name in filter(None, names):
        source = root / name
        if source.is_file():
            destination = snapshot / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
    logs = snapshot / "proof-logs"
    logs.mkdir()
    (logs / "README.md").write_text("# Timeout feature coverage evidence\n")
    command = (
        "import sys; from ci import lint; env=lint.self_build_env(); "
        "env['SOLDR_DYLINT_TOOLCHAIN']=lint.DYLINT_TOOLCHAIN; "
        "env['SOLDR_VERDICTS']='off'; "
        "sys.exit(lint.dylint_product_pass(env, [sys.argv[1]]))"
    )
    env = {**os.environ, "CARGO_TARGET_DIR": str(root / "target" / "timeout-feature-proof")}
    for index, paths in enumerate(groups(snapshot)):
        originals = {path: (snapshot / path).read_text() for path in paths}
        try:
            for path, original in originals.items():
                (snapshot / path).write_text(original + PROBE)
            for os_name, target in lint.DYLINT_OS_TARGETS.items():
                log = logs / f"{index}-{os_name}.log"
                with log.open("w") as stream:
                    result = subprocess.run(
                        ["uv", "run", "--no-project", "python", "-c", command, target],
                        cwd=snapshot, env=env, stdout=stream, stderr=subprocess.STDOUT,
                        timeout=1800,
                    )
                output = log.read_text()
                if result.returncode == 0 or "raw `std::..::Command::status`" not in output:
                    raise SystemExit(f"missing timeout-policy failure: {log}")
                missing = [path for path in paths if path not in output]
                if missing:
                    raise SystemExit(f"unselected product sources {missing}: {log}")
                print(f"PASS {os_name}: {', '.join(paths)}", flush=True)
        finally:
            for path, original in originals.items():
                (snapshot / path).write_text(original)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
