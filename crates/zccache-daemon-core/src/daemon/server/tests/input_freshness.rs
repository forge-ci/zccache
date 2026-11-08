//! #1935: queued watcher events cannot authorize an old compiler artifact.

use super::super::*;

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
        // #2002: a normal size-changing edit must be detected even if the
        // watcher has not delivered any event. Size makes this deterministic
        // on filesystems whose timestamp resolution coalesces both writes.
        std::fs::write(&edited, b"normal size-changing source edit").unwrap();
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
