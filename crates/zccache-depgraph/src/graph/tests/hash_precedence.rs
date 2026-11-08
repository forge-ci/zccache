//! #1935: a measured content change overrides watcher journal silence.

use super::*;
use crate::{CompileContext, IncludeSearchPaths, ScanResult};

#[test]
fn fresh_hash_overrides_journal_for_every_input_and_verdict_path() {
    for changed in [
        "/src/main.rs",
        "/inc/header.h",
        "/inc/force.h",
        "/dep/lib.rlib",
    ] {
        for mode in ["plain", "diagnostic", "metadata"] {
            let graph = DepGraph::new();
            let ctx = CompileContext {
                source_file: "/src/main.rs".into(),
                include_search: IncludeSearchPaths::default(),
                defines: vec![],
                undefines: vec![],
                flags: vec![],
                force_includes: vec!["/inc/force.h".into()],
                unknown_flags: vec![],
                compiler_hash: zccache_hash::hash_bytes(b"compiler"),
            };
            let registration = graph.register_rustc_with_key_and_root_result(
                ctx.context_key(),
                ctx,
                None,
                vec![("dep".into(), "/dep/lib.rlib".into())],
                Some(ContextKey::from_raw([3; 32])),
            );
            let key = registration.map_key;
            let original =
                |path: &Path| Some(zccache_hash::hash_bytes(path.to_string_lossy().as_bytes()));
            graph
                .update(
                    &key,
                    ScanResult {
                        resolved: vec!["/inc/header.h".into()],
                        unresolved: vec![],
                        has_computed: false,
                    },
                    original,
                )
                .unwrap();
            let current = |path: &Path| {
                if path == Path::new(changed) {
                    Some(zccache_hash::hash_bytes(b"changed"))
                } else {
                    original(path)
                }
            };
            let verdict = match mode {
                "plain" => graph.check(&key, |_| true, current),
                "diagnostic" => graph.check_diagnostic(&key, |_| true, current).0,
                _ => {
                    graph
                        .check_rustc_metadata_compat_diagnostic(
                            &registration.metadata_compat_map_key.unwrap(),
                            &[("dep".into(), "/dep/lib.rlib".into())],
                            |_| true,
                            current,
                        )
                        .0
                }
            };
            assert!(
                !matches!(verdict, CacheVerdict::Hit { .. }),
                "{mode}: {changed}: {verdict:?}"
            );
        }
    }
}

/// #2002: real size-changing edits override journal silence on both the
/// ordinary build verdict and the metadata alias used by `check` after `test`.
#[test]
fn normal_source_edit_invalidates_every_verdict_without_events() {
    for changed in ["source", "extern"] {
        for mode in ["plain", "diagnostic", "metadata"] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("lib.rs");
            std::fs::write(&source, b"#[test] fn ok() {}\n").unwrap();
            let external = dir.path().join("libdep.rlib");
            std::fs::write(&external, b"old dependency").unwrap();
            let externs = vec![("dep".into(), external.clone().into())];
            let graph = DepGraph::new();
            let ctx = CompileContext {
                source_file: source.clone().into(),
                include_search: IncludeSearchPaths::default(),
                defines: vec![],
                undefines: vec![],
                flags: vec![],
                force_includes: vec![],
                unknown_flags: vec![],
                compiler_hash: zccache_hash::hash_bytes(b"compiler"),
            };
            let registration = graph.register_rustc_with_key_and_root_result(
                ctx.context_key(),
                ctx,
                None,
                externs.clone(),
                Some(ContextKey::from_raw([3; 32])),
            );
            let hash = |path: &Path| {
                std::fs::read(path)
                    .ok()
                    .map(|b| zccache_hash::hash_bytes(&b))
            };
            graph
                .update(
                    &registration.map_key,
                    ScanResult {
                        resolved: vec![],
                        unresolved: vec![],
                        has_computed: false,
                    },
                    hash,
                )
                .unwrap();
            let verdict = || match mode {
                "plain" => graph.check(&registration.map_key, |_| true, hash),
                "diagnostic" => {
                    graph
                        .check_diagnostic(&registration.map_key, |_| true, hash)
                        .0
                }
                _ => {
                    graph
                        .check_rustc_metadata_compat_diagnostic(
                            &registration.metadata_compat_map_key.unwrap(),
                            &externs,
                            |_| true,
                            hash,
                        )
                        .0
                }
            };
            assert!(
                matches!(verdict(), CacheVerdict::Hit { .. }),
                "unchanged: {mode}"
            );
            std::fs::write(
                if changed == "source" {
                    &source
                } else {
                    &external
                },
                b"pub fn broken() { missing_agent_fixture_symbol(); }\n",
            )
            .unwrap();
            let edited = verdict();
            if changed == "source" && mode != "metadata" {
                // A measured source change must use the current source key, not
                // misclassify it as header drift under a silent watcher.
                assert!(
                    matches!(edited, CacheVerdict::SourceChanged { .. }),
                    "edited: {mode}/{changed}: {edited:?}"
                );
            } else {
                assert!(
                    !matches!(edited, CacheVerdict::Hit { .. }),
                    "edited: {mode}/{changed}: {edited:?}"
                );
            }
        }
    }
}
