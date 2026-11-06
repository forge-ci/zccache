use super::*;

fn file_time(path: &Path) -> kernal_api::platform::fs::FileTime {
    kernal_api::platform::fs::FileTime::from_last_modification_time(
        &std::fs::metadata(path).unwrap(),
    )
}

#[test]
fn rustc_compat_target_keeps_requested_metadata_name_but_restores_observed_wasm_suffix() {
    let root = tempfile::tempdir().unwrap();
    let requested_metadata: NormalizedPath = root.path().join("libchecked-warm.rmeta").into();
    assert_eq!(
        rustc_compat_materialization_target(&requested_metadata, "libchecked-cold.rmeta",),
        requested_metadata,
        "a compatible metadata request owns its current Cargo identity"
    );

    let requested_wasm: NormalizedPath = root.path().join("wasm_restore").into();
    assert_eq!(
        rustc_compat_materialization_target(&requested_wasm, "wasm_restore.wasm"),
        NormalizedPath::from(root.path().join("wasm_restore.wasm")),
        "an extensionless Rustc plan must retain the compiler-observed Wasm suffix"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_compatible_rustc_hit_requires_matching_verdict_and_replays_its_streams() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let state = server.state.as_ref();
    let source_path: NormalizedPath = dir.path().join("source.rs").into();
    // A Dylint driver can reuse metadata from a cold compilation while
    // Cargo gives the compatible request a different identity-derived
    // destination. The cached name chooses the payload; the current
    // request must choose where that payload lands.
    let output_path: NormalizedPath = dir.path().join("libfoldhash-new.rmeta").into();
    let cache_path = state.artifact_dir.join("artifact-key_0");
    std::fs::write(&cache_path, b"artifact bytes").unwrap();
    write_authoritative_blob_digest(&cache_path).unwrap();
    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.path().into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    let mut artifact_meta = ArtifactIndex::new(
        vec!["libfoldhash-cold.rmeta".to_string()],
        vec![14],
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "artifact-key".to_string(),
        CachedArtifact::from_file_payloads(artifact_meta.clone(), vec![cache_path.clone()]),
    );

    let materialize = || {
        materialize_cached_compile_hit(CachedHitMaterializeRequest {
            state,
            sid: &sid,
            artifact_key_hex: "artifact-key",
            verdict_key_hex: Some("verdict-key"),
            source_path: &source_path,
            output_path: &output_path,
            secondary_output_dir: dir.path().into(),
            current_depfile_dest: None,
            current_rustc_out_dir: None,
            depfile_key_root: None,
            compile_start: Instant::now(),
            hit_label: "HIT_TEST",
            cached_error_label: "CACHED_ERROR_TEST",
            record_compilation: false,
            downgrade_output_metadata: false,
            mtime_floor_paths: Vec::new(),
            rustc_metadata_compat_outputs: Some(vec![output_path.clone()]),
            rustc_archive_hardlink_eligible: Some(true),
            materialization_mode: MaterializationMode::Auto,
            pch_mtime_replay: true,
            phases: CachedHitPhases::request_cache(0, 0),
        })
    };

    assert!(matches!(
        materialize(),
        Err(CachedHitFailure::VerdictMissing)
    ));
    assert!(
        !output_path.is_file(),
        "missing verdict must gate artifact replay"
    );

    let requested_stdout = format!("error stdout: {}", output_path.display()).into_bytes();
    let requested_stderr = format!("lint error: {}", output_path.display()).into_bytes();
    let canonical_stdout = canonicalize_staged_output_bytes(&requested_stdout, dir.path());
    let canonical_stderr = canonicalize_staged_output_bytes(&requested_stderr, dir.path());
    assert!(contains_staged_output_marker(&canonical_stdout));
    assert!(contains_staged_output_marker(&canonical_stderr));
    artifact_meta.rustc_verdicts.insert(
        "verdict-key".to_string(),
        ArtifactVerdict {
            stdout: Arc::new(canonical_stdout),
            stderr: Arc::new(canonical_stderr),
            exit_code: 1,
        },
    );
    state.artifacts.insert(
        "artifact-key".to_string(),
        CachedArtifact::from_file_payloads(artifact_meta.clone(), vec![cache_path.clone()]),
    );
    let response = materialize().unwrap();
    assert!(matches!(
        response,
        Response::CompileResult {
            exit_code: 1,
            cached: true,
            ref stdout,
            ref stderr,
        } if stdout.as_slice() == requested_stdout
            && stderr.as_slice() == requested_stderr
    ));
    assert!(
        !output_path.is_file(),
        "an error verdict must never materialize shared success outputs"
    );

    artifact_meta.rustc_verdicts.insert(
        "verdict-key".to_string(),
        ArtifactVerdict {
            stdout: Arc::new(b"verdict stdout".to_vec()),
            stderr: Arc::new(b"lint diagnostic".to_vec()),
            exit_code: 0,
        },
    );
    state.artifacts.insert(
        "artifact-key".to_string(),
        CachedArtifact::from_file_payloads(artifact_meta, vec![cache_path]),
    );
    let response = materialize().unwrap();
    assert!(matches!(
        response,
        Response::CompileResult {
            exit_code: 0,
            cached: true,
            ref stdout,
            ref stderr,
        } if stdout.as_slice() == b"verdict stdout"
            && stderr.as_slice() == b"lint diagnostic"
    ));
    assert_eq!(std::fs::read(output_path).unwrap(), b"artifact bytes");
    assert!(
        !dir.path().join("libfoldhash-cold.rmeta").exists(),
        "a metadata-compatible hit must not redirect output to the cached identity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn target_paths_get_fresh_mtime_through_shared_materializer() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let state = server.state.as_ref();
    let cache_dir = state.artifact_dir.clone();
    let source_path: NormalizedPath = dir.path().join("source.cc").into();
    let output_path: NormalizedPath = dir.path().join("output.o").into();
    let cache_path = cache_dir.join("artifact-key_0");
    let payload = Arc::new(b"compiled object".to_vec());
    let _ = crate::platform::fs::permissions::make_writable(&cache_path);
    std::fs::write(&cache_path, payload.as_slice()).unwrap();
    write_authoritative_blob_digest(&cache_path).unwrap();

    let old_time = kernal_api::platform::fs::FileTime::from_unix_time(1_000_000_000, 0);
    kernal_api::platform::fs::set_file_mtime(&cache_path, old_time).unwrap();

    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.path().into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    let meta = ArtifactIndex::new(
        vec!["output.o".to_string()],
        vec![payload.len() as u64],
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "artifact-key".to_string(),
        CachedArtifact::from_file_payloads(meta, vec![cache_path]),
    );

    let response = materialize_cached_compile_hit(CachedHitMaterializeRequest {
        state,
        sid: &sid,
        artifact_key_hex: "artifact-key",
        verdict_key_hex: None,
        source_path: &source_path,
        output_path: &output_path,
        secondary_output_dir: dir.path().into(),
        current_depfile_dest: None,
        current_rustc_out_dir: None,
        depfile_key_root: None,
        compile_start: Instant::now(),
        hit_label: "HIT_TEST",
        cached_error_label: "CACHED_ERROR_TEST",
        record_compilation: true,
        downgrade_output_metadata: true,
        mtime_floor_paths: Vec::new(),
        rustc_metadata_compat_outputs: None,
        rustc_archive_hardlink_eligible: None,
        materialization_mode: MaterializationMode::Auto,
        pch_mtime_replay: true,
        phases: CachedHitPhases::request_cache(0, 0),
    })
    .unwrap();

    assert!(matches!(
        response,
        Response::CompileResult {
            cached: true,
            exit_code: 0,
            ..
        }
    ));
    assert_eq!(std::fs::read(&output_path).unwrap(), payload.as_slice());
    let output_time = file_time(&output_path);
    assert!(
        output_time.unix_seconds() > old_time.unix_seconds(),
        "compile-hit output must be fresher than stale cache artifact; \
         output={output_time:?}, cache={old_time:?}",
    );
    assert_eq!(state.stats.snapshot().compilations, 1);
    assert_eq!(state.stats.snapshot().hits, 1);
}

/// Issue #643: when zccache wraps `clang++ -MD -MF <depfile>` the user's
/// depfile is part of the build-system's incremental-rebuild contract
/// (e.g. `deps = gcc` in ninja). On a cache hit we currently restore
/// only the `.obj` — the `.d` is silently absent, the build tool records
/// zero dependencies for the object, and from then on it never
/// recompiles when included headers change. Result: stale objects and
/// mysterious `undefined symbol` link errors after `git pull`.
///
/// This test pins the fix: a cached artifact with two payloads (`.obj`
/// at index 0, depfile at index 1) plus an explicit current-build depfile
/// destination must restore BOTH files. The cached `name` of the
/// depfile output is just an identifier — the real destination on hit
/// is supplied by the caller (it comes from the current compile's
/// `-MF` argument, not from where the depfile happened to live when
/// the cache miss recorded it).
#[tokio::test(flavor = "current_thread")]
async fn cached_hit_restores_user_depfile_alongside_object() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let state = server.state.as_ref();
    let cache_dir = state.artifact_dir.clone();

    let source_path: NormalizedPath = dir.path().join("source.cc").into();
    let output_path: NormalizedPath = dir.path().join("source.o").into();
    // Critical: the destination depfile path used by *this* build is
    // not necessarily the cached basename. The caller derives it from
    // the current invocation's `-MF` (or default `<output>.d`) and
    // passes it in. Use a different filename to prove the fix routes
    // bytes by request, not by stored name.
    let depfile_dest: NormalizedPath = dir.path().join("build/out/deps.mk").into();

    let obj_payload = Arc::new(b"compiled object bytes".to_vec());
    let dep_payload = Arc::new(
        format!(
            "{STAGED_OUTPUT_REMAP_ROOT}/source.o: source.cc header_a.h header_b.h\n\n\
             header_a.h:\n\nheader_b.h:\n"
        )
        .into_bytes(),
    );
    let cached_stdout =
        Arc::new(format!("artifact:{STAGED_OUTPUT_REMAP_ROOT}/source.o\n").into_bytes());
    let obj_cache_path = cache_dir.join("depfile-key_0");
    let dep_cache_path = cache_dir.join("depfile-key_1");
    let _ = crate::platform::fs::permissions::make_writable(&obj_cache_path);
    let _ = crate::platform::fs::permissions::make_writable(&dep_cache_path);
    std::fs::write(&obj_cache_path, obj_payload.as_slice()).unwrap();
    std::fs::write(&dep_cache_path, dep_payload.as_slice()).unwrap();
    write_authoritative_blob_digest(&obj_cache_path).unwrap();
    write_authoritative_blob_digest(&dep_cache_path).unwrap();

    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.path().into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });

    // The cached `name[1]` is the basename from the original miss
    // (e.g. "source.o.d"). The destination on hit is independent and
    // comes from the current request.
    let meta = ArtifactIndex::new(
        vec!["source.o".to_string(), "source.o.d".to_string()],
        vec![obj_payload.len() as u64, dep_payload.len() as u64],
        cached_stdout,
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "depfile-key".to_string(),
        CachedArtifact::from_file_payloads(meta, vec![obj_cache_path, dep_cache_path]),
    );

    let response = materialize_cached_compile_hit(CachedHitMaterializeRequest {
        state,
        sid: &sid,
        artifact_key_hex: "depfile-key",
        verdict_key_hex: None,
        source_path: &source_path,
        output_path: &output_path,
        secondary_output_dir: dir.path().into(),
        current_depfile_dest: Some(depfile_dest.clone()),
        current_rustc_out_dir: None,
        depfile_key_root: None,
        compile_start: Instant::now(),
        hit_label: "HIT_TEST",
        cached_error_label: "CACHED_ERROR_TEST",
        record_compilation: true,
        downgrade_output_metadata: true,
        mtime_floor_paths: Vec::new(),
        rustc_metadata_compat_outputs: None,
        rustc_archive_hardlink_eligible: None,
        materialization_mode: MaterializationMode::Auto,
        pch_mtime_replay: true,
        phases: CachedHitPhases::request_cache(0, 0),
    })
    .expect("materialize_cached_compile_hit must succeed");
    let Response::CompileResult {
        cached,
        exit_code,
        stdout,
        ..
    } = response
    else {
        panic!("expected cached compile result");
    };
    assert!(cached);
    assert_eq!(exit_code, 0);
    assert_eq!(
        stdout.as_slice(),
        format!("artifact:{}\n", output_path.display()).as_bytes()
    );

    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        obj_payload.as_slice(),
        "cache hit must restore the object at its destination",
    );
    assert!(
        depfile_dest.as_path().exists(),
        "cache hit must restore the depfile at the *current* build's -MF \
         destination ({}), not the cached basename — this is the #643 \
         stale-incremental-build fix",
        depfile_dest.display(),
    );
    let restored = std::fs::read_to_string(depfile_dest.as_path()).unwrap();
    assert!(!restored.contains(STAGED_OUTPUT_REMAP_ROOT));
    assert!(
        restored.starts_with(&format!("{}:", output_path.display())),
        "restored depfile target must use the current output path: {restored}"
    );
}

/// One hit against an artifact whose payloads are `(name, bytes)`, with the
/// current request asking for a depfile at `<dir>/source.o.d`.
fn hit_requesting_depfile(
    dir: &Path,
    state: &SharedState,
    payloads: &[(&str, &[u8])],
) -> (
    Result<Response, CachedHitFailure>,
    NormalizedPath,
    NormalizedPath,
) {
    let source_path: NormalizedPath = dir.join("source.cc").into();
    let output_path: NormalizedPath = dir.join("source.o").into();
    let depfile_dest: NormalizedPath = dir.join("source.o.d").into();
    let mut paths = Vec::new();
    for (index, (_, bytes)) in payloads.iter().enumerate() {
        let path = state.artifact_dir.join(format!("depfile-1992_{index}"));
        std::fs::write(&path, bytes).unwrap();
        write_authoritative_blob_digest(&path).unwrap();
        paths.push(path);
    }
    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    let meta = ArtifactIndex::new(
        payloads
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect(),
        payloads
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .collect(),
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "depfile-1992".to_string(),
        CachedArtifact::from_file_payloads(meta, paths),
    );
    let result = materialize_cached_compile_hit(CachedHitMaterializeRequest {
        state,
        sid: &sid,
        artifact_key_hex: "depfile-1992",
        verdict_key_hex: None,
        source_path: &source_path,
        output_path: &output_path,
        secondary_output_dir: dir.into(),
        current_depfile_dest: Some(depfile_dest.clone()),
        current_rustc_out_dir: None,
        depfile_key_root: None,
        compile_start: Instant::now(),
        hit_label: "HIT_TEST",
        cached_error_label: "CACHED_ERROR_TEST",
        record_compilation: true,
        downgrade_output_metadata: false,
        mtime_floor_paths: Vec::new(),
        rustc_metadata_compat_outputs: None,
        rustc_archive_hardlink_eligible: None,
        materialization_mode: MaterializationMode::Auto,
        pch_mtime_replay: true,
        phases: CachedHitPhases::request_cache(0, 0),
    });
    (result, output_path, depfile_dest)
}

fn failure_name(result: Result<Response, CachedHitFailure>) -> &'static str {
    match result {
        Ok(_) => "served",
        Err(failure) => failure.miss_bucket().1,
    }
}

/// #1992: Ninja reads a missing depfile as "no dependencies" and never rebuilds
/// the object after a header edit, so an entry without a depfile payload is a
/// miss for a compile that asked for one. Nothing is written first.
#[tokio::test(flavor = "current_thread")]
async fn object_only_artifact_is_a_miss_when_the_request_wants_a_depfile() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let (result, output, depfile) = hit_requesting_depfile(
        dir.path(),
        server.state.as_ref(),
        &[("source.o", b"object only")],
    );
    assert_eq!(failure_name(result), "depfile_invalid");
    assert!(!output.as_path().exists(), "a refused hit writes no object");
    assert!(!depfile.as_path().exists());
}

/// #1992: an import-provenance sidecar at index 1 is not a depfile.
#[tokio::test(flavor = "current_thread")]
async fn pch_with_only_a_provenance_sidecar_is_a_miss_when_the_request_wants_a_depfile() {
    let dir = tempfile::tempdir().unwrap();
    let sidecar = format!("source.gch{}", super::super::pch_provenance::SIDECAR_SUFFIX);
    let output: NormalizedPath = dir.path().join("source.gch").into();
    assert!(!cached_depfile_payload_present(
        &["source.gch".to_string(), sidecar],
        &[4, 4],
        &output
    ));
}

/// #1992: entries already persisted with an empty or malformed depfile are
/// never served, whichever way the bytes went bad.
#[tokio::test(flavor = "current_thread")]
async fn persisted_bad_depfile_payloads_are_a_miss() {
    let cases: [&[u8]; 4] = [b"", b" \n\n", b"source.o:\n", &[0xff, 0xfe]];
    for bad in cases {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
        let (result, _, _) = hit_requesting_depfile(
            dir.path(),
            server.state.as_ref(),
            &[("source.o", b"object"), ("source.o.d", bad)],
        );
        assert_eq!(failure_name(result), "depfile_invalid", "payload {bad:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn valid_depfile_payload_is_served_to_the_requested_path() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let (result, _, depfile) = hit_requesting_depfile(
        dir.path(),
        server.state.as_ref(),
        &[
            ("source.o", b"object"),
            ("source.o.d", b"source.o: source.cc a.h\n"),
        ],
    );
    assert_eq!(failure_name(result), "served");
    assert_eq!(
        std::fs::read(depfile.as_path()).unwrap(),
        b"source.o: source.cc a.h\n"
    );
}

/// A compile that did not ask for a depfile keeps using object-only entries.
#[tokio::test(flavor = "current_thread")]
async fn object_only_artifact_still_serves_a_request_without_a_depfile() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let state = server.state.as_ref();
    let path = state.artifact_dir.join("no-depfile_0");
    std::fs::write(&path, b"object").unwrap();
    write_authoritative_blob_digest(&path).unwrap();
    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.path().into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    let meta = ArtifactIndex::new(
        vec!["source.o".to_string()],
        vec![6],
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "no-depfile".to_string(),
        CachedArtifact::from_file_payloads(meta, vec![path]),
    );
    let source_path: NormalizedPath = dir.path().join("source.cc").into();
    let output_path: NormalizedPath = dir.path().join("source.o").into();
    materialize_cached_compile_hit(CachedHitMaterializeRequest {
        state,
        sid: &sid,
        artifact_key_hex: "no-depfile",
        verdict_key_hex: None,
        source_path: &source_path,
        output_path: &output_path,
        secondary_output_dir: dir.path().into(),
        current_depfile_dest: None,
        current_rustc_out_dir: None,
        depfile_key_root: None,
        compile_start: Instant::now(),
        hit_label: "HIT_TEST",
        cached_error_label: "CACHED_ERROR_TEST",
        record_compilation: true,
        downgrade_output_metadata: false,
        mtime_floor_paths: Vec::new(),
        rustc_metadata_compat_outputs: None,
        rustc_archive_hardlink_eligible: None,
        materialization_mode: MaterializationMode::Auto,
        pch_mtime_replay: true,
        phases: CachedHitPhases::request_cache(0, 0),
    })
    .expect("no depfile was requested");
    assert_eq!(std::fs::read(&output_path).unwrap(), b"object");
}

/// Issue #460: warm-hit materialization should stay under budget — the
/// fix collapsed 9 clock reads per hit to 4. A future regression that
/// reintroduces a syscall-per-phase pattern (or worse, a synchronous I/O
/// call) on the hit path would bust this budget. 100 iterations / 1 s
/// gives ~50× headroom on Linux Docker and ~5× on Windows CI (Defender +
/// shared-runner jitter typically lands warm-hit timings around 2 ms each
/// on those runners; native Windows hosts measure ~150–250 µs/hit).
#[tokio::test(flavor = "current_thread")]
async fn warm_hit_materialization_under_budget() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(dir.path());
    let state = server.state.as_ref();
    let cache_dir = state.artifact_dir.clone();
    let source_path: NormalizedPath = dir.path().join("source.cc").into();
    let cache_path = cache_dir.join("budget-key_0");
    let payload = Arc::new(b"compiled object".to_vec());
    let _ = crate::platform::fs::permissions::make_writable(&cache_path);
    std::fs::write(&cache_path, payload.as_slice()).unwrap();
    write_authoritative_blob_digest(&cache_path).unwrap();

    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.path().into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    let meta = ArtifactIndex::new(
        vec!["output.o".to_string()],
        vec![payload.len() as u64],
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        0,
    );
    state.artifacts.insert(
        "budget-key".to_string(),
        CachedArtifact::from_file_payloads(meta, vec![cache_path]),
    );

    const SAMPLES: u32 = 3;
    const ITERATIONS: u32 = 100;
    let mut best = std::time::Duration::MAX;
    for sample in 0..SAMPLES {
        let start = Instant::now();
        for i in 0..ITERATIONS {
            let output_path: NormalizedPath = dir.path().join(format!("out-{sample}-{i}.o")).into();
            let response = materialize_cached_compile_hit(CachedHitMaterializeRequest {
                state,
                sid: &sid,
                artifact_key_hex: "budget-key",
                verdict_key_hex: None,
                source_path: &source_path,
                output_path: &output_path,
                secondary_output_dir: dir.path().into(),
                current_depfile_dest: None,
                current_rustc_out_dir: None,
                depfile_key_root: None,
                compile_start: Instant::now(),
                hit_label: "HIT_TEST",
                cached_error_label: "CACHED_ERROR_TEST",
                record_compilation: true,
                downgrade_output_metadata: false,
                mtime_floor_paths: Vec::new(),
                rustc_metadata_compat_outputs: None,
                rustc_archive_hardlink_eligible: None,
                materialization_mode: MaterializationMode::Auto,
                pch_mtime_replay: true,
                phases: CachedHitPhases::request_cache(0, 0),
            })
            .expect("materialize_cached_compile_hit must succeed");
            assert!(matches!(
                response,
                Response::CompileResult {
                    cached: true,
                    exit_code: 0,
                    ..
                }
            ));
        }
        best = best.min(start.elapsed());
    }
    let budget = if crate::platform::host::is_windows() {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_secs(1)
    };
    assert!(
        best < budget,
        "warm-hit materialization regressed: best of {SAMPLES} samples of \
         {ITERATIONS} hits took {best:?} \
         (budget: {budget:?}; avg {:?}/hit)",
        best / ITERATIONS
    );
    assert_eq!(state.stats.snapshot().hits as u32, SAMPLES * ITERATIONS);
}

/// Materialize one cached C/C++ or rustc output with `floor_input` as the
/// only mtime-floor input, and return the output's resulting mtime.
fn materialize_with_floor_input(
    rustc_archive_hardlink_eligible: Option<bool>,
    output_name: &str,
    floor_input: &Path,
    dir: &Path,
) -> std::time::SystemTime {
    let server = crate::daemon::server::tests::bind_isolated_server(dir);
    let state = server.state.as_ref();
    let source_path: NormalizedPath = dir.join("unit.src").into();
    let output_path: NormalizedPath = dir.join(output_name).into();
    let cache_path = state.artifact_dir.join("floor-key_0");
    std::fs::write(&cache_path, b"object bytes").unwrap();
    write_authoritative_blob_digest(&cache_path).unwrap();
    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: dir.into(),
        log_file: None,
        track_stats: false,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    state.artifacts.insert(
        "floor-key".to_string(),
        CachedArtifact::from_file_payloads(
            ArtifactIndex::new(
                vec![output_name.to_string()],
                vec![12],
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                0,
            ),
            vec![cache_path],
        ),
    );
    materialize_cached_compile_hit(CachedHitMaterializeRequest {
        state,
        sid: &sid,
        artifact_key_hex: "floor-key",
        verdict_key_hex: None,
        source_path: &source_path,
        output_path: &output_path,
        secondary_output_dir: dir.into(),
        current_depfile_dest: None,
        current_rustc_out_dir: None,
        depfile_key_root: None,
        compile_start: Instant::now(),
        hit_label: "HIT_TEST",
        cached_error_label: "CACHED_ERROR_TEST",
        record_compilation: false,
        downgrade_output_metadata: false,
        mtime_floor_paths: vec![floor_input.into()],
        rustc_metadata_compat_outputs: None,
        rustc_archive_hardlink_eligible,
        materialization_mode: MaterializationMode::Auto,
        pch_mtime_replay: true,
        phases: CachedHitPhases::request_cache(0, 0),
    })
    .expect("cached hit materializes");
    std::fs::metadata(output_path.as_path())
        .unwrap()
        .modified()
        .unwrap()
}

fn future_dated_file(path: &Path) -> std::time::SystemTime {
    std::fs::write(path, b"input").unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(24 * 60 * 60);
    kernal_api::platform::fs::set_file_mtime(
        path,
        kernal_api::platform::fs::FileTime::from_system_time(future),
    )
    .unwrap();
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// A C/C++ hit is stamped with the batch floor's `now()` seed, exactly as a
/// bare compiler stamps its output; make/ninja compare that against the
/// source and headers. It must not stat every recorded input to find a
/// larger mtime: with ~70 headers that was 165 us of a ~0.4 ms warm hit, and
/// it grows with the include set. So a future-dated header is not consulted.
#[tokio::test(flavor = "current_thread")]
async fn perf_cc_hit_mtime_floor_does_not_stat_recorded_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let header = dir.path().join("future.h");
    let header_mtime = future_dated_file(&header);

    let output_mtime = materialize_with_floor_input(None, "unit.o", &header, dir.path());

    assert!(
        output_mtime < header_mtime,
        "a C/C++ hit must not floor its object to input mtimes; \
         output={output_mtime:?}, future header={header_mtime:?}"
    );
}

/// Rustc hits keep the input floor: cargo's fingerprint check treats an
/// extern newer than the output as stale (#599), and the extern set is small.
#[tokio::test(flavor = "current_thread")]
async fn rustc_hit_mtime_floor_still_covers_recorded_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let extern_rlib = dir.path().join("libdep.rlib");
    let extern_mtime = future_dated_file(&extern_rlib);

    let output_mtime =
        materialize_with_floor_input(Some(true), "libunit.rlib", &extern_rlib, dir.path());

    assert!(
        output_mtime >= extern_mtime,
        "a rustc hit must stay at least as new as its externs; \
         output={output_mtime:?}, future extern={extern_mtime:?}"
    );
}
