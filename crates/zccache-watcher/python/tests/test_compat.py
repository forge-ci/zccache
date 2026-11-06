import tempfile
import time
import unittest
from pathlib import Path

from zccache.watcher import (
    DebouncedFileWatcherProcess,
    FileChangeEvent,
    FileWatcher,
    FileWatcherProcess,
    watch_files,
)


def wait_for(predicate, timeout: float = 5.0, interval: float = 0.05) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


class WatcherTests(unittest.TestCase):
    def _wait_for_changes(
        self,
        watcher: FileWatcherProcess | DebouncedFileWatcherProcess,
        timeout: float = 2.0,
    ) -> list[str]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            changes = watcher.get_all_changes(timeout=0.1)
            if changes:
                return changes
        return []

    def _collect_until_seen(
        self,
        watcher: DebouncedFileWatcherProcess,
        expected: set[str],
        timeout: float = 15.0,
    ) -> list[list[str]]:
        batches: list[list[str]] = []
        seen: set[str] = set()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and not expected <= seen:
            batch = watcher.get_all_changes(timeout=0.1)
            if batch:
                batches.append(batch)
                seen.update(batch)
        return batches

    def test_file_watcher_process_detects_modification(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            watched = Path(temp_dir) / "src"
            watched.mkdir()
            test_file = watched / "main.cpp"
            test_file.write_text("int x = 1;\n")

            watcher = FileWatcherProcess(watched, excluded_patterns=[], poll_interval=0.05)
            try:
                test_file.write_text("int x = 22;\n")
                observed = self._wait_for_changes(watcher)
                self.assertIn(str(test_file.resolve()), observed)
            finally:
                watcher.stop()

    def test_file_watcher_process_ignores_excluded_directories(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            watched = Path(temp_dir) / "project"
            watched.mkdir()
            ignored_dir = watched / "fastled_js"
            ignored_dir.mkdir()
            ignored_file = ignored_dir / "bundle.js"
            ignored_file.write_text("console.log('a')\n")

            watcher = FileWatcherProcess(
                watched,
                excluded_patterns=["fastled_js"],
                poll_interval=0.05,
            )
            try:
                ignored_file.write_text("console.log('b')\n")
                self.assertEqual(watcher.get_all_changes(timeout=0.5), [])
            finally:
                watcher.stop()

    def test_include_folders_limit_scan_scope(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            src = root / "src"
            assets = root / "assets"
            src.mkdir()
            assets.mkdir()
            wanted = src / "main.cpp"
            ignored = assets / "logo.txt"
            wanted.write_text("a\n")
            ignored.write_text("x\n")

            watcher = watch_files(
                root,
                include_folders=["src"],
                poll_interval=0.05,
                debounce_seconds=0.05,
            )
            try:
                ignored.write_text("y\n")
                self.assertIsNone(watcher.poll(0.3))
                wanted.write_text("b\n")
                event = None
                deadline = time.time() + 2.0
                while time.time() < deadline and event is None:
                    event = watcher.poll(0.1)
                assert event is not None
                self.assertEqual(event.paths, [str(wanted.resolve())])
            finally:
                watcher.stop()

    def test_include_globs_and_excluded_patterns_apply_together(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "src").mkdir()
            (root / "src" / "generated").mkdir()
            keep = root / "src" / "app.cpp"
            skip_ext = root / "src" / "notes.txt"
            skip_excluded = root / "src" / "generated" / "gen.cpp"
            keep.write_text("a\n")
            skip_ext.write_text("b\n")
            skip_excluded.write_text("c\n")

            watcher = FileWatcher(
                root,
                include_globs=["src/**/*.cpp"],
                excluded_patterns=["src/generated/**"],
                debounce_seconds=0.05,
                poll_interval=0.05,
            )
            try:
                skip_ext.write_text("b2\n")
                self.assertIsNone(watcher.poll(0.3))
                skip_excluded.write_text("c2\n")
                self.assertIsNone(watcher.poll(0.3))
                keep.write_text("a2\n")
                event = None
                deadline = time.time() + 2.0
                while time.time() < deadline and event is None:
                    event = watcher.poll(0.1)
                self.assertIsNotNone(event)
                assert event is not None
                self.assertEqual(event.paths, [str(keep.resolve())])
            finally:
                watcher.stop()

    def test_callback_api_receives_batches(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            file_path = root / "watch.cpp"
            file_path.write_text("a\n")
            events: list[FileChangeEvent] = []
            watcher = watch_files(
                root,
                include_globs=["**/*.cpp"],
                debounce_seconds=0.05,
                poll_interval=0.05,
                callback=events.append,
            )
            try:
                file_path.write_text("bb\n")
                self.assertTrue(wait_for(lambda: bool(events)))
                self.assertIn(str(file_path.resolve()), events[0].paths)
            finally:
                watcher.stop()

    def test_notification_predicate_can_filter_delivery_late(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            keep = root / "keep.cpp"
            drop = root / "drop.cpp"
            keep.write_text("a\n")
            drop.write_text("a\n")

            calls: list[tuple[str, str]] = []

            def predicate(path: Path, *, relative_path: str, change: str, **kwargs: object) -> bool:
                calls.append((relative_path, change))
                return path.name != "drop.cpp"

            watcher = FileWatcher(
                root,
                include_globs=["**/*.cpp"],
                debounce_seconds=0.05,
                poll_interval=0.05,
                notification_predicate=predicate,
            )
            try:
                drop.write_text("b\n")
                event = watcher.poll(0.4)
                self.assertIsNone(event)

                keep.write_text("b\n")
                event = watcher.poll(1.0)
                self.assertIsNotNone(event)
                assert event is not None
                self.assertEqual(event.paths, [str(keep.resolve())])
                self.assertIn(("drop.cpp", "changed"), calls)
                self.assertIn(("keep.cpp", "changed"), calls)
            finally:
                watcher.stop()

    def test_context_manager_and_resume_reset_state_cleanly(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            file_path = root / "watch.cpp"
            file_path.write_text("a\n")

            watcher = FileWatcher(
                root,
                include_globs=["**/*.cpp"],
                debounce_seconds=0.05,
                poll_interval=0.05,
                autostart=False,
            )
            self.assertFalse(watcher.is_running)

            with watcher:
                self.assertTrue(watcher.is_running)
                file_path.write_text("bb\n")
                event = watcher.poll(1.0)
                self.assertIsNotNone(event)
                assert event is not None
                self.assertEqual(event.paths, [str(file_path.resolve())])

            self.assertFalse(watcher.is_running)

            file_path.write_text("c\n")
            self.assertIsNone(watcher.poll(0.2))

            watcher.resume()
            self.assertTrue(watcher.is_running)
            self.assertIsNone(watcher.poll(0.2))

            file_path.write_text("d\n")
            event = watcher.poll(1.0)
            self.assertIsNotNone(event)
            assert event is not None
            self.assertEqual(event.paths, [str(file_path.resolve())])
            watcher.stop()

    def test_debounced_watcher_batches_and_deduplicates_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            watched = Path(temp_dir) / "src"
            watched.mkdir()
            first = watched / "a.cpp"
            second = watched / "b.cpp"
            first.write_text("a")
            second.write_text("b")
            expected = {str(first.resolve()), str(second.resolve())}

            debounced = DebouncedFileWatcherProcess(
                FileWatcherProcess(
                    watched,
                    excluded_patterns=[],
                    poll_interval=0.05,
                    debounce_seconds=1.0,
                ),
                debounce_seconds=1.0,
            )
            try:
                # A stalled writer legitimately splits a burst: the window is
                # wall-clock, so only a batch holding both paths proves batching.
                # A window that never holds a burst splits every attempt.
                attempts: list[list[list[str]]] = []
                for _ in range(3):
                    # Growing the files keeps each write visible to the
                    # (mtime, size) diff even within one timestamp tick.
                    with first.open("a") as handle:
                        handle.write("a")
                    # Long enough to span scans, so `first` is seen changed
                    # twice inside one window and has to be de-duplicated.
                    time.sleep(0.15)
                    with first.open("a") as handle:
                        handle.write("a")
                    with second.open("a") as handle:
                        handle.write("b")
                    batches = self._collect_until_seen(debounced, expected)
                    for batch in batches:
                        self.assertEqual(batch, sorted(set(batch)))
                    self.assertEqual(
                        expected - {path for batch in batches for path in batch}, set()
                    )
                    attempts.append(batches)
                    if any(expected <= set(batch) for batch in batches):
                        return
                self.fail(f"burst was never delivered as one batch: {attempts}")
            finally:
                debounced.stop()


if __name__ == "__main__":
    unittest.main()
