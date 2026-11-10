//! Real namespace and alias regressions for output cleanup ownership.
use super::*;

fn state(directory: &Path) -> Arc<SharedState> {
    state_with_staging(directory, None)
}

fn state_with_staging(directory: &Path, staging: Option<&NormalizedPath>) -> Arc<SharedState> {
    let endpoint = crate::ipc::unique_test_endpoint();
    let identity = crate::ipc::current_backend_identity(&endpoint).unwrap();
    let (state, _writer) = crate::daemon::server::new_shared_state(
        &endpoint,
        &NormalizedPath::new(directory),
        staging,
        identity,
        None,
        None,
    )
    .unwrap();
    state
}

#[tokio::test]
async fn output_delivery_refuses_every_live_cache_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(&directory.path().join("cache"));
    for protected in [
        state.cache_dir.join("index.bin").to_path_buf(),
        state.artifact_dir.join("another-live-blob").to_path_buf(),
        state.staging.path().join("leased-output"),
    ] {
        std::fs::write(&protected, b"live").unwrap();
        let payloads = [CachedPayload::Bytes(Arc::new(b"replacement".to_vec()))];
        let result = write_payloads_par_observed_for_state(
            &state,
            std::slice::from_ref(&protected),
            &payloads,
            MaterializationMode::Copy,
        );
        assert!(
            result.is_err(),
            "output delivery accepted a live cache pathname"
        );
        assert_eq!(std::fs::read(&protected).unwrap(), b"live");
    }
}

#[tokio::test]
async fn output_scope_allows_missing_external_parents() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(&directory.path().join("cache"));
    let output = directory
        .path()
        .join("new-parent")
        .join("more")
        .join("output.o");
    let result = write_payloads_par_observed_for_state(
        &state,
        std::slice::from_ref(&output),
        &[CachedPayload::Bytes(Arc::new(b"output".to_vec()))],
        MaterializationMode::Copy,
    );
    assert!(result.is_ok());
    assert_eq!(std::fs::read(output).unwrap(), b"output");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn output_scope_rejects_symlinked_cache_parent_and_file_aliases() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(&directory.path().join("cache"));
    let protected = state.cache_dir.join("live");
    std::fs::write(&protected, b"live").unwrap();
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&state.cache_dir, &alias).unwrap();
    let scope = OutputScope::for_state(&state).unwrap();
    assert!(scope.output(&alias.join("live")).is_err());
    assert!(scope.output(&alias.join("missing").join("output")).is_err());
    let file_alias = directory.path().join("file-alias");
    std::os::unix::fs::symlink(&protected, &file_alias).unwrap();
    assert!(scope.output(&file_alias).is_err());
    assert_eq!(std::fs::read(protected).unwrap(), b"live");
}

#[test]
fn a_private_destination_cannot_claim_an_existing_or_sibling_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let live = directory.path().join("live-generation");
    std::fs::create_dir(&live).unwrap();
    let protected = live.join("output");
    std::fs::write(&protected, b"live").unwrap();
    assert!(PrivateOutputNamespace::create(&live).is_err());
    let private = PrivateOutputNamespace::create(&directory.path().join(".tmp-private")).unwrap();
    assert!(private.output(&protected).is_err());
    let destination = directory.path().join(".tmp-private").join("own-output");
    std::fs::write(&destination, b"partial").unwrap();
    private.output(&destination).unwrap().unlink().unwrap();
    assert!(!destination.exists());
    assert_eq!(std::fs::read(protected).unwrap(), b"live");
}

#[tokio::test]
async fn perf_output_scope_validation_does_not_scan_cache_contents() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(&directory.path().join("cache"));
    for index in 0..1000 {
        std::fs::write(state.artifact_dir.join(format!("entry-{index}")), b"x").unwrap();
    }
    let scope = OutputScope::for_state(&state).unwrap();
    let output = directory.path().join("requested-output");
    let started = std::time::Instant::now();
    for _ in 0..64 {
        scope.output(&output).unwrap();
    }
    assert!(
        started.elapsed() < std::time::Duration::from_millis(250),
        "output validation traversed the cache namespace"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn output_scope_dotdot_checks_the_exact_normalized_mutation_path() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(&directory.path().join("cache"));
    let outside = directory.path().join("outside").join("child");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, state.cache_dir.join("redirect")).unwrap();
    let protected = state.cache_dir.join("victim");
    std::fs::write(&protected, b"live").unwrap();
    let scope = OutputScope::for_state(&state).unwrap();
    for leaf in ["victim", "missing"] {
        let alias = state
            .cache_dir
            .as_path()
            .join("redirect")
            .join("..")
            .join(leaf);
        assert!(
            scope.output(alias.as_path()).is_err(),
            "validated and stored output names resolved differently"
        );
    }
    assert_eq!(std::fs::read(protected).unwrap(), b"live");
}

#[tokio::test]
async fn output_scope_also_protects_a_configured_external_staging_root() {
    let directory = tempfile::tempdir().unwrap();
    let configured = NormalizedPath::new(directory.path().join("external-staging"));
    let state = state_with_staging(&directory.path().join("cache"), Some(&configured));
    let protected = state.staging.path().join("leased-output");
    std::fs::write(&protected, b"live").unwrap();
    let result = write_payloads_par_observed_for_state(
        &state,
        std::slice::from_ref(&protected),
        &[CachedPayload::Bytes(Arc::new(b"replacement".to_vec()))],
        MaterializationMode::Copy,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(protected).unwrap(), b"live");
}
