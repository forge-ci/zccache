//! Native archive context identity cannot use Cargo extern basename rules (#1945).
use super::make_rustc_context;
use std::path::Path;

#[test]
fn native_archive_paths_distinguish_contexts_and_metadata_aliases() {
    let mut first = make_rustc_context("/workspace/src/lib.rs", "2021");
    first.emit_types = vec!["metadata".into()];
    first.extern_crates = vec![(
        format!("{}0:static=foo", crate::NATIVE_INPUT_PREFIX),
        "/workspace/first/libfoo.a".into(),
    )];
    let mut second = first.clone();
    second.extern_crates[0].1 = "/workspace/second/libfoo.a".into();
    let root = Some(Path::new("/workspace"));
    assert_ne!(
        first.context_key_with_root(root),
        second.context_key_with_root(root)
    );
    let first_alias = first.check_metadata_compat_key_with_root(root);
    assert!(first_alias.is_some());
    assert_ne!(
        first_alias,
        second.check_metadata_compat_key_with_root(root)
    );
}

#[test]
fn native_archive_paths_keep_equivalent_worktree_keys_equal() {
    let mut first = make_rustc_context("/first/src/lib.rs", "2021");
    first.emit_types = vec!["metadata".into()];
    first.extern_crates = vec![(
        format!("{}0:static=foo", crate::NATIVE_INPUT_PREFIX),
        "/first/native/libfoo.a".into(),
    )];
    let mut second = first.clone();
    second.source_file = "/second/src/lib.rs".into();
    second.extern_crates[0].1 = "/second/native/libfoo.a".into();
    let root_a = Some(Path::new("/first"));
    let root_b = Some(Path::new("/second"));
    assert_eq!(
        first.context_key_with_root(root_a),
        second.context_key_with_root(root_b)
    );
    assert_eq!(
        first.check_metadata_compat_key_with_root(root_a),
        second.check_metadata_compat_key_with_root(root_b)
    );
}
