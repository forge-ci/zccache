//! Real-compiler input-selection regressions for #1930.

use super::TestHarness;

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
