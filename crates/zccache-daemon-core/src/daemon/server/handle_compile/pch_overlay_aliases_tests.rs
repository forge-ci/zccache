//! Alias identity and scanner destination regressions.

use super::{coalesce_roots, strip_scan_outputs};

#[test]
fn existing_hardlinked_roots_share_one_snapshot_and_conflicts_refuse() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original");
    let alias = directory.path().join("alias");
    let first = directory.path().join("first-snapshot");
    let second = directory.path().join("second-snapshot");
    std::fs::write(&original, b"header").unwrap();
    std::fs::hard_link(&original, &alias).unwrap();
    for snapshot in [&first, &second] {
        std::fs::write(snapshot, b"header").unwrap();
        kernal_api::platform::fs::set_file_mtime(
            snapshot,
            kernal_api::platform::fs::FileTime::from_unix_time(1_700_000_000, 0),
        )
        .unwrap();
    }
    let roots = vec![
        serde_json::json!({"name": original, "external-contents": first}),
        serde_json::json!({"name": alias, "external-contents": second}),
    ];
    let mut unified = roots.clone();
    let mappings = coalesce_roots(&mut unified, directory.path()).unwrap();
    assert_eq!(mappings[0].1, mappings[1].1);
    assert_eq!(
        unified[0]["external-contents"],
        unified[1]["external-contents"]
    );
    kernal_api::platform::fs::set_file_mtime(
        &second,
        kernal_api::platform::fs::FileTime::from_unix_time(1_700_000_001, 0),
    )
    .unwrap();
    assert!(coalesce_roots(&mut roots.clone(), directory.path()).is_err());
    std::fs::write(&second, b"edited").unwrap();
    kernal_api::platform::fs::set_file_mtime(
        &second,
        kernal_api::platform::fs::FileTime::from_unix_time(1_700_000_000, 0),
    )
    .unwrap();
    assert!(coalesce_roots(&mut roots.clone(), directory.path()).is_err());
}

fn strip(args: &[&str]) -> Vec<String> {
    strip_scan_outputs(
        &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        false,
    )
    .unwrap()
}

#[test]
fn scan_never_reuses_user_output_or_dependency_destinations() {
    assert_eq!(
        strip(&[
            "-c",
            "-o",
            "object.o",
            "-MFuser.d",
            "-MT",
            "target",
            "-MQquoted",
            "-MJ",
            "commands.json",
            "-MD",
            "-MP",
            "-dependency-file",
            "cc1.d",
            "--output=other.o",
            "-serialize-diagnostics=diags",
            "-dumpdir",
            "dumps/",
            "-Xclang",
            "-verify-pch",
            "-save-temps=obj",
            "-ftime-trace=trace.json",
            "-std=c++17",
            "-include-pch",
            "base.pch",
            "tu.cpp",
        ]),
        ["-std=c++17", "-include-pch", "base.pch", "tu.cpp"]
    );
}

#[test]
fn forwarded_dependency_outputs_are_removed_with_their_values() {
    assert_eq!(
        strip(&[
            "-Wp,-MD,wp.d,-DKEEP=1",
            "-Wp,-MMD,wp2.d,-MFjoined.d",
            "-Xclang",
            "-dependency-file",
            "-Xclang",
            "clang.d",
            "-Xpreprocessor",
            "-MMD",
            "-Xpreprocessor",
            "preprocessor.d",
            "-Xclang",
            "-D",
            "-Xclang",
            "KEEP2=1",
            "tu.cpp",
        ]),
        [
            "-Wp,-DKEEP=1",
            "-Xclang",
            "-D",
            "-Xclang",
            "KEEP2=1",
            "tu.cpp"
        ]
    );
}

#[test]
fn option_values_and_dash_prefixed_sources_are_preserved() {
    assert_eq!(
        strip(&[
            "-D",
            "-MD",
            "-I",
            "-MFpath",
            "-objcmt-migrate-all",
            "--",
            "-o.cpp"
        ]),
        [
            "-D",
            "-MD",
            "-I",
            "-MFpath",
            "-objcmt-migrate-all",
            "--",
            "-o.cpp"
        ]
    );
}

#[test]
fn unsupported_forwarding_and_missing_values_are_refused() {
    for args in [
        vec!["-Xclang=-dependency-file"],
        vec!["-o"],
        vec!["-Xclang"],
    ] {
        assert!(strip_scan_outputs(
            &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            false
        )
        .is_err());
    }
}
