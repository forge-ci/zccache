//! #2027: internal scans are warning-neutral and cannot refuse a compile.

use super::{scan_safe_args, UNUSED_ARGUMENT_SUPPRESSION};
use crate::compiler::CompilerFamily;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn scan_argv_ends_with_the_suppression_flag() {
    for family in [CompilerFamily::Clang, CompilerFamily::Gcc] {
        let args = scan_safe_args(family, &argv(&["-Werror", "-M", "-MF", "a.d", "a.cpp"]));
        assert_eq!(
            args.last().unwrap(),
            UNUSED_ARGUMENT_SUPPRESSION,
            "{family:?}"
        );
    }
}

#[test]
fn suppression_precedes_the_input_separator() {
    let args = scan_safe_args(
        CompilerFamily::Clang,
        &argv(&["-Werror", "-M", "--", "a.cpp", "-b.cpp"]),
    );
    assert_eq!(
        args,
        argv(&[
            "-Werror",
            "-M",
            UNUSED_ARGUMENT_SUPPRESSION,
            "--",
            "a.cpp",
            "-b.cpp"
        ])
    );
}

#[test]
fn suppression_follows_user_response_files() {
    let args = scan_safe_args(CompilerFamily::Clang, &argv(&["@user.rsp", "-M"]));
    assert_eq!(
        args,
        argv(&["@user.rsp", "-M", UNUSED_ARGUMENT_SUPPRESSION])
    );
}

#[test]
fn suppression_overrides_a_user_werror_for_the_argument() {
    let args = scan_safe_args(
        CompilerFamily::Clang,
        &argv(&["-Werror=unused-command-line-argument", "-M", "a.cpp"]),
    );
    let werror = args
        .iter()
        .position(|arg| arg == "-Werror=unused-command-line-argument")
        .unwrap();
    let suppression = args
        .iter()
        .position(|arg| arg == UNUSED_ARGUMENT_SUPPRESSION)
        .unwrap();
    assert!(werror < suppression, "{args:?}");
    assert_eq!(suppression, args.len() - 1);
}

#[test]
fn msvc_and_rust_scans_are_unchanged() {
    for family in [
        CompilerFamily::Msvc,
        CompilerFamily::Rustc,
        CompilerFamily::Rustfmt,
    ] {
        let input = argv(&["/E", "a.cpp"]);
        assert_eq!(scan_safe_args(family, &input), input, "{family:?}");
    }
}

/// Scan modules return plain errors; only their fail-open callers decide
/// what the client sees. A client-visible refusal or daemon error built
/// inside a scan module would let a helper fail a working compile again.
#[test]
fn scan_modules_cannot_construct_client_visible_refusals() {
    let modules = [
        (
            "handle_compile/pch_overlay_aliases.rs",
            include_str!("handle_compile/pch_overlay_aliases.rs"),
        ),
        (
            "handle_compile/pipeline/system_includes.rs",
            include_str!("handle_compile/pipeline/system_includes.rs"),
        ),
        ("compiler_spawn.rs", include_str!("compiler_spawn.rs")),
    ];
    for (name, source) in modules {
        // Production code only; a module's own tests may name the types.
        let production = source.split("#[cfg(test)]").next().unwrap();
        for banned in ["PchRefusal", "Response::Error", "CompileExecResult::Error"] {
            assert!(
                !production.contains(banned),
                "{name} constructs a client-visible `{banned}`; scan failures must fail open"
            );
        }
    }
}
