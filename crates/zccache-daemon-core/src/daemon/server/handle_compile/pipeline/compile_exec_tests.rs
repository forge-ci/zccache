use super::{
    finish_pre_hash, header_trace_is_supported, prepare_private_clang_header_trace,
    schedule_pre_hash, should_inject_header_trace, should_inject_header_trace_for_source,
    use_full_clang_depfile_for_cpp, use_private_clang_header_trace,
};
use crate::compiler::CompilerFamily;
use crate::daemon::server::dependency_policy::DependencyDiscoveryMode;
use crate::depgraph::UserDepFlags;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[tokio::test]
async fn exclusive_pre_hash_finishes_before_the_compiler_phase() {
    let (task, ready) = schedule_pre_hash(true, || 42_u32).await;

    assert!(
        task.is_none(),
        "exclusive hashing must not overlap the compiler"
    );
    assert_eq!(ready, Some(42));
}

#[tokio::test]
async fn shared_pre_hash_retains_compiler_overlap() {
    let (task, ready) = schedule_pre_hash(false, || 42_u32).await;

    assert!(
        ready.is_none(),
        "shared hashing must stay on the overlap path"
    );
    assert_eq!(finish_pre_hash(task, ready).await, Some(42));
}

#[test]
fn header_trace_is_limited_to_unmodified_gnu_diagnostic_streams() {
    assert!(header_trace_is_supported(
        CompilerFamily::Clang,
        &args(&["-c", "main.c", "-Iinclude"]),
    ));
    assert!(header_trace_is_supported(
        CompilerFamily::Gcc,
        &args(&["-c", "main.cc"]),
    ));
    assert!(!header_trace_is_supported(
        CompilerFamily::Msvc,
        &args(&["/c", "main.c"]),
    ));
    for incompatible in [
        "-H",
        "-Wp,-H",
        "-Wp,-DTRACE,-H,-UOLD",
        "-Xpreprocessor=-H",
        "-Xclang=-H",
        "-fshow-skipped-includes",
        "-header-include-file",
        "-sys-header-deps",
        "-Xclang=-header-include-file",
        "-Xclang=-sys-header-deps",
        "-dependency-dot",
        "-Xclang=-dependency-dot",
        "-Wp,-dependency-dot,custom.dot",
        "-Wp,-header-include-file,custom.headers,-sys-header-deps",
        "-fdiagnostics-format=json",
    ] {
        assert!(!header_trace_is_supported(
            CompilerFamily::Clang,
            &args(&["-c", "main.c", incompatible]),
        ));
    }
    assert!(!header_trace_is_supported(
        CompilerFamily::Gcc,
        &args(&["-c", "main.c", "-Xpreprocessor", "-H"]),
    ));
}

#[test]
fn user_depfiles_never_enable_private_header_trace() {
    let compile_args = args(&["-c", "main.c"]);
    assert!(should_inject_header_trace(
        DependencyDiscoveryMode::AllHeaders,
        CompilerFamily::Clang,
        &compile_args,
        &UserDepFlags::default(),
    ));
    for dep_flags in [
        UserDepFlags {
            has_md: true,
            has_mmd: true,
            ..Default::default()
        },
        UserDepFlags {
            has_md: true,
            has_mmd: true,
            mf_path: Some("custom.d".into()),
            ..Default::default()
        },
        UserDepFlags {
            depfile_to_stdout: true,
            ..Default::default()
        },
    ] {
        assert!(!should_inject_header_trace(
            DependencyDiscoveryMode::AllHeaders,
            CompilerFamily::Clang,
            &compile_args,
            &dep_flags,
        ));
    }
}

#[test]
fn private_clang_trace_is_bounded_to_plain_c_translation_units() {
    assert!(use_private_clang_header_trace(
        CompilerFamily::Clang,
        std::path::Path::new("main.c"),
        &args(&["-c", "main.c"]),
    ));
    assert!(!use_private_clang_header_trace(
        CompilerFamily::Clang,
        std::path::Path::new("main.cpp"),
        &args(&["-c", "main.cpp"]),
    ));
    assert!(!use_private_clang_header_trace(
        CompilerFamily::Clang,
        std::path::Path::new("main.C"),
        &args(&["-c", "main.C"]),
    ));
    assert!(!use_private_clang_header_trace(
        CompilerFamily::Clang,
        std::path::Path::new("main.cxx"),
        &args(&["-c", "main.cxx"]),
    ));
    assert!(!use_private_clang_header_trace(
        CompilerFamily::Clang,
        std::path::Path::new("main.c"),
        &args(&["-x", "c++", "-c", "main.c"]),
    ));
    assert!(!use_private_clang_header_trace(
        CompilerFamily::Gcc,
        std::path::Path::new("main.c"),
        &args(&["-c", "main.c"]),
    ));
    for incompatible in [
        "-Xclang=-header-include-file",
        "-Xclang=-sys-header-deps",
        "-Xclang=-dependency-dot",
        "-Wp,-dependency-dot,custom.dot",
        "-Wp,-header-include-file,custom.headers,-sys-header-deps",
        "--sysroot=/sdk",
        "-isysroot/sdk",
        "-Xclang=-isysroot",
        "-Xclang=-isysroot=/sdk",
        "-Xclang=--sysroot",
        "-Xclang=--sysroot=/sdk",
        "-Wp,-isysroot,/sdk",
        "-Wp,--sysroot,/sdk",
    ] {
        assert!(!use_private_clang_header_trace(
            CompilerFamily::Clang,
            std::path::Path::new("main.c"),
            &args(&["-c", "main.c", incompatible]),
        ));
    }
}

#[test]
fn plain_clang_cpp_uses_exact_full_depfile_without_header_trace() {
    let dep_flags = UserDepFlags::default();
    for source in ["main.cc", "main.cpp", "main.cxx", "main.c++", "main.C"] {
        assert!(use_full_clang_depfile_for_cpp(
            CompilerFamily::Clang,
            std::path::Path::new(source),
        ));
        assert!(!should_inject_header_trace_for_source(
            DependencyDiscoveryMode::AllHeaders,
            CompilerFamily::Clang,
            std::path::Path::new(source),
            &args(&["-c", source]),
            &dep_flags,
        ));
    }
    assert!(!use_full_clang_depfile_for_cpp(
        CompilerFamily::Clang,
        std::path::Path::new("main.c"),
    ));
    assert!(should_inject_header_trace_for_source(
        DependencyDiscoveryMode::AllHeaders,
        CompilerFamily::Clang,
        std::path::Path::new("main.c"),
        &args(&["-c", "main.c"]),
        &dep_flags,
    ));
    assert!(!use_full_clang_depfile_for_cpp(
        CompilerFamily::Gcc,
        std::path::Path::new("main.cpp"),
    ));
}

#[test]
fn private_clang_trace_does_not_duplicate_the_header_graph_in_mmd() {
    let temp = tempfile::TempDir::new().unwrap();
    let depfile = temp.path().join("main.d");
    let mut extra_args = args(&["-MMD", "-MF", depfile.to_str().unwrap()]);
    let mut strategy = crate::depgraph::DepfileStrategy::InjectedMmd {
        path: depfile.into(),
    };

    let trace = prepare_private_clang_header_trace(&mut extra_args, &mut strategy);

    assert_eq!(strategy, crate::depgraph::DepfileStrategy::CompilerTrace);
    assert!(!extra_args.iter().any(|arg| arg == "-MMD" || arg == "-MF"));
    assert!(extra_args.iter().any(|arg| arg == "-dependency-dot"));
    assert!(!extra_args.iter().any(|arg| arg == "-sys-header-deps"));
    assert_eq!(
        trace.path.extension(),
        Some(std::ffi::OsStr::new("headers"))
    );
}
