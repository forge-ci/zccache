"""Linux CI observer for a foreground daemon; never restarts the observed process.

Run outside the build's memory cgroup when possible. This is CI instrumentation,
not a daemon launcher for production. See docs/daemon-death.md.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import TypedDict


class Process(TypedDict):
    pid: int
    ppid: int
    start_ticks: int
    rss_bytes: int | None
    peak_rss_bytes: int | None


def read_text(path: Path) -> str | None:
    try:
        return path.read_text()
    except (OSError, UnicodeError):
        return None


def process(pid: int) -> Process | None:
    """Use start ticks to distinguish reuse; missing measurements stay unknown."""
    stat = read_text(Path(f"/proc/{pid}/stat"))
    if stat is None:
        return None
    fields = stat[stat.rfind(")") + 2 :].split()
    if len(fields) < 22:
        return None
    status = read_text(Path(f"/proc/{pid}/status"))
    memory: dict[str, int] = {}
    for line in (status or "").splitlines():
        name, _, value = line.partition(":")
        if name in {"VmRSS", "VmHWM"}:
            memory[name] = int(value.split()[0]) * 1024
    return {
        "pid": pid,
        "ppid": int(fields[1]),
        "start_ticks": int(fields[19]),
        "rss_bytes": memory.get("VmRSS"),
        "peak_rss_bytes": memory.get("VmHWM"),
    }


def tree(root: Process) -> dict[str, object]:
    processes: dict[int, Process] = {}
    for path in Path("/proc").iterdir():
        if path.name.isdecimal():
            entry = process(int(path.name))
            if entry:
                processes[entry["pid"]] = entry
    current = processes.get(root["pid"])
    if current is None or current["start_ticks"] != root["start_ticks"]:
        return {"root_present": False, "processes": []}
    selected = {root["pid"]}
    while True:
        children = {p["pid"] for p in processes.values() if p["ppid"] in selected}
        expanded = selected | children
        if expanded == selected:
            break
        selected = expanded
    entries = [processes[pid] for pid in sorted(selected)]
    return {
        "root_present": True,
        "process_count": len(entries),
        "truncated": len(entries) > 256,
        "sampled_tree_rss_bytes": (
            sum(p["rss_bytes"] or 0 for p in entries)
            if all(p["rss_bytes"] is not None for p in entries)
            else None
        ),
        "processes": entries[:256],
    }


def cgroups(pid: int) -> list[Path]:
    """Resolve the visible cgroup-v2 membership and every visible ancestor."""
    membership = read_text(Path(f"/proc/{pid}/cgroup")) or ""
    for line in membership.splitlines():
        if line.startswith("0::"):
            root = Path("/sys/fs/cgroup")
            path = root.joinpath(line[3:].lstrip("/")).resolve()
            if not path.is_relative_to(root):
                return []
            return [path, *[p for p in path.parents if p.is_relative_to(root)]]
    return []


def cgroup_sample(paths: list[Path]) -> list[dict[str, object]]:
    return [
        {
            "path": str(path),
            **{
                name: read_text(path / name)
                for name in (
                    "memory.current",
                    "memory.peak",
                    "memory.max",
                    "memory.events",
                    "memory.events.local",
                    "memory.swap.current",
                    "memory.swap.max",
                    "memory.oom.group",
                    "memory.pressure",
                )
            },
        }
        for path in paths
    ]


def positive_seconds(value: str) -> float:
    seconds = float(value)
    if not 0.05 <= seconds <= 21600:
        raise argparse.ArgumentTypeError("seconds must be between 0.05 and 21600")
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--duration-secs", required=True, type=positive_seconds)
    parser.add_argument("--interval-secs", default=1.0, type=positive_seconds)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if sys.platform != "linux" or not command:
        parser.error("Linux and a foreground command after -- are required")
    args.output.mkdir(parents=True, exist_ok=False)
    with (args.output / "samples.jsonl").open("x") as samples:

        def record(event: str, **data: object) -> None:
            samples.write(
                json.dumps(
                    {
                        "event": event,
                        "time_ns": time.time_ns(),
                        "monotonic_ns": time.monotonic_ns(),
                        **data,
                    }
                )
                + "\n"
            )
            samples.flush()
            os.fsync(samples.fileno())

        with (args.output / "daemon.log").open("xb") as log:
            child = subprocess.Popen(command, stdout=log, stderr=log)
            root = process(child.pid)
            paths = cgroups(child.pid)
            record(
                "started", pid=child.pid, identity=root, cgroups=cgroup_sample(paths)
            )
            reason = None
            deadline = time.monotonic() + args.duration_secs
            try:
                while child.poll() is None:
                    record(
                        "sample",
                        tree=tree(root) if root else None,
                        cgroups=cgroup_sample(paths),
                        meminfo=read_text(Path("/proc/meminfo")),
                    )
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        reason = "observer-deadline"
                        break
                    try:
                        child.wait(timeout=min(args.interval_secs, remaining))
                    except subprocess.TimeoutExpired:
                        pass
            except KeyboardInterrupt:
                reason = "observer-interrupt"
            finally:
                if child.poll() is None:
                    record(
                        "observer-stop",
                        reason=reason or "observer-error",
                        signal=signal.SIGTERM,
                    )
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        record(
                            "observer-stop",
                            reason="cleanup-deadline",
                            signal=signal.SIGKILL,
                        )
                        child.kill()
                        child.wait(timeout=10)
                record(
                    "exited",
                    returncode=child.returncode,
                    termination_signal=-child.returncode
                    if child.returncode < 0
                    else None,
                    observer_stop_reason=reason,
                    cgroups=cgroup_sample(paths),
                )
            if reason:
                return 124 if reason == "observer-deadline" else 130
            return 128 - child.returncode if child.returncode < 0 else child.returncode


if __name__ == "__main__":
    raise SystemExit(main())
