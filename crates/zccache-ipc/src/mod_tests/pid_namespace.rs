//! #2035: a daemon record on a persistent cache dir can outlive its container.
//!
//! Containers share the host's `boot_id` but get fresh PID namespaces, so the
//! recorded PID can name an unrelated live process. Such a record must retire
//! itself instead of holding every later client in the recovery ladder.

use super::EnvGuard;
use crate::ownership::{pid_namespace_path_for, verify_error_proves_foreign};
use crate::platform::host::{parse_pid_namespace, pid_namespace};
use crate::{
    check_running_daemon_for, current_backend_identity, lock_file_path_for,
    read_backend_identity_for, verify_daemon_pid_for, write_backend_identity_for,
    write_lock_file_for,
};
use kernal_api::daemon_identity::{DaemonIdentity, DaemonIdentityRecord, DaemonVerifyError};
use std::path::{Path, PathBuf};

const FOREIGN_PID_NAMESPACE: &str = "pid:[1]";

fn endpoint_in(cache: &Path) -> String {
    cache.join("daemon.sock").to_string_lossy().into_owned()
}

/// The record of a daemon that really is this process, with exe and hash intact.
fn seed_own_record(endpoint: &str) -> u32 {
    let identity = current_backend_identity(endpoint).expect("capture own identity");
    write_lock_file_for(endpoint, identity.pid()).expect("seed lock");
    write_backend_identity_for(endpoint, &identity).expect("seed identity");
    identity.pid()
}

/// `seed_own_record`, with the digest or the pid replaced by the caller.
fn seed_edited_record(endpoint: &str, edit: impl FnOnce(&mut DaemonIdentityRecord)) -> u32 {
    let mut record = current_backend_identity(endpoint)
        .expect("capture own identity")
        .to_record();
    edit(&mut record);
    let identity = DaemonIdentity::from_record(record);
    write_lock_file_for(endpoint, identity.pid()).expect("seed lock");
    write_backend_identity_for(endpoint, &identity).expect("seed identity");
    identity.pid()
}

#[test]
fn a_record_from_another_pid_namespace_is_stale_even_when_the_exe_matches() {
    if pid_namespace().is_none() {
        return; // No PID namespaces on this host: the exe check below still applies.
    }
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    seed_own_record(&endpoint);
    std::fs::write(
        pid_namespace_path_for(&endpoint).as_path(),
        FOREIGN_PID_NAMESPACE,
    )
    .unwrap();
    #[cfg(unix)]
    let _listener = std::os::unix::net::UnixListener::bind(&endpoint).expect("bind stale socket");

    assert_eq!(check_running_daemon_for(&endpoint), None);
    assert!(!lock_file_path_for(&endpoint).as_path().exists());
    assert!(read_backend_identity_for(&endpoint).is_none());
    assert!(!pid_namespace_path_for(&endpoint).as_path().exists());
    #[cfg(unix)]
    assert!(!Path::new(&endpoint).exists(), "stale socket must retire");
}

#[test]
fn verify_daemon_pid_refuses_a_record_from_another_pid_namespace() {
    if pid_namespace().is_none() {
        return;
    }
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    let pid = seed_own_record(&endpoint);
    assert!(verify_daemon_pid_for(&endpoint, pid), "control: own record");

    std::fs::write(
        pid_namespace_path_for(&endpoint).as_path(),
        FOREIGN_PID_NAMESPACE,
    )
    .unwrap();
    assert!(!verify_daemon_pid_for(&endpoint, pid));
}

/// #1976: the proof a replacement needs before it may stop an instance.
#[test]
fn only_a_verifying_record_names_a_verified_instance() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    assert!(crate::verified_daemon_instance_for(&endpoint, std::process::id()).is_none());

    let pid = seed_own_record(&endpoint);
    let instance = crate::verified_daemon_instance_for(&endpoint, pid).expect("own record");
    assert_eq!(instance.pid(), pid);
    assert!(crate::verified_daemon_instance_for(&endpoint, pid + 1).is_none());

    seed_edited_record(&endpoint, |record| record.blake3_digest = [7; 32]);
    assert!(crate::verified_daemon_instance_for(&endpoint, pid).is_none());
}

#[test]
fn a_record_from_this_pid_namespace_stays_live() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    let pid = seed_own_record(&endpoint);

    assert_eq!(check_running_daemon_for(&endpoint), Some(pid));
    assert!(lock_file_path_for(&endpoint).as_path().exists());
    assert!(read_backend_identity_for(&endpoint).is_some());
}

/// The issue's exact state: same boot, recorded PID alive, but some other program.
#[cfg(unix)]
#[test]
fn a_pid_now_owned_by_another_program_is_stale() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn unrelated process");
    let other_pid = other.id();
    seed_edited_record(&endpoint, |record| record.pid = other_pid);

    let observed = check_running_daemon_for(&endpoint);
    let _ = other.kill();
    let _ = other.wait();

    assert_eq!(observed, None);
    assert!(!lock_file_path_for(&endpoint).as_path().exists());
}

/// A running daemon whose binary was replaced in place still serves clients
/// (`daemon_exe_overwrite`), so a digest change alone never retires it.
#[test]
fn a_changed_digest_alone_does_not_retire_a_live_daemon() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    let pid = seed_edited_record(&endpoint, |record| record.blake3_digest = [0xAB; 32]);

    assert_eq!(check_running_daemon_for(&endpoint), Some(pid));
    assert!(lock_file_path_for(&endpoint).as_path().exists());
}

/// RUNNING_PROCESS_DISABLE and legacy direct IPC run without a sidecar.
#[test]
fn a_live_pid_without_an_identity_record_is_kept() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    let pid = std::process::id();
    write_lock_file_for(&endpoint, pid).unwrap();

    assert_eq!(check_running_daemon_for(&endpoint), Some(pid));
}

/// A lock written before this identity's daemon got that far is not a verdict
/// about the process the lock names.
#[cfg(unix)]
#[test]
fn an_identity_for_a_different_pid_says_nothing_about_the_lock() {
    let cache = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set_cache_dir(cache.path());
    let endpoint = endpoint_in(cache.path());
    seed_own_record(&endpoint);
    std::fs::write(
        pid_namespace_path_for(&endpoint).as_path(),
        FOREIGN_PID_NAMESPACE,
    )
    .unwrap();
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn unrelated process");
    let other_pid = other.id();
    write_lock_file_for(&endpoint, other_pid).unwrap();

    let observed = check_running_daemon_for(&endpoint);
    let _ = other.kill();
    let _ = other.wait();

    assert_eq!(observed, Some(other_pid));
    assert!(read_backend_identity_for(&endpoint).is_some());
}

#[test]
fn only_conclusive_verification_failures_prove_a_foreign_record() {
    let pid = 7;
    let path_mismatch = |actual: &str| DaemonVerifyError::ExecutablePathMismatch {
        pid,
        expected: PathBuf::from("/d/zccache-daemon"),
        actual: PathBuf::from(actual),
    };
    assert!(verify_error_proves_foreign(&path_mismatch("/usr/bin/bash")));
    assert!(verify_error_proves_foreign(
        &DaemonVerifyError::StaleProcess { pid }
    ));
    assert!(verify_error_proves_foreign(
        &DaemonVerifyError::BootIdMismatch {
            expected: "a".into(),
            actual: "b".into()
        }
    ));

    assert!(!verify_error_proves_foreign(&path_mismatch(
        "/d/zccache-daemon (deleted)"
    )));
    assert!(!verify_error_proves_foreign(
        &DaemonVerifyError::BootIdMismatch {
            expected: "a".into(),
            actual: "unavailable".into()
        }
    ));
    assert!(!verify_error_proves_foreign(
        &DaemonVerifyError::ExecutableHashMismatch { pid }
    ));
    assert!(!verify_error_proves_foreign(&DaemonVerifyError::NotFound {
        pid
    }));
}

#[test]
fn the_pid_namespace_is_cut_out_of_the_facade_namespace_id() {
    assert_eq!(
        parse_pid_namespace("mnt:[4026531841]:pid:[4026531836]").as_deref(),
        Some("pid:[4026531836]")
    );
    assert_eq!(parse_pid_namespace("mntns:unknown:pidns:unknown"), None);
}
