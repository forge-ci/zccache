//! Real-compiler input-selection regressions for #1930.

use super::TestHarness;

fn direct_rustc(h: &TestHarness, args: &[&str]) -> zccache_test_support::test_child::TestOutput {
    let mut command = std::process::Command::new(h.rc());
    command.current_dir(h.tmp.path()).args(args);
    zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap()
}

fn write_old(h: &TestHarness, name: &str, text: &str) {
    let path = h.write_file(name, text);
    age(path.as_path());
}

fn age(path: &std::path::Path) {
    use kernal_api::platform::fs::{set_file_mtime, FileTime};
    set_file_mtime(path, FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
}

async fn library(h: &mut TestHarness, directory: &str, value: u32) {
    let source = format!("dependency_{value}.rs");
    write_old(h, &source, &format!("pub const VALUE: u32 = {value};"));
    std::fs::create_dir_all(h.path(directory)).unwrap();
    let output = format!("{directory}/libchoice.rlib");
    let (exit, _) = h
        .compile_args(
            &[
                "--crate-name",
                "choice",
                "--crate-type",
                "rlib",
                &source,
                "-o",
                &output,
            ],
            None,
        )
        .await;
    assert_eq!(exit, 0);
    use kernal_api::platform::fs::{set_file_mtime, FileTime};
    set_file_mtime(
        h.path(&output).as_path(),
        FileTime::from_unix_time(1_600_000_000 + i64::from(value), 0),
    )
    .unwrap();
}

async fn consumer(h: &mut TestHarness, flags: &[&str]) -> (i32, bool, Vec<u8>) {
    let mut args = vec![
        "--crate-name",
        "consumer",
        "--crate-type",
        "rlib",
        "consumer.rs",
        "-o",
        "libconsumer.rlib",
    ];
    args.extend_from_slice(flags);
    let (exit, cached) = h.compile_args(&args, None).await;
    let bytes = std::fs::read(h.path("libconsumer.rlib")).unwrap_or_default();
    (exit, cached, bytes)
}

#[tokio::test]
async fn implicit_extern_same_path_replacement() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(
        &h,
        "consumer.rs",
        "extern crate choice; pub const VALUE: u32 = choice::VALUE;",
    );
    library(&mut h, "libs", 17).await;
    let flags = ["--extern", "choice", "-L", "crate=libs"];
    let (exit, _, first) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    library(&mut h, "libs", 91).await;
    let (exit, cached, second) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    assert!(!cached, "a replaced implicit extern must not hit");
    assert_ne!(
        first, second,
        "the new library constant must reach the artifact"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn explicit_extern_content_and_search_path_reuse() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(
        &h,
        "consumer.rs",
        "extern crate choice; pub const VALUE: u32 = choice::VALUE;",
    );
    library(&mut h, "one", 17).await;
    library(&mut h, "two", 17).await;
    let (exit, _, first) = consumer(
        &mut h,
        &["--extern=choice=one/libchoice.rlib", "-Ldependency=one"],
    )
    .await;
    assert_eq!(exit, 0);
    let (exit, cached, second) = consumer(
        &mut h,
        &["--extern=choice=two/libchoice.rlib", "-Ldependency=two"],
    )
    .await;
    assert_eq!(exit, 0);
    assert!(
        cached,
        "equivalent explicit extern bytes must reuse across target directories"
    );
    assert_eq!(first, second);
    library(&mut h, "two", 91).await;
    let (exit, cached, third) = consumer(
        &mut h,
        &["--extern=choice=two/libchoice.rlib", "-Ldependency=two"],
    )
    .await;
    assert_eq!(exit, 0);
    assert!(!cached);
    assert_ne!(second, third);
    h.shutdown().await;
}

#[tokio::test]
async fn library_search_kind_and_order() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(
        &h,
        "consumer.rs",
        "extern crate choice; pub const VALUE: u32 = choice::VALUE;",
    );
    library(&mut h, "one", 17).await;
    library(&mut h, "two", 91).await;
    let (exit, _, first) = consumer(&mut h, &["-Lcrate=one", "-Ldependency=two"]).await;
    assert_eq!(exit, 0);
    let (exit, cached, second) = consumer(&mut h, &["-Ldependency=one", "-Lcrate=two"]).await;
    assert_eq!(exit, 0);
    assert!(!cached, "search kind must not reuse the previous artifact");
    assert_ne!(first, second);
    let (exit, cached, _) = consumer(&mut h, &["-Lnative=two", "-Ldependency=one"]).await;
    assert_ne!(
        exit, 0,
        "neither native nor dependency admits a direct extern"
    );
    assert!(!cached);
    // Rustc rejects distinct matching crates, regardless of directory order.
    for paths in [
        ["-Lcrate=one", "-Lcrate=two"],
        ["-Lcrate=two", "-Lcrate=one"],
    ] {
        let (exit, cached, _) = consumer(&mut h, &paths).await;
        assert_ne!(exit, 0, "ambiguous search must preserve rustc's error");
        assert!(!cached);
    }
    h.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn custom_sysroot_same_path_library_replacement() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(&h, "consumer.rs", "#![no_std]\npub const VALUE: u32 = 17;");
    let mut command = std::process::Command::new(h.rc());
    command.args(["--print", "target-libdir"]);
    let output = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(output.code, Some(0));
    let target_lib =
        zccache::core::NormalizedPath::new(String::from_utf8(output.stdout).unwrap().trim());
    let target = target_lib.parent().unwrap().file_name().unwrap();
    let custom_lib = h
        .path("sysroot")
        .join("lib/rustlib")
        .join(target)
        .join("lib");
    std::fs::create_dir_all(&custom_lib).unwrap();
    let mut core = Vec::new();
    for entry in std::fs::read_dir(target_lib).unwrap() {
        let entry = entry.unwrap();
        let destination = custom_lib.join(entry.file_name());
        kernal_api::platform::fs::symlink_file(&entry.path(), destination.as_path()).unwrap();
        if entry.file_name().to_string_lossy().starts_with("libcore-")
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "rlib" || ext == "rmeta")
        {
            core.push(destination);
        }
    }
    let (exit, _, _) = consumer(&mut h, &["--sysroot", "sysroot"]).await;
    assert_eq!(exit, 0);
    // Replace only the fixture symlink, never the installed toolchain library.
    assert!(!core.is_empty(), "libcore metadata/archive");
    for path in core {
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"invalid replacement core library").unwrap();
        age(path.as_path());
    }
    let mut command = std::process::Command::new(h.rc());
    command.current_dir(h.tmp.path()).args([
        "--crate-name",
        "consumer",
        "--crate-type",
        "rlib",
        "consumer.rs",
        "--sysroot",
        "sysroot",
        "-o",
        "control.rlib",
    ]);
    let control = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_ne!(
        control.code,
        Some(0),
        "control must reject replaced core metadata/archive"
    );
    let (exit, cached, _) = consumer(&mut h, &["--sysroot", "sysroot"]).await;
    assert_ne!(
        exit, 0,
        "a broken custom libcore must fail, not replay a good artifact"
    );
    assert!(!cached);
    h.shutdown().await;
}

#[tokio::test]
async fn transitive_dependency_missing_preserves_compiler_error() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    library(&mut h, "deps", 17).await;
    write_old(
        &h,
        "umbrella.rs",
        "extern crate choice; pub fn value() -> u32 { choice::VALUE }",
    );
    let (exit, _) = h
        .compile_args(
            &[
                "--crate-name",
                "umbrella",
                "--crate-type",
                "rlib",
                "umbrella.rs",
                "--extern=choice=deps/libchoice.rlib",
                "-o",
                "libumbrella.rlib",
            ],
            None,
        )
        .await;
    assert_eq!(exit, 0);
    age(h.path("libumbrella.rlib").as_path());
    write_old(
        &h,
        "consumer.rs",
        "extern crate umbrella; pub fn value() -> u32 { umbrella::value() }",
    );
    let flags = ["--extern=umbrella=libumbrella.rlib", "-Ldependency=deps"];
    let (exit, _, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    let (exit, cached, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    assert!(cached, "unchanged transitive libraries should hit");
    std::fs::create_dir_all(h.path("other")).unwrap();
    for name in ["libchoice.rlib", "libumbrella.rlib"] {
        let source = if name == "libchoice.rlib" {
            "deps/libchoice.rlib"
        } else {
            "libumbrella.rlib"
        };
        let destination = h.path(&format!("other/{name}"));
        std::fs::copy(h.path(source), &destination).unwrap();
        age(destination.as_path());
    }
    let (exit, reused, _) = consumer(
        &mut h,
        &[
            "--extern=umbrella=other/libumbrella.rlib",
            "-Ldependency=other",
        ],
    )
    .await;
    assert_eq!(exit, 0);
    assert!(
        reused,
        "the transitive closure must reuse identical bytes in another target directory"
    );
    std::fs::remove_file(h.path("deps/libchoice.rlib")).unwrap();
    let mut command = std::process::Command::new(h.rc());
    command.current_dir(h.tmp.path()).args([
        "--crate-name",
        "consumer",
        "--crate-type",
        "rlib",
        "consumer.rs",
        "--extern=umbrella=libumbrella.rlib",
        "-Ldependency=deps",
        "-o",
        "control.rlib",
    ]);
    let control = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_ne!(
        control.code,
        Some(0),
        "uncached compiler must reject the missing transitive crate"
    );
    let (exit, cached, _) = consumer(&mut h, &flags).await;
    assert_ne!(
        exit, 0,
        "a missing implicit dependency must preserve rustc's error"
    );
    assert!(!cached);
    h.shutdown().await;
}

#[tokio::test]
async fn unused_alias_does_not_hide_transitive_lookup_errors() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    library(&mut h, "deps", 17).await;
    std::fs::create_dir_all(h.path("other")).unwrap();
    std::fs::copy(
        h.path("deps/libchoice.rlib"),
        h.path("other/libchoice.rlib"),
    )
    .unwrap();
    age(h.path("other/libchoice.rlib").as_path());
    write_old(
        &h,
        "umbrella.rs",
        "extern crate choice; pub fn value() -> u32 { choice::VALUE }",
    );
    let (exit, _) = h
        .compile_args(
            &[
                "--crate-name",
                "umbrella",
                "--crate-type",
                "rlib",
                "umbrella.rs",
                "--extern=choice=deps/libchoice.rlib",
                "-o",
                "libumbrella.rlib",
            ],
            None,
        )
        .await;
    assert_eq!(exit, 0);
    age(h.path("libumbrella.rlib").as_path());
    write_old(
        &h,
        "consumer.rs",
        "extern crate umbrella; pub fn value() -> u32 { umbrella::value() }",
    );
    let flags = [
        "--extern=umbrella=libumbrella.rlib",
        "--extern=unused_alias=other/libchoice.rlib",
        "-Ldependency=deps",
    ];
    let (exit, _, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    std::fs::remove_file(h.path("deps/libchoice.rlib")).unwrap();
    let mut args = vec![
        "--crate-name",
        "consumer",
        "--crate-type",
        "rlib",
        "consumer.rs",
        "-o",
        "control.rlib",
    ];
    args.extend_from_slice(&flags);
    let control = direct_rustc(&h, &args);
    eprintln!(
        "unused alias direct control: {:?}: {}",
        control.code,
        control.stderr_text()
    );
    let (exit, cached, _) = consumer(&mut h, &flags).await;
    assert_eq!(
        Some(exit),
        control.code,
        "an unused alias must not hide a compiler error (cached={cached})"
    );
    if exit != 0 {
        assert!(!cached);
    }
    h.shutdown().await;
}

#[tokio::test]
async fn metadata_only_root_is_not_a_linkable_extern() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    std::fs::create_dir_all(h.path("deps")).unwrap();
    write_old(&h, "choice.rs", "pub fn value() -> u32 { 17 }");
    let (exit, _) = h
        .compile_args(
            &[
                "--crate-name",
                "choice",
                "--crate-type",
                "rlib",
                "--emit=metadata=deps/libchoice.rmeta,link=deps/libchoice.rlib",
                "choice.rs",
            ],
            None,
        )
        .await;
    assert_eq!(exit, 0);
    age(h.path("deps/libchoice.rmeta").as_path());
    write_old(
        &h,
        "bin.rs",
        "extern crate choice; fn main() { println!(\"{}\", choice::value()); }",
    );
    let args = [
        "--crate-name",
        "metadata_root",
        "--crate-type",
        "bin",
        "bin.rs",
        "--extern=choice=deps/libchoice.rmeta",
        "-Ldependency=deps",
        "-o",
        "metadata_root",
    ];
    let control = direct_rustc(&h, &args);
    eprintln!(
        "metadata-only root direct control: {:?}: {}",
        control.code,
        control.stderr_text()
    );
    assert_ne!(
        control.code,
        Some(0),
        "pinned rustc must require a codegen-capable explicit root"
    );
    assert!(
        control.stderr_text().contains("rlib"),
        "unexpected control error: {}",
        control.stderr_text()
    );
    let (exit, cached) = h.compile_args(&args, None).await;
    assert_eq!(Some(exit), control.code);
    assert!(!cached);
    h.shutdown().await;
}

#[cfg(unix)]
fn native_object(h: &TestHarness, value: u32) {
    write_old(
        h,
        "native.c",
        &format!("unsigned answer(void) {{ return {value}; }}"),
    );
    let mut command = std::process::Command::new("cc");
    command
        .current_dir(h.tmp.path())
        .args(["-c", "native.c", "-o", "native.o"]);
    let result = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(result.code, Some(0), "{}", result.stderr_text());
}

#[cfg(unix)]
fn native_archive(h: &TestHarness, value: u32, directory: &str) {
    native_object(h, value);
    std::fs::create_dir_all(h.path(directory)).unwrap();
    // cc-rs leaves relocatable objects beside the archive. They must not
    // disable unchanged-build hits or be mistaken for shared-library input.
    let object = std::fs::read(h.path("native.o")).unwrap();
    let object_path = h.path(&format!("{directory}/native.o"));
    std::fs::write(&object_path, object).unwrap();
    let output = format!("{directory}/libchoice.a");
    let mut command = std::process::Command::new("ar");
    command
        .current_dir(h.tmp.path())
        .args(["crs", output.as_str(), "native.o"]);
    let result = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(result.code, Some(0), "{}", result.stderr_text());
    use kernal_api::platform::fs::{set_file_mtime, FileTime};
    set_file_mtime(
        object_path.as_path(),
        FileTime::from_unix_time(1_600_000_000 + i64::from(value), 0),
    )
    .unwrap();
    set_file_mtime(
        h.path(&output).as_path(),
        FileTime::from_unix_time(1_600_000_000 + i64::from(value), 0),
    )
    .unwrap();
}

#[cfg(unix)]
fn linked_native_value(h: &TestHarness, library: &str) -> u32 {
    write_old(
        h,
        "runner.rs",
        "extern crate consumer; fn main() { println!(\"{}\", consumer::value()); }",
    );
    let executable = h.path("runner");
    let mut compile = std::process::Command::new(h.rc());
    compile
        .current_dir(h.tmp.path())
        .args([
            "--crate-name",
            "runner",
            "runner.rs",
            &format!("--extern=consumer={library}"),
            "-o",
        ])
        .arg(executable.as_path());
    let compiled = zccache_test_support::test_child::TestChild::spawn(compile)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(compiled.code, Some(0), "{}", compiled.stderr_text());
    let executed = zccache_test_support::test_child::TestChild::spawn(std::process::Command::new(
        executable.as_path(),
    ))
    .unwrap()
    .finish(std::time::Duration::from_secs(30))
    .unwrap();
    assert_eq!(executed.code, Some(0));
    executed.stdout_text().trim().parse().unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn source_link_attribute_tracks_bundled_archive() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(&h, "consumer.rs", "#[link(name=\"choice\", kind=\"static\")] extern \"C\" { fn answer() -> u32; } pub fn value() -> u32 { unsafe { answer() } }");
    native_archive(&h, 17, "native");
    let (exit, _, first) = consumer(&mut h, &["-Lnative=native"]).await;
    assert_eq!(exit, 0);
    assert_eq!(linked_native_value(&h, "libconsumer.rlib"), 17);
    let (exit, cached, _) = consumer(&mut h, &["-Lnative=native"]).await;
    assert_eq!(exit, 0);
    assert!(cached, "unchanged source-declared archive must hit");
    native_archive(&h, 91, "native");
    let mut command = std::process::Command::new(h.rc());
    command.current_dir(h.tmp.path()).args([
        "--crate-name",
        "consumer",
        "--crate-type",
        "rlib",
        "consumer.rs",
        "-Lnative=native",
        "-o",
        "control.rlib",
    ]);
    let control = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(control.code, Some(0));
    let expected = std::fs::read(h.path("control.rlib")).unwrap();
    assert_ne!(
        first, expected,
        "uncached compiler must embed the changed archive"
    );
    let (exit, cached, second) = consumer(&mut h, &["-Lnative=native"]).await;
    assert_eq!(exit, 0);
    assert!(!cached, "source #[link] archive replacement must miss");
    assert_ne!(first, second);
    assert_eq!(linked_native_value(&h, "libconsumer.rlib"), 91);
    h.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn source_link_attribute_tracks_new_earlier_archive() {
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(&h, "consumer.rs", "#[link(name=\"choice\", kind=\"static\")] extern \"C\" { fn answer() -> u32; } pub fn value() -> u32 { unsafe { answer() } }");
    std::fs::create_dir_all(h.path("early")).unwrap();
    native_archive(&h, 17, "native");
    let flags = ["-Lnative=early", "-Lnative=native"];
    let (exit, _, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    assert_eq!(linked_native_value(&h, "libconsumer.rlib"), 17);
    let (exit, cached, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    assert!(cached);
    native_archive(&h, 91, "early");
    let (exit, cached, _) = consumer(&mut h, &flags).await;
    assert_eq!(exit, 0);
    assert!(
        !cached,
        "a new earlier candidate must be resolved before request hits"
    );
    assert_eq!(linked_native_value(&h, "libconsumer.rlib"), 91);
    h.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn implicit_shared_library_preserves_linker_error() {
    if !kernal_api::platform::host::target_is_linux() {
        return;
    }
    for script in [false, true] {
        let mut h = TestHarness::new().await.expect("real rustc required");
        write_old(&h, "shared.rs", "#[link(name=\"choice\", kind=\"dylib\")] extern \"C\" { fn answer() -> u32; } fn main() { unsafe { answer(); } }");
        std::fs::create_dir_all(h.path("native")).unwrap();
        std::fs::create_dir_all(h.path("external")).unwrap();
        let output = if script {
            "external/libchoice.so"
        } else {
            "native/libchoice.so"
        };
        if script {
            write_old(
                &h,
                "native/libchoice.so",
                &format!("INPUT ( {} )\n", h.path(output).display()),
            );
        }
        let args = [
            "--crate-name",
            "shared",
            "--crate-type",
            "bin",
            "shared.rs",
            "-Lnative=native",
            "-o",
            "shared",
        ];
        for (iteration, symbol) in ["answer", "removed_answer"].into_iter().enumerate() {
            write_old(
                &h,
                "shared.c",
                &format!("unsigned {symbol}(void) {{ return 17; }}"),
            );
            let mut command = std::process::Command::new("cc");
            command
                .current_dir(h.tmp.path())
                .args(["-shared", "-fPIC", "shared.c", "-o", output]);
            let built = zccache_test_support::test_child::TestChild::spawn(command)
                .unwrap()
                .finish(std::time::Duration::from_secs(30))
                .unwrap();
            assert_eq!(built.code, Some(0), "{}", built.stderr_text());
            if iteration == 0 {
                let (exit, _) = h.compile_args(&args, None).await;
                assert_eq!(exit, 0);
            } else {
                let control = direct_rustc(&h, &args);
                assert_ne!(
                    control.code,
                    Some(0),
                    "direct linker must reject the missing symbol"
                );
                let (exit, cached) = h.compile_args(&args, None).await;
                assert_ne!(
                    exit, 0,
                    "dylib/script replacement must preserve the link error (script={script})"
                );
                assert!(!cached);
            }
        }
        h.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn thin_archive_member_changes_do_not_replay_old_binary() {
    if !kernal_api::platform::host::target_is_linux() {
        return;
    }
    let mut h = TestHarness::new().await.expect("real rustc required");
    write_old(&h, "thin.rs", "#[link(name=\"choice\", kind=\"static\")] extern \"C\" { fn answer() -> u32; } fn main() { println!(\"{}\", unsafe { answer() }); }");
    native_object(&h, 17);
    std::fs::create_dir_all(h.path("native")).unwrap();
    let mut command = std::process::Command::new("ar");
    command
        .current_dir(h.tmp.path())
        .args(["crsT", "native/libchoice.a", "native.o"]);
    let built = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(built.code, Some(0), "{}", built.stderr_text());
    let archive = std::fs::read(h.path("native/libchoice.a")).unwrap();
    let args = [
        "--crate-name",
        "thin",
        "--crate-type",
        "bin",
        "thin.rs",
        "-Lnative=native",
        "-o",
        "thin",
    ];
    let (exit, _) = h.compile_args(&args, None).await;
    assert_eq!(exit, 0);
    native_object(&h, 91);
    assert_eq!(
        archive,
        std::fs::read(h.path("native/libchoice.a")).unwrap(),
        "only the indirect member changed"
    );
    let control = direct_rustc(
        &h,
        &[
            "--crate-name",
            "thin",
            "--crate-type",
            "bin",
            "thin.rs",
            "-Lnative=native",
            "-o",
            "thin_control",
        ],
    );
    assert_eq!(control.code, Some(0), "{}", control.stderr_text());
    let run = zccache_test_support::test_child::TestChild::spawn(std::process::Command::new(
        h.path("thin_control").as_path(),
    ))
    .unwrap()
    .finish(std::time::Duration::from_secs(30))
    .unwrap();
    assert_eq!(run.stdout_text().trim(), "91");
    let (exit, cached) = h.compile_args(&args, None).await;
    assert_eq!(exit, 0);
    assert!(
        !cached,
        "an untracked thin member must not replay a cached binary"
    );
    let run = zccache_test_support::test_child::TestChild::spawn(std::process::Command::new(
        h.path("thin").as_path(),
    ))
    .unwrap()
    .finish(std::time::Duration::from_secs(30))
    .unwrap();
    assert_eq!(run.stdout_text().trim(), "91");
    h.shutdown().await;
}
