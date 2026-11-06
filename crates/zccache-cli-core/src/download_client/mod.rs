#![allow(clippy::missing_errors_doc)]

pub(crate) mod artifact;

use std::path::Path;

use crate::core::NormalizedPath;
use crate::download::{
    canonical_destination, DownloadAttachResult, DownloadDaemonStatus, DownloadOptions,
    DownloadStatus,
};
use crate::download_protocol::{Request, Response};

pub use artifact::{
    ArchiveFormat, DownloadSource, FetchRequest, FetchResult, FetchState, FetchStateKind,
    FetchStatus, WaitMode,
};

type ClientConn = crate::download_ipc::DownloadIpcConnection;

use crate::download_protocol::daemon_mgmt;
pub use crate::download_protocol::daemon_mgmt::{
    default_endpoint, lock_file_path, read_lock_file_pid, remove_lock_file, write_lock_file,
};

pub fn check_running_daemon() -> Option<u32> {
    check_running_daemon_for(&default_endpoint())
}

/// The live PID of `endpoint`'s download daemon, retiring the record and the
/// endpoint when that PID is dead or provably another process (#2090).
///
/// Only `endpoint`'s own lock and record are read, and only `endpoint` is
/// retired: a custom-endpoint client must not touch the default daemon's socket.
pub fn check_running_daemon_for(endpoint: &str) -> Option<u32> {
    let pid = daemon_mgmt::read_lock_file_pid_for(endpoint)?;
    if lock_pid_is_the_daemon(endpoint, pid) {
        return Some(pid);
    }
    crate::ipc::remove_backend_identity_for(endpoint);
    daemon_mgmt::remove_lock_file_for(endpoint);
    let _ = crate::ipc::retire_endpoint(endpoint);
    None
}

/// A record naming `pid` decides, with the #2035 rules shared with the compile
/// daemon: another PID namespace or a different process disproves it, a replaced
/// binary does not. Without one, only the executable name is left (issue #132).
fn lock_pid_is_the_daemon(endpoint: &str, pid: u32) -> bool {
    if !crate::ipc::is_process_alive(pid) {
        return false;
    }
    if crate::ipc::read_backend_identity_for(endpoint).is_some_and(|record| record.pid() == pid) {
        return !crate::ipc::daemon_record_is_stale(endpoint, pid);
    }
    crate::ipc::verify_pid_exe_stem(pid, "zccache-download-daemon")
}

/// Why no download daemon could be made available.
#[derive(Debug, thiserror::Error)]
enum EnsureDaemonError {
    /// Nothing proves this live PID is the daemon, so it is not safe to stop.
    #[error(
        "download daemon process {pid} is alive but not accepting connections at {endpoint}, \
         and its record does not prove it is safe to stop; stop that process, then retry"
    )]
    UnprovenInstance { pid: u32, endpoint: String },
    /// The record proves the instance, but it would not die.
    #[error("download daemon process {pid} is not accepting connections and could not be stopped: {reason}")]
    StopFailed { pid: u32, reason: String },
    #[error("{0}")]
    Other(String),
}

/// Stop the instance behind an unreachable endpoint, only if its record proves it.
/// The daemon binds before writing its lock, so a lock with a live PID and no
/// endpoint is not a slow start. The successor's bind replaces any stale socket and
/// its records replace these, so nothing is removed here that a concurrent client's
/// successor could already own.
fn stop_unreachable_instance(endpoint: &str, pid: u32) -> Result<(), EnsureDaemonError> {
    if crate::core::config::daemon_spawn_disabled() {
        return Err(EnsureDaemonError::Other(
            crate::core::config::no_spawn_error("zccache-download-daemon"),
        ));
    }
    if crate::ipc::verified_daemon_instance_for(endpoint, pid).is_none() {
        return Err(EnsureDaemonError::UnprovenInstance {
            pid,
            endpoint: endpoint.to_owned(),
        });
    }
    match crate::ipc::force_kill_verified_daemon_for(endpoint, pid) {
        Err(error) if crate::ipc::is_process_alive(pid) => Err(EnsureDaemonError::StopFailed {
            pid,
            reason: error.to_string(),
        }),
        _ => Ok(()),
    }
}

async fn connect_client(endpoint: &str) -> Result<ClientConn, crate::ipc::IpcError> {
    ClientConn::connect(endpoint).await
}

fn resolve_endpoint(explicit: Option<&str>) -> String {
    explicit
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("ZCCACHE_DOWNLOAD_ENDPOINT").ok())
        .unwrap_or_else(default_endpoint)
}

fn run_async<T>(future: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
    crate::core::wait::block_on_for(&runtime, future, crate::core::wait::WHOLE_COMMAND)
        .map_err(|error| format!("command exceeded its runtime budget: {error}"))?
}

fn find_daemon_binary() -> Result<Option<NormalizedPath>, String> {
    let name =
        crate::platform::executable::native_name(std::ffi::OsStr::new("zccache-download-daemon"));

    if let Ok(exe) = crate::platform::executable::current_image() {
        // #1000: the release ships one multicall `zccache` executable. When
        // the download client is running inside that binary, copy it to the
        // version-rooted download-daemon name so argv[0] dispatch enters the
        // server and the PID identity check remains exact. Other embedders
        // and the standalone download CLI do not contain that entry point,
        // so they retain the sibling/PATH compatibility lookup below.
        #[cfg(feature = "cli")]
        if is_multicall_host(exe.as_path()) {
            let dest = crate::core::config::daemon_state_dir().join(&name);
            let deployed = materialize_multicall_download_daemon(exe.as_path(), dest.as_path())
                .map_err(|err| {
                    format!(
                        "failed to self-deploy zccache-download-daemon from {} to {}: {err}",
                        exe.display(),
                        dest.display()
                    )
                })?;
            return Ok(deployed.map(Into::into));
        }

        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&name);
            if candidate.exists() {
                return Ok(Some(candidate.into()));
            }
        }
    }

    Ok(crate::platform::executable::find_on_path(&name).map(Into::into))
}

#[cfg(feature = "cli")]
fn is_multicall_host(exe: &Path) -> bool {
    crate::platform::executable::stem_matches(exe.as_os_str(), "zccache")
        || crate::platform::executable::stem_matches(exe.as_os_str(), "zccache-daemon")
}

#[cfg(feature = "cli")]
fn materialize_multicall_download_daemon(
    exe: &Path,
    dest: &Path,
) -> Result<Option<std::path::PathBuf>, std::io::Error> {
    if !is_multicall_host(exe) {
        return Ok(None);
    }
    crate::cli::materialize_daemon_exe_to(exe, dest).map(Some)
}

#[cfg(all(test, feature = "cli"))]
mod self_deploy_contract_tests {
    use super::*;

    #[test]
    fn only_the_multicall_cli_is_a_self_deploy_source() {
        assert!(is_multicall_host(Path::new("zccache")));
        assert!(is_multicall_host(Path::new("zccache.exe")));
        assert!(is_multicall_host(Path::new("zccache-daemon.exe")));
        assert!(!is_multicall_host(Path::new("zccache-download")));
        assert!(!is_multicall_host(Path::new("host-embedding-zccache")));
    }

    #[test]
    fn multicall_cli_is_materialized_under_the_download_daemon_name() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let source = temp.path().join(if cfg!(windows) {
            "zccache.exe"
        } else {
            "zccache"
        });
        let dest = temp.path().join("version").join(if cfg!(windows) {
            "zccache-download-daemon.exe"
        } else {
            "zccache-download-daemon"
        });
        std::fs::write(&source, b"multicall-binary").expect("write source");

        let deployed = materialize_multicall_download_daemon(&source, &dest)
            .expect("materialize multicall binary")
            .expect("multicall source is recognized");

        assert_eq!(deployed, dest);
        assert_eq!(std::fs::read(deployed).unwrap(), b"multicall-binary");
    }

    #[test]
    fn recognized_multicall_source_fails_closed_when_deployment_fails() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let missing_source = temp.path().join(if cfg!(windows) {
            "zccache.exe"
        } else {
            "zccache"
        });
        let dest = temp.path().join("version").join(if cfg!(windows) {
            "zccache-download-daemon.exe"
        } else {
            "zccache-download-daemon"
        });

        assert!(materialize_multicall_download_daemon(&missing_source, &dest).is_err());
    }

    #[test]
    fn non_multicall_embedder_keeps_compatibility_lookup_path() {
        let result = materialize_multicall_download_daemon(
            Path::new("host-embedding-zccache"),
            Path::new("unused-download-daemon"),
        )
        .expect("non-multicall detection does not touch the filesystem");
        assert!(result.is_none());
    }
}

fn spawn_daemon(bin: &Path, endpoint: &str) -> Result<(), String> {
    // Issue #982: backstop for the host no-spawn guard (see
    // `core::config::NO_SPAWN_ENV`) — the download daemon is covered
    // by the same contract as the compile daemon.
    if crate::core::config::daemon_spawn_disabled() {
        return Err(crate::core::config::no_spawn_error(
            "zccache-download-daemon",
        ));
    }
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["--foreground", "--endpoint", endpoint]);
    // The baseline below drops the caller's environment. Without its `ZCCACHE_*`
    // configuration the daemon records itself under another cache root, where this
    // client can never find it (#2090). Same rule as the compile daemon's
    // `zccache_config_env_from`, which is behind the `cli` feature.
    for (key, value) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("ZCCACHE_")) {
            cmd.env(key, value);
        }
    }
    // Canonical daemon launch owns null standard streams and sanitized native
    // handles. This is ordinary inherited placement, not independent spawning;
    // the existing no-spawn guard and readiness loop remain authoritative.
    let _daemon = kernal_api::platform::process::spawn_sync_daemon(
        &mut cmd,
        kernal_api::platform::process::DaemonStdio {
            stdout: kernal_api::platform::process::DaemonStdioSource::Null,
            stderr: kernal_api::platform::process::DaemonStdioSource::Null,
        },
        kernal_api::platform::process::SyncEnvironment::UserBaseline,
        false,
    )
    .map_err(|e| format!("failed to spawn download daemon: {e}"))?;
    Ok(())
}

async fn ensure_daemon(endpoint: &str) -> Result<(), String> {
    ensure_daemon_with(endpoint, || {
        let bin = find_daemon_binary()?.ok_or("cannot find zccache-download-daemon binary")?;
        spawn_daemon(&bin, endpoint)
    })
    .await
    .map_err(|error| error.to_string())
}

async fn wait_out_unreachable_instance(endpoint: &str, pid: u32) -> Result<(), EnsureDaemonError> {
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if connect_client(endpoint).await.is_ok() {
            return Ok(());
        }
        if !crate::ipc::is_process_alive(pid) {
            break;
        }
    }
    match check_running_daemon_for(endpoint) {
        Some(current) if current == pid => stop_unreachable_instance(endpoint, pid),
        // Another client's replacement holds the lock, and locks follow the bind.
        // Spawning here would unlink its socket.
        Some(current) if connect_client(endpoint).await.is_err() => {
            Err(EnsureDaemonError::UnprovenInstance {
                pid: current,
                endpoint: endpoint.to_owned(),
            })
        }
        _ => Ok(()),
    }
}

async fn ensure_daemon_with(
    endpoint: &str,
    spawn: impl FnOnce() -> Result<(), String>,
) -> Result<(), EnsureDaemonError> {
    if connect_client(endpoint).await.is_ok() {
        return Ok(());
    }
    if let Some(pid) = check_running_daemon_for(endpoint) {
        wait_out_unreachable_instance(endpoint, pid).await?;
        if connect_client(endpoint).await.is_ok() {
            return Ok(());
        }
    }
    // Issue #982: refuse before binary resolution so the guard's message
    // wins over "cannot find zccache-download-daemon binary".
    if crate::core::config::daemon_spawn_disabled() {
        return Err(EnsureDaemonError::Other(
            crate::core::config::no_spawn_error("zccache-download-daemon"),
        ));
    }
    spawn().map_err(EnsureDaemonError::Other)?;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if connect_client(endpoint).await.is_ok() {
            return Ok(());
        }
    }
    Err(EnsureDaemonError::Other(
        "download daemon started but did not accept connections after 10s".to_string(),
    ))
}

#[cfg(all(test, feature = "cli", feature = "download-daemon"))]
#[path = "liveness_tests.rs"]
mod liveness_tests;

#[derive(Clone)]
pub struct DownloadClient {
    endpoint: Option<String>,
}

impl DownloadClient {
    #[must_use]
    pub fn new(endpoint: Option<String>) -> Self {
        Self { endpoint }
    }

    #[must_use]
    pub fn resolved_endpoint(&self) -> String {
        resolve_endpoint(self.endpoint.as_deref())
    }

    pub fn start_daemon(&self) -> Result<(), String> {
        let client = self.clone();
        run_async(async move { client.start_daemon_async().await })
    }

    pub fn stop_daemon(&self) -> Result<bool, String> {
        let client = self.clone();
        run_async(async move { client.stop_daemon_async().await })
    }

    pub fn daemon_status(&self) -> Result<DownloadDaemonStatus, String> {
        let client = self.clone();
        run_async(async move { client.daemon_status_async().await })
    }

    pub async fn start_daemon_async(&self) -> Result<(), String> {
        let endpoint = self.resolved_endpoint();
        ensure_daemon(&endpoint).await
    }

    pub async fn stop_daemon_async(&self) -> Result<bool, String> {
        let endpoint = self.resolved_endpoint();
        let mut conn = match connect_client(&endpoint).await {
            Ok(conn) => conn,
            Err(_) => return Ok(false),
        };
        conn.send(&Request::Shutdown)
            .await
            .map_err(|e| format!("failed to send shutdown to download daemon: {e}"))?;
        match conn.recv::<Response>().await {
            Ok(Some(Response::ShuttingDown)) => Ok(true),
            Ok(Some(Response::Error { message })) => Err(message),
            Ok(Some(other)) => Err(format!("unexpected response: {other:?}")),
            Ok(None) => Err("download daemon closed connection unexpectedly".to_string()),
            Err(e) => Err(format!("broken connection to download daemon: {e}")),
        }
    }

    pub async fn daemon_status_async(&self) -> Result<DownloadDaemonStatus, String> {
        let endpoint = self.resolved_endpoint();
        let mut conn = connect_client(&endpoint)
            .await
            .map_err(|e| format!("download daemon not running at {endpoint}: {e}"))?;
        conn.send(&Request::Status)
            .await
            .map_err(|e| format!("failed to query download daemon: {e}"))?;
        match conn.recv::<Response>().await {
            Ok(Some(Response::Status(status))) => Ok(status),
            Ok(Some(Response::Error { message })) => Err(message),
            Ok(Some(other)) => Err(format!("unexpected response: {other:?}")),
            Ok(None) => Err("download daemon closed connection unexpectedly".to_string()),
            Err(e) => Err(format!("broken connection to download daemon: {e}")),
        }
    }

    pub fn download(
        &self,
        url: &str,
        destination: &Path,
        options: DownloadOptions,
    ) -> Result<DownloadHandle, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to create runtime: {e}"))?;
        let async_handle = crate::core::wait::block_on_for(
            &runtime,
            self.download_async(url, destination, options),
            crate::core::wait::PROBE,
        )
        .map_err(|error| error.to_string())??;
        Ok(DownloadHandle {
            runtime,
            conn: async_handle.conn,
            initiator: async_handle.initiator,
            download_id: async_handle.download_id,
        })
    }

    pub async fn download_async(
        &self,
        url: &str,
        destination: &Path,
        options: DownloadOptions,
    ) -> Result<AsyncDownloadHandle, String> {
        let endpoint = self.resolved_endpoint();
        let url = url.to_string();
        let destination = canonical_destination(destination).map_err(|e| e.to_string())?;
        ensure_daemon(&endpoint).await?;
        let mut conn = connect_client(&endpoint)
            .await
            .map_err(|e| format!("cannot connect to download daemon at {endpoint}: {e}"))?;
        conn.send(&Request::DownloadAttach {
            url: url.clone(),
            destination,
            options,
        })
        .await
        .map_err(|e| format!("failed to send attach request: {e}"))?;
        match conn.recv::<Response>().await {
            Ok(Some(Response::DownloadAttached {
                download_id,
                initiator,
                status: _,
            })) => Ok(AsyncDownloadHandle {
                conn,
                initiator,
                download_id,
            }),
            Ok(Some(Response::Error { message })) => Err(message),
            Ok(Some(other)) => Err(format!("unexpected response: {other:?}")),
            Ok(None) => Err("download daemon closed connection unexpectedly".to_string()),
            Err(e) => Err(format!("broken connection to download daemon: {e}")),
        }
    }
}

pub struct AsyncDownloadHandle {
    conn: ClientConn,
    initiator: bool,
    download_id: String,
}

impl AsyncDownloadHandle {
    #[must_use]
    pub fn initiator(&self) -> bool {
        self.initiator
    }

    #[must_use]
    pub fn download_id(&self) -> &str {
        &self.download_id
    }

    pub async fn status(&mut self) -> Result<DownloadStatus, String> {
        send_download_command(&mut self.conn, Request::DownloadStatus).await
    }

    pub async fn wait(&mut self, timeout_ms: Option<u64>) -> Result<DownloadStatus, String> {
        send_download_command(&mut self.conn, Request::DownloadWait { timeout_ms }).await
    }

    pub async fn cancel(&mut self) -> Result<DownloadStatus, String> {
        send_download_command(&mut self.conn, Request::DownloadCancel).await
    }

    pub fn close(self) -> Result<(), String> {
        Ok(())
    }
}

pub struct DownloadHandle {
    runtime: tokio::runtime::Runtime,
    conn: ClientConn,
    initiator: bool,
    download_id: String,
}

impl DownloadHandle {
    #[must_use]
    pub fn initiator(&self) -> bool {
        self.initiator
    }

    #[must_use]
    pub fn download_id(&self) -> &str {
        &self.download_id
    }

    pub fn status(&mut self) -> Result<DownloadStatus, String> {
        crate::core::wait::block_on_for(
            &self.runtime,
            send_download_command(&mut self.conn, Request::DownloadStatus),
            crate::core::wait::WHOLE_COMMAND,
        )
        .map_err(|error| error.to_string())?
    }

    pub fn wait(&mut self, timeout_ms: Option<u64>) -> Result<DownloadStatus, String> {
        crate::core::wait::block_on_for(
            &self.runtime,
            send_download_command(&mut self.conn, Request::DownloadWait { timeout_ms }),
            crate::core::wait::WHOLE_COMMAND,
        )
        .map_err(|error| error.to_string())?
    }

    pub fn cancel(&mut self) -> Result<DownloadStatus, String> {
        crate::core::wait::block_on_for(
            &self.runtime,
            send_download_command(&mut self.conn, Request::DownloadCancel),
            crate::core::wait::WHOLE_COMMAND,
        )
        .map_err(|error| error.to_string())?
    }

    pub fn close(self) -> Result<(), String> {
        Ok(())
    }
}

async fn send_download_command(
    conn: &mut ClientConn,
    request: Request,
) -> Result<DownloadStatus, String> {
    let action = match &request {
        Request::DownloadStatus => "status",
        Request::DownloadWait { .. } => "wait",
        Request::DownloadCancel => "cancel",
        _ => "download",
    };
    conn.send(&request)
        .await
        .map_err(|e| format!("failed to send {action} request: {e}"))?;
    match conn.recv::<Response>().await {
        Ok(Some(Response::DownloadStatusResult { status })) => Ok(status),
        Ok(Some(Response::DownloadFinished { status })) => Ok(status),
        Ok(Some(Response::DownloadCancelled { status })) => Ok(status),
        Ok(Some(Response::Error { message })) => Err(message),
        Ok(Some(other)) => Err(format!("unexpected response: {other:?}")),
        Ok(None) => Err("download daemon closed connection unexpectedly".to_string()),
        Err(e) => Err(format!("broken connection to download daemon: {e}")),
    }
}

pub fn is_terminal(status: &DownloadStatus) -> bool {
    matches!(
        status.phase,
        crate::download::DownloadPhase::Completed
            | crate::download::DownloadPhase::Cancelled
            | crate::download::DownloadPhase::Failed
    )
}

pub fn coerce_attach_result(
    download_id: String,
    initiator: bool,
    status: DownloadStatus,
) -> DownloadAttachResult {
    DownloadAttachResult {
        download_id,
        initiator,
        status,
    }
}
