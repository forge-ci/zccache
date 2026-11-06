//! Actual rustc probes establish last-kind-wins, including a later implicit kind.
use super::*;

fn check_emit_options(options: &[&str], expected: &[&str]) {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let mut args = vec![
        "--crate-name=probe".into(),
        "--crate-type=lib".into(),
        format!("--out-dir={}", cwd.display()),
        "probe.rs".into(),
    ];
    args.extend(options.iter().map(|arg| (*arg).to_owned()));
    let crate::compiler::ParsedInvocation::Cacheable(compilation) =
        crate::compiler::parse_invocation("rustc", &args)
    else {
        panic!("expected cacheable")
    };
    let primary = if compilation.output_file.is_absolute() {
        compilation.output_file.to_path_buf()
    } else {
        cwd.join(&compilation.output_file)
    };
    let parsed = crate::depgraph::parse_rustc_args(&args, cwd);
    let mut expected: Vec<_> = expected
        .iter()
        .map(|name| NormalizedPath::new(cwd.join(name)))
        .collect();
    let expected_primary = expected[0].clone();
    std::fs::write(cwd.join("old.output"), b"stale old output").unwrap();
    for path in &expected {
        std::fs::write(path, b"actual output").unwrap();
    }
    let mut required = rustc_expected_output_paths(&parsed, &primary, cwd, None);
    let mut captured: Vec<_> = collect_rustc_output_files(&parsed, &primary, cwd)
        .into_iter()
        .map(|o| o.path)
        .collect();
    for paths in [&mut expected, &mut required, &mut captured] {
        paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    }
    assert_eq!(
        (NormalizedPath::new(primary), required, captured),
        (expected_primary, expected.clone(), expected)
    );
}

#[test]
fn repeated_metadata_path_does_not_require_or_capture_old_path() {
    check_emit_options(
        &["--emit=metadata=old.output,metadata=new.output"],
        &["new.output"],
    );
}
#[test]
fn repeated_link_path_does_not_require_or_capture_old_path() {
    check_emit_options(&["--emit=link=old.output,link=new.output"], &["new.output"]);
}
#[test]
fn later_implicit_emit_discards_old_explicit_path() {
    check_emit_options(
        &["--emit=metadata=old.output,metadata"],
        &["libprobe.rmeta"],
    );
}
#[test]
fn later_explicit_emit_replaces_implicit_destination() {
    check_emit_options(&["--emit=metadata,metadata=new.output"], &["new.output"]);
}
#[test]
fn repeated_emit_flags_share_last_kind_wins_policy() {
    check_emit_options(
        &[
            "--emit",
            "metadata=old.output",
            "--emit=metadata=new.output",
        ],
        &["new.output"],
    );
}

#[test]
fn distinct_kinds_across_flags_are_retained() {
    check_emit_options(
        &["--emit=metadata=kept.rmeta", "--emit=obj"],
        &["kept.rmeta", "probe.o"],
    );
}
#[test]
fn later_implicit_flag_discards_old_explicit_path() {
    check_emit_options(
        &["--emit=metadata=old.output", "--emit=metadata"],
        &["libprobe.rmeta"],
    );
}
#[test]
fn later_implicit_link_discards_old_explicit_path() {
    check_emit_options(&["--emit=link=old.output,link"], &["libprobe.rlib"]);
}
