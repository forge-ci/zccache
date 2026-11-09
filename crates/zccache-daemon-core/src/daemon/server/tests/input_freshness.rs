//! #1935: queued watcher events cannot authorize an old compiler artifact.

use super::super::*;

fn set_non_racy_mtime(path: &Path) {
    // These tests need a stat-only hit before editing. Recently hashed writes
    // must decline that shortcut (#1989), so age the fixture without sleeping.
    kernal_api::platform::fs::set_file_mtime(
        path,
        kernal_api::platform::fs::FileTime::from_unix_time(1_000_000_000, 0),
    )
    .unwrap();
}

#[tokio::test]
async fn fast_entry_checks_source_headers_force_includes_and_externs_without_events() {
    for changed in [
        "main.rs",
        "inc/header.h",
        "inc/force.h",
        "deps/external.rlib",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let server = super::bind_isolated_server(dir.path());
        let names = [
            "main.rs",
            "inc/header.h",
            "inc/force.h",
            "deps/external.rlib",
        ];
        let paths: Vec<NormalizedPath> = names.iter().map(|p| dir.path().join(p).into()).collect();
        for path in &paths {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"old").unwrap();
            set_non_racy_mtime(path);
            server.state.cache_system.journal().register(path.clone());
            server
                .state
                .cache_system
                .lookup_since(path, Clock::ZERO)
                .unwrap();
        }
        let ctx = CompileContext {
            source_file: paths[0].clone(),
            include_search: crate::depgraph::IncludeSearchPaths::default(),
            defines: vec![],
            undefines: vec![],
            flags: vec![],
            force_includes: vec![paths[2].clone()],
            unknown_flags: vec![],
            compiler_hash: crate::hash::hash_bytes(b"fixture"),
        };
        let graph = server.state.dep_graph.load();
        let registration = graph.register_rustc_with_key_and_root_result(
            ctx.context_key(),
            ctx,
            None,
            vec![("dep".into(), paths[3].clone())],
            None,
        );
        let scan = crate::depgraph::ScanResult {
            resolved: vec![paths[1].clone()],
            unresolved: vec![],
            has_computed: false,
        };
        let hash = |path: &Path| {
            std::fs::read(path)
                .ok()
                .map(|b| crate::hash::hash_bytes(&b))
        };
        let expected = graph
            .update(&registration.map_key, scan.clone(), hash)
            .unwrap()
            .hash()
            .to_hex();
        let clock = server.state.cache_system.current_clock();
        let fresh = || {
            context_artifact_is_fresh(
                &server.state,
                &registration.map_key,
                &paths[0],
                clock,
                None,
                &expected,
            )
        };
        let hits_before = graph.stats().hits;
        assert!(fresh(), "unchanged: {changed}");
        assert_eq!(graph.stats().hits, hits_before + 1);
        let accepted_hits = graph.stats().hits;
        let edited: NormalizedPath = dir.path().join(changed).into();
        std::fs::write(&edited, b"new").unwrap();
        // A later, still non-racy mtime lets the post-refresh checks below
        // compare actual hashes instead of declining the racy shortcut.
        // Same-tick rewrites have their own fscache regression test (#1989).
        kernal_api::platform::fs::set_file_mtime(
            &edited,
            kernal_api::platform::fs::FileTime::from_unix_time(1_000_000_010, 0),
        )
        .unwrap();
        assert!(!server
            .state
            .cache_system
            .journal()
            .changed_since(&edited, clock));
        assert!(!fresh(), "pending event: {changed}");
        assert_eq!(
            graph.stats().hits,
            accepted_hits,
            "refused pending event: {changed}"
        );
        // Even another request's updated metadata cannot prove the old key.
        server
            .state
            .cache_system
            .lookup_since(&edited, Clock::ZERO)
            .unwrap();
        assert!(!fresh(), "updated metadata: {changed}");
        assert_eq!(
            graph.stats().hits,
            accepted_hits,
            "refused updated metadata: {changed}"
        );
        // A newer depgraph key must not authorize an older fast-entry key.
        graph.update(&registration.map_key, scan, hash).unwrap();
        assert!(!fresh(), "updated context: {changed}");
        assert_eq!(
            graph.stats().hits,
            accepted_hits,
            "refused updated context: {changed}"
        );
    }
}

/// #1987 residual: the units of one multi-file request share one verdict per
/// input path. The first unit's check fills the memo with every stat-verified
/// input; later units read it instead of re-stating shared headers, so a
/// memo entry, not a fresh stat, decides for the rest of the request.
#[tokio::test]
async fn multi_request_memo_verifies_each_input_once() {
    let dir = tempfile::tempdir().unwrap();
    let server = super::bind_isolated_server(dir.path());
    let paths: Vec<NormalizedPath> = ["a.cpp", "inc/shared.h"]
        .iter()
        .map(|p| dir.path().join(p).into())
        .collect();
    for path in &paths {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"body").unwrap();
        set_non_racy_mtime(path);
        server.state.cache_system.journal().register(path.clone());
        server
            .state
            .cache_system
            .lookup_since(path, Clock::ZERO)
            .unwrap();
    }
    let ctx = CompileContext {
        source_file: paths[0].clone(),
        include_search: crate::depgraph::IncludeSearchPaths::default(),
        defines: vec![],
        undefines: vec![],
        flags: vec![],
        force_includes: vec![],
        unknown_flags: vec![],
        compiler_hash: crate::hash::hash_bytes(b"fixture"),
    };
    let graph = server.state.dep_graph.load();
    let key = graph.register(ctx);
    let scan = crate::depgraph::ScanResult {
        resolved: vec![paths[1].clone()],
        unresolved: vec![],
        has_computed: false,
    };
    let expected = graph
        .update(&key, scan, |path: &Path| {
            std::fs::read(path)
                .ok()
                .map(|b| crate::hash::hash_bytes(&b))
        })
        .unwrap()
        .hash()
        .to_hex();
    let clock = server.state.cache_system.current_clock();
    let memo = InputHashMemo::new();
    let fresh = |memo: Option<&InputHashMemo>| {
        context_artifact_is_fresh_with(&server.state, &key, &paths[0], clock, None, &expected, memo)
    };

    assert!(fresh(Some(&memo)), "first unit verifies and fills the memo");
    for path in &paths {
        assert!(
            memo.get(path).is_some_and(|hash| hash.is_some()),
            "memo holds the stat-verified hash of {}",
            path.display()
        );
    }
    assert_eq!(memo.len(), paths.len(), "one entry per distinct input");

    // A later unit trusts the request's verdict for a shared header instead
    // of stating it again: a recorded miss declines it even though the file
    // on disk is unchanged (and the memo-free check still accepts).
    memo.insert(paths[1].clone(), None);
    assert!(!fresh(Some(&memo)), "later unit reads the request verdict");
    assert!(fresh(None), "single-request path still stats");
}

/// #2002: size changes must invalidate an artifact without any watcher event,
/// including when the filesystem reports both writes in the same mtime tick.
#[tokio::test]
async fn size_change_rejects_fast_entry_without_events() {
    let dir = tempfile::tempdir().unwrap();
    let server = super::bind_isolated_server(dir.path());
    let source: NormalizedPath = dir.path().join("lib.rs").into();
    std::fs::write(&source, b"#[test] fn ok() {}\n").unwrap();
    set_non_racy_mtime(&source);
    let graph = server.state.dep_graph.load();
    let ctx = CompileContext {
        source_file: source.clone(),
        include_search: crate::depgraph::IncludeSearchPaths::default(),
        defines: vec![],
        undefines: vec![],
        flags: vec![],
        force_includes: vec![],
        unknown_flags: vec![],
        compiler_hash: crate::hash::hash_bytes(b"fixture"),
    };
    let key = graph.register(ctx);
    let expected = graph
        .update(
            &key,
            crate::depgraph::ScanResult {
                resolved: vec![],
                unresolved: vec![],
                has_computed: false,
            },
            |path| {
                std::fs::read(path)
                    .ok()
                    .map(|b| crate::hash::hash_bytes(&b))
            },
        )
        .unwrap()
        .hash()
        .to_hex();
    let journal = server.state.cache_system.journal();
    journal.register(source.clone());
    server
        .state
        .cache_system
        .lookup_since(&source, Clock::ZERO)
        .unwrap();
    let clock = server.state.cache_system.current_clock();
    let fresh = || context_artifact_is_fresh(&server.state, &key, &source, clock, None, &expected);
    assert!(fresh(), "unchanged source must retain its hit");
    std::fs::write(
        &source,
        b"pub fn broken() { missing_agent_fixture_symbol(); }\n",
    )
    .unwrap();
    // Keep mtime unchanged so size still invalidates the entry on coarse
    // filesystems; aging the baseline must not weaken this regression.
    set_non_racy_mtime(&source);
    assert!(!journal.changed_since(&source, clock));
    assert!(!fresh(), "size-changing edit authorized a stale artifact");
}
