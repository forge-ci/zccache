//! Endpoint-scoped daemon ownership records: the lock file and the backend
//! identity sidecar (#1904).
//!
//! Both records describe *the daemon serving one endpoint*. Before #1904 they
//! were derived from `default_endpoint()` / `default_cache_dir()` and took no
//! endpoint, so two daemons on two endpoints fought over one record:
//!
//! - the second daemon's start overwrote the first's PID and identity, and
//!   `zccache stop` on the default endpoint force-killed whichever daemon had
//!   written last (Failure A);
//! - `check_running_daemon` for a custom endpoint unlinked the *default*
//!   daemon's socket, wedging a live daemon out of reach of every client
//!   (Failure B).
//!
//! Every function here is named for the endpoint whose record it touches, and
//! the endpoint-less form is a thin wrapper over [`default_endpoint`] so an
//! already-installed build keeps reading and writing the historical filenames
//! byte-for-byte.

use super::{
    default_endpoint, endpoint_scope_tag, normalized_override_root, running_process_endpoint,
    ForceKillError,
};
use zccache_core::NormalizedPath;

/// Path where the daemon records the identity sidecar consumed by identity
/// probes ([`kernal_api::daemon_identity::DaemonIdentity::read_sidecar`]).
#[must_use]
pub fn backend_identity_path() -> NormalizedPath {
    backend_identity_path_for(&default_endpoint())
}

/// Endpoint-scoped counterpart of [`backend_identity_path`] (#1904). The
/// sidecar carries no version tag today; only the endpoint scope segment is
/// added, and only for a non-default endpoint.
#[must_use]
pub fn backend_identity_path_for(endpoint: &str) -> NormalizedPath {
    let namespace = zccache_core::config::daemon_namespace();
    let name = backend_identity_file_name_for(namespace.as_deref(), endpoint);
    if let Some(cache_dir) = normalized_override_root() {
        return cache_dir.join(&name);
    }
    zccache_core::config::default_cache_dir().join(&name)
}

fn backend_identity_file_name_for(namespace: Option<&str>, endpoint: &str) -> String {
    let scope = endpoint_scope_tag(endpoint);
    match namespace {
        Some(ns) => format!("daemon-{ns}{scope}.running-process.json"),
        None => format!("daemon{scope}.running-process.json"),
    }
}

/// Persist the daemon identity used by future identity probes.
///
/// The facade sidecar is byte-identical to the historical
/// `serde_json::to_vec_pretty` form and written atomically, so older and newer
/// zccache binaries keep reading each other's identity.
pub fn write_backend_identity(
    daemon: &kernal_api::daemon_identity::DaemonIdentity,
) -> Result<(), std::io::Error> {
    write_backend_identity_for(&default_endpoint(), daemon)
}

/// Endpoint-scoped counterpart of [`write_backend_identity`] (#1904).
///
/// Without the endpoint in the name, a daemon started with
/// `ZCCACHE_ENDPOINT=/tmp/custom.sock` overwrites the default daemon's
/// identity — after which every kill decision taken against the *default*
/// endpoint verifies against the *custom* daemon's record.
pub fn write_backend_identity_for(
    endpoint: &str,
    daemon: &kernal_api::daemon_identity::DaemonIdentity,
) -> Result<(), std::io::Error> {
    let path = backend_identity_path_for(endpoint);
    if let Some(parent) = path.parent() {
        // #1171: same directory family as the socket endpoint.
        zccache_core::config::create_dir_all_private(parent)?;
    }
    // #2035: the namespace goes first. A reader catching the gap pairs the old
    // identity with the new namespace, which only skips a staleness shortcut;
    // the other order could retire the daemon that is starting right now.
    let namespace_path = pid_namespace_path_for(endpoint);
    match crate::platform::host::pid_namespace() {
        Some(namespace) => std::fs::write(namespace_path.as_path(), namespace)?,
        None => {
            let _ = std::fs::remove_file(namespace_path.as_path());
        }
    }
    daemon.write_sidecar(path.as_path())
}

/// The PID namespace of the daemon recorded for `endpoint` (#2035). A sibling
/// file, because the identity sidecar is a frozen format shared across
/// zccache versions and owned by the facade.
pub(crate) fn pid_namespace_path_for(endpoint: &str) -> NormalizedPath {
    let identity = backend_identity_path_for(endpoint);
    NormalizedPath::from(format!("{}.pid-namespace", identity.display()))
}

/// Remove `endpoint`'s identity record and its PID-namespace sibling.
pub fn remove_backend_identity_for(endpoint: &str) {
    let _ = std::fs::remove_file(backend_identity_path_for(endpoint).as_path());
    let _ = std::fs::remove_file(pid_namespace_path_for(endpoint).as_path());
}

/// Was `endpoint`'s record written in a PID namespace other than this one?
/// A PID from another namespace names nothing here, whatever now holds it.
/// Unknown on either side (no namespaces, or no sibling file) is not a "yes".
fn recorded_in_another_pid_namespace(endpoint: &str) -> bool {
    let Some(current) = crate::platform::host::pid_namespace() else {
        return false;
    };
    std::fs::read_to_string(pid_namespace_path_for(endpoint).as_path())
        .is_ok_and(|recorded| !recorded.trim().is_empty() && recorded.trim() != current)
}

/// Does this failure prove the recorded daemon is not the process holding the
/// PID (#2035)? Deliberately excludes a digest change and a deleted executable:
/// a live daemon whose binary was replaced in place still serves its clients.
/// An unreadable fact proves nothing either.
pub(crate) fn verify_error_proves_foreign(
    error: &kernal_api::daemon_identity::DaemonVerifyError,
) -> bool {
    use kernal_api::daemon_identity::DaemonVerifyError as E;
    match error {
        E::StaleProcess { .. } => true,
        E::BootIdMismatch { actual, .. } => actual != "unavailable",
        E::ExecutablePathMismatch {
            expected, actual, ..
        } => {
            let deleted = format!("{} (deleted)", expected.display());
            actual.as_os_str() != std::ffi::OsStr::new(&deleted)
        }
        _ => false,
    }
}

/// Does `endpoint`'s own record prove that live `pid` is not its daemon (#2035)?
///
/// A missing record, or one naming another PID, is no verdict: legacy/direct
/// daemons run without one, and a starting daemon writes its lock first.
pub(crate) fn daemon_record_is_stale(endpoint: &str, pid: u32) -> bool {
    let Some(identity) = read_backend_identity_for(endpoint) else {
        return false;
    };
    if identity.pid() != pid {
        return false;
    }
    recorded_in_another_pid_namespace(endpoint)
        || identity
            .verify_live()
            .is_err_and(|error| verify_error_proves_foreign(&error))
}

/// Read the persisted daemon identity, if one is recorded and parseable.
///
/// #1161: the identity has been *written* on every daemon start for a long
/// time, and nothing read it back except `probe_backend_handle`'s inline load.
/// Kill decisions verified only "PID is alive and its exe stem is
/// `zccache-daemon`" — which a recycled PID belonging to a *different*
/// zccache-daemon satisfies, so auto-recovery could kill an unrelated live
/// instance.
///
/// The identity already carries what distinguishes instances:
/// `started_at_unix_ms` (PID reuse within a boot) and `boot_id` (across
/// boots). Exposing the read is what lets a kill be bound to the instance the
/// caller actually failed to talk to.
///
/// `None` means "nothing recorded, or unreadable" — deliberately *not*
/// "matches anything". See [`daemon_identity_matches`].
#[must_use]
pub fn read_backend_identity() -> Option<kernal_api::daemon_identity::DaemonIdentity> {
    read_backend_identity_for(&default_endpoint())
}

/// Endpoint-scoped counterpart of [`read_backend_identity`] (#1904).
#[must_use]
pub fn read_backend_identity_for(
    endpoint: &str,
) -> Option<kernal_api::daemon_identity::DaemonIdentity> {
    kernal_api::daemon_identity::DaemonIdentity::read_sidecar(
        backend_identity_path_for(endpoint).as_path(),
    )
}

/// Is the daemon recorded on disk right now the same *instance* as `expected`?
///
/// Compares PID **and** start time **and** boot id. PID alone is not identity:
/// the OS reuses PIDs, aggressively so on Windows, and the exe-stem check that
/// guarded this before is satisfied by any `zccache-daemon` — including one
/// serving a different namespace.
///
/// Returns `false` when nothing is recorded. That is the safe direction for a
/// kill gate: refusing costs one clear error about a daemon that is already
/// not answering, while permitting costs killing a live daemon that was never
/// the one at fault. Note this deliberately differs from
/// `verify_pid_exe_stem`, whose `None => true` fallback is about
/// *reading an exe path* on platforms that cannot — not about authorising a
/// kill.
#[must_use]
pub fn daemon_identity_matches(expected: &kernal_api::daemon_identity::DaemonIdentity) -> bool {
    daemon_identity_matches_for(&default_endpoint(), expected)
}

/// Endpoint-scoped counterpart of [`daemon_identity_matches`] (#1904).
///
/// The comparison must be against the record belonging to `endpoint`: a
/// client managing a custom endpoint that verified itself against the default
/// endpoint's sidecar would authorise a kill of an unrelated daemon.
#[must_use]
pub fn daemon_identity_matches_for(
    endpoint: &str,
    expected: &kernal_api::daemon_identity::DaemonIdentity,
) -> bool {
    let Some(current) = read_backend_identity_for(endpoint) else {
        return false;
    };
    current.pid() == expected.pid()
        && current.started_at_unix_ms() == expected.started_at_unix_ms()
        && current.boot_id() == expected.boot_id()
}

/// Load and actively verify the daemon identity through the frozen v1
/// identity probe. Returns the verified identity when the recorded daemon
/// still serves `endpoint`.
#[must_use]
pub fn probe_backend_handle(endpoint: &str) -> Option<kernal_api::daemon_identity::DaemonIdentity> {
    // #1904: read the record belonging to `endpoint`. Reading the default
    // endpoint's sidecar here let a custom endpoint's probe verify against
    // the wrong daemon's identity.
    let daemon = read_backend_identity_for(endpoint)?;
    let endpoint = running_process_endpoint(endpoint);
    (daemon.probe_endpoint_blocking(&endpoint)
        == kernal_api::daemon_identity::ProbeSameEndpoint::Current)
        .then_some(daemon)
}

/// Returns true if `pid` exists **and** it is the daemon recorded for
/// `endpoint`. Defends against stale `daemon.lock` files where the recorded
/// PID has been recycled by an unrelated process — typical when a CI runner
/// restores a cache directory containing a lock file from a prior,
/// abruptly-terminated run. Identity verification authorizes a daemon-specific
/// kill; discovery retains a live PID with unavailable identity so
/// legacy/direct IPC can still prove ownership without destructively retiring
/// its endpoint. See issue #132.
#[must_use]
pub fn verify_daemon_pid(pid: u32) -> bool {
    verify_daemon_pid_for(&default_endpoint(), pid)
}

/// Endpoint-scoped counterpart of [`verify_daemon_pid`] (#1904).
#[must_use]
pub fn verify_daemon_pid_for(endpoint: &str, pid: u32) -> bool {
    verified_daemon_instance_for(endpoint, pid).is_some()
}

/// The recorded identity of `endpoint`'s daemon, if live `pid` provably is that
/// daemon (#1976). This is the positive proof a replacement needs before it may
/// stop an instance whose endpoint has gone: a missing record, a record naming
/// another PID and a changed binary all return `None`.
#[must_use]
pub fn verified_daemon_instance_for(
    endpoint: &str,
    pid: u32,
) -> Option<kernal_api::daemon_identity::DaemonIdentity> {
    let identity = read_backend_identity_for(endpoint)?;
    if identity.pid() != pid || recorded_in_another_pid_namespace(endpoint) {
        return None;
    }
    identity
        .verify_live()
        .is_ok_and(|verified| verified.is_alive())
        .then_some(identity)
}

/// Force-kill the persisted daemon instance through one retained verified
/// native control handle. `Ok(None)` means no persisted identity is available:
/// callers must preserve the live legacy/direct IPC endpoint rather than
/// reopening a PID and risking a recycled process.
pub fn force_kill_verified_daemon(
    pid: u32,
) -> Result<Option<kernal_api::daemon_identity::VerifiedDaemon>, ForceKillError> {
    force_kill_verified_daemon_for(&default_endpoint(), pid)
}

/// Endpoint-scoped counterpart of [`force_kill_verified_daemon`] (#1904).
///
/// This is the kill authorisation Failure A describes: the PID comes from the
/// endpoint's own lock, so the identity it is checked against must come from
/// that same endpoint's sidecar. Reading the default endpoint's record here
/// validated the wrong instance and terminated it.
pub fn force_kill_verified_daemon_for(
    endpoint: &str,
    pid: u32,
) -> Result<Option<kernal_api::daemon_identity::VerifiedDaemon>, ForceKillError> {
    let Some(identity) = read_backend_identity_for(endpoint) else {
        return Ok(None);
    };
    if identity.pid() != pid {
        return Ok(None);
    }
    let verified = identity.verify_for_control()?;
    verified.force_kill()?;
    Ok(Some(verified))
}
