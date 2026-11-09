//! Cache admission for library inputs whose selected bytes are not modeled (#1930).

use super::{
    super::{parse_invocation, ParsedInvocation},
    args,
};

#[test]
fn library_search_unresolved_inputs_are_not_cacheable() {
    for flags in [
        vec!["--sysroot", "/custom"],
        vec!["--sysroot=/custom"],
        vec!["--extern", "choice"],
        vec!["--extern=noprelude:choice"],
        vec!["-L", "crate=/libs"],
        vec!["-Lall=/libs"],
        vec!["-L/libs"],
        vec!["-L", "/libs"],
    ] {
        let mut invocation = vec!["--crate-type", "rlib", "lib.rs"];
        invocation.extend_from_slice(&flags);
        assert!(
            matches!(
                parse_invocation("rustc", &args(&invocation)),
                ParsedInvocation::NonCacheable { .. }
            ),
            "unmodeled inputs: {flags:?}"
        );
    }
}

#[test]
fn library_search_explicit_cargo_externs_still_cache() {
    let invocation = args(&[
        "--crate-type",
        "rlib",
        "lib.rs",
        "-Ldependency=/target/deps",
        "-Lnative=/build/out",
        "--extern",
        "choice=/target/deps/libchoice.rlib",
    ]);
    assert!(matches!(
        parse_invocation("rustc", &invocation),
        ParsedInvocation::Cacheable(_)
    ));
}
