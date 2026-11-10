from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Union

from zccache._native import NativeDownloadApi

DownloadSource = Union[str, list[str]]


@dataclass(frozen=True)
class DownloadStatus:
    phase: str
    total_bytes: int | None
    downloaded_bytes: int
    percentage: float | None
    active_clients: int
    initiator: bool
    destination: str
    source_url: str
    error: str | None = None


@dataclass(frozen=True)
class DownloadDaemonStatus:
    version: str
    active_downloads: int
    connected_clients: int
    uptime_secs: int
    endpoint: str


@dataclass(frozen=True)
class FetchResult:
    status: str
    cache_path: str
    expanded_path: str | None
    bytes: int | None
    sha256: str


@dataclass(frozen=True)
class FetchState:
    kind: str
    cache_path: str
    expanded_path: str | None
    bytes: int | None
    sha256: str | None
    reason: str | None


def _coerce_status(status: object, initiator: bool) -> DownloadStatus:
    return DownloadStatus(
        phase=status.phase,
        total_bytes=status.total_bytes,
        downloaded_bytes=status.downloaded_bytes,
        percentage=status.percentage,
        active_clients=status.active_clients,
        initiator=initiator,
        destination=status.destination,
        source_url=status.source_url,
        error=status.error,
    )


def _coerce_daemon_status(status: object) -> DownloadDaemonStatus:
    return DownloadDaemonStatus(
        version=status.version,
        active_downloads=status.active_downloads,
        connected_clients=status.connected_clients,
        uptime_secs=status.uptime_secs,
        endpoint=status.endpoint,
    )


def _coerce_fetch_result(result: object) -> FetchResult:
    return FetchResult(
        status=result.status,
        cache_path=result.cache_path,
        expanded_path=result.expanded_path,
        bytes=result.bytes,
        sha256=result.sha256,
    )


def _coerce_fetch_state(state: object) -> FetchState:
    return FetchState(
        kind=state.kind,
        cache_path=state.cache_path,
        expanded_path=state.expanded_path,
        bytes=state.bytes,
        sha256=state.sha256,
        reason=state.reason,
    )


class DownloadHandle:
    def __init__(self, native: object) -> None:
        self._native = native

    @property
    def initiator(self) -> bool:
        return bool(self._native.initiator)

    @property
    def download_id(self) -> str:
        return str(self._native.download_id)

    def status(self) -> DownloadStatus:
        return _coerce_status(self._native.status(), self.initiator)

    def wait(self, timeout_ms: int) -> DownloadStatus:
        return _coerce_status(self._native.wait(timeout_ms), self.initiator)

    def cancel(self) -> DownloadStatus:
        return _coerce_status(self._native.cancel(), self.initiator)

    def close(self) -> None:
        self._native.close()

    def __enter__(self) -> "DownloadHandle":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.close()


class DownloadApi:
    def __init__(self, endpoint: str | None = None) -> None:
        self._native = NativeDownloadApi(endpoint)

    def start(self) -> None:
        self._native.start()

    def stop(self) -> bool:
        return bool(self._native.stop())

    def daemon_status(self) -> DownloadDaemonStatus:
        return _coerce_daemon_status(self._native.daemon_status())

    def attach(
        self,
        *,
        source_url: str,
        destination: str | Path,
        force: bool = False,
        max_connections: int | None = None,
        min_segment_size: int | None = None,
    ) -> DownloadHandle:
        return DownloadHandle(
            self._native.download(
                source_url,
                str(Path(destination)),
                force,
                max_connections,
                min_segment_size,
            )
        )

    def download(
        self,
        *,
        source: DownloadSource,
        destination: str | Path | None = None,
        expanded: str | Path | None = None,
        expected_sha256: str | None = None,
        archive_format: str = "auto",
        max_connections: int | None = None,
        min_segment_size: int | None = None,
        blocking: bool = True,
        dry_run: bool = False,
        force: bool = False,
    ) -> FetchResult:
        destination_path = (
            None if destination is None else str(Path(destination))
        )
        return _coerce_fetch_result(
            self._native.fetch(
                source,
                destination_path,
                None if expanded is None else str(Path(expanded)),
                expected_sha256,
                archive_format,
                max_connections,
                min_segment_size,
                blocking,
                dry_run,
                force,
            )
        )

    def fetch(
        self,
        *,
        source: DownloadSource,
        destination: str | Path | None = None,
        expanded: str | Path | None = None,
        expected_sha256: str | None = None,
        archive_format: str = "auto",
        max_connections: int | None = None,
        min_segment_size: int | None = None,
        blocking: bool = True,
        dry_run: bool = False,
        force: bool = False,
    ) -> FetchResult:
        return self.download(
            source=source,
            destination=destination,
            expanded=expanded,
            expected_sha256=expected_sha256,
            archive_format=archive_format,
            max_connections=max_connections,
            min_segment_size=min_segment_size,
            blocking=blocking,
            dry_run=dry_run,
            force=force,
        )

    def exists(
        self,
        *,
        source: DownloadSource,
        destination: str | Path | None = None,
        expanded: str | Path | None = None,
        expected_sha256: str | None = None,
        archive_format: str = "auto",
    ) -> FetchState:
        return _coerce_fetch_state(
            self._native.exists(
                source,
                None if destination is None else str(Path(destination)),
                None if expanded is None else str(Path(expanded)),
                expected_sha256,
                archive_format,
            )
        )
