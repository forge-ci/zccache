//! Perf Guard: a warm hit must borrow the graph's already-normalized inputs.

use super::*;
use crate::scanner::ScanResult;
use crate::search_paths::IncludeSearchPaths;
use std::cell::Cell;

#[test]
fn perf_fast_hit_borrows_normalized_inputs_without_rebuilding_paths() {
    let graph = DepGraph::new();
    let source = NormalizedPath::new("/workspace/main.cpp");
    let header = NormalizedPath::new("/workspace/include/shared.h");
    let forced = NormalizedPath::new("/workspace/include/forced.h");
    let inputs = [&source, &header, &forced];
    let key = graph.register(CompileContext {
        source_file: source.clone(),
        include_search: IncludeSearchPaths::default(),
        defines: vec![],
        undefines: vec![],
        flags: vec![],
        force_includes: vec![forced.clone()],
        unknown_flags: vec![],
        compiler_hash: zccache_hash::hash_bytes(b"compiler"),
    });
    let hash = zccache_hash::hash_bytes(b"contents");
    let artifact = graph
        .update(
            &key,
            ScanResult {
                resolved: vec![header.clone()],
                unresolved: vec![],
                has_computed: false,
            },
            |_| Some(hash),
        )
        .unwrap();
    let expected = artifact.hash().to_hex();
    let calls = Cell::new(0);
    for _ in 0..50 {
        let hit = graph.try_fast_hit_for_artifact_with_normalized_inputs(
            &key,
            &expected,
            |path: &NormalizedPath| {
                let original = inputs.iter().find(|input| **input == path).unwrap();
                assert!(
                    std::ptr::eq(path.as_path(), original.as_path()),
                    "warm input validation must retain the interned path, not normalize it again"
                );
                calls.set(calls.get() + 1);
                Some(hash)
            },
            |_| None,
        );
        assert_eq!(hit.as_ref(), Some(&artifact));
    }
    assert_eq!(calls.get(), 50 * inputs.len());

    // Borrowing must preserve every input check and the artifact identity check.
    for changed in inputs {
        assert!(graph
            .try_fast_hit_for_artifact_with_normalized_inputs(
                &key,
                &expected,
                |path| (path != changed).then_some(hash),
                |_| None,
            )
            .is_none());
    }
    assert!(graph
        .try_fast_hit_for_artifact_with_normalized_inputs(
            &key,
            "different-artifact",
            |_| panic!("refuse a mismatched artifact before checking inputs"),
            |_| None,
        )
        .is_none());
}
