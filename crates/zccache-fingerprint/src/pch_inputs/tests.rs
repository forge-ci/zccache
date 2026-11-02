//! Tests for clang PCH input-record parsing and content-verified replay.
//!
//! The fixtures build a real PCH with clang under FastLED's strict flags
//! (`-Werror=invalid-pch -verify-pch -fpch-validate-input-files-content`,
//! timestamps on), so the records under test are exactly what clang wrote.
//! They skip when `clang++` is not on PATH.

use super::*;
use kernal_api::platform::fs::{set_file_mtime, FileTime};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// `canonicalize` without Windows' `\\?\` verbatim prefix, which clang
/// never records.
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    let text = path.to_string_lossy();
    Ok(text
        .strip_prefix(r"\\?\")
        .map_or_else(|| path.clone(), PathBuf::from))
}

const OLD_SECS: i64 = 1_600_000_000;

fn clang() -> Option<&'static str> {
    Command::new("clang++")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| "clang++")
}

/// `clang()`, but only when it records xxh3 content hashes (clang >= 19);
/// older clang stores an `llvm::hash_code`, so content-verified replay is
/// unavailable and these tests do not apply.
fn xxh3_clang() -> Option<&'static str> {
    let clang = clang()?;
    let fixture = build_fixture(clang);
    let inputs = fixture.inputs();
    let record = record_for(&inputs, &fixture.pch_header());
    let bytes = fs::read(fixture.pch_header()).unwrap();
    if record.content_xxh3 == Some(content_xxh3(&bytes)) {
        Some(clang)
    } else {
        eprintln!("skipping: this clang does not record xxh3 input hashes");
        None
    }
}

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn root(&self) -> PathBuf {
        canonical(self.dir.path()).unwrap()
    }
    fn header(&self) -> PathBuf {
        self.root().join("inc").join("a.h")
    }
    fn pch_header(&self) -> PathBuf {
        self.root().join("pch.h")
    }
    fn pch(&self) -> PathBuf {
        self.root().join("pch.h.pch")
    }
    fn inputs(&self) -> Vec<PchInput> {
        read_pch_inputs(&self.pch()).unwrap().unwrap()
    }
    fn options(&self) -> (PathBuf, PathBuf) {
        (self.root(), self.pch_header())
    }
}

/// Build `pch.h` (which includes `inc/a.h`) into `pch.h.pch` with clang's
/// strict validation flags. Both headers get an old, fixed mtime first.
fn build_fixture(clang: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = canonical(dir.path()).unwrap();
    fs::create_dir_all(root.join("inc")).unwrap();
    fs::write(
        root.join("inc").join("a.h"),
        "#pragma once\nstruct A { int x; };\ntemplate <class T = int> struct B {};\n",
    )
    .unwrap();
    fs::write(root.join("pch.h"), "#pragma once\n#include \"a.h\"\n").unwrap();
    for name in ["pch.h", "inc/a.h"] {
        set_file_mtime(&root.join(name), FileTime::from_unix_time(OLD_SECS, 0)).unwrap();
    }
    let status = Command::new(clang)
        .current_dir(&root)
        .args([
            "-x",
            "c++-header",
            "-c",
            "pch.h",
            "-Iinc",
            "-o",
            "pch.h.pch",
            "-Wno-unused-command-line-argument",
            "-Werror=invalid-pch",
            "-verify-pch",
            "-fpch-validate-input-files-content",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "clang failed to build the PCH fixture");
    Fixture { dir }
}

fn record_for<'a>(inputs: &'a [PchInput], path: &Path) -> &'a PchInput {
    inputs
        .iter()
        .find(|input| Path::new(&input.path) == path)
        .unwrap_or_else(|| panic!("no record for {}: {inputs:#?}", path.display()))
}

fn plan(fixture: &Fixture, replay_enabled: bool) -> Result<PchServePlan, PchRefusal> {
    let (root, header) = fixture.options();
    plan_pch_serve(
        &fixture.inputs(),
        PchServeOptions {
            replay_root: &root,
            expected_top_level: Some(&header),
            replay_enabled,
        },
    )
}

fn mtime_secs_of(path: &Path) -> i64 {
    FileTime::from_last_modification_time(&fs::metadata(path).unwrap()).unix_seconds()
}

#[test]
fn records_equal_what_clang_stats_and_hashes() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    let inputs = fixture.inputs();
    for path in [fixture.pch_header(), fixture.header()] {
        let record = record_for(&inputs, &path);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(record.size, bytes.len() as u64, "{}", path.display());
        assert_eq!(record.mtime_secs, OLD_SECS as u64, "{}", path.display());
        assert_eq!(record.content_xxh3, Some(content_xxh3(&bytes)));
        assert!(!record.overridden && !record.transient);
    }
    assert!(record_for(&inputs, &fixture.pch_header()).top_level);
    assert!(!record_for(&inputs, &fixture.header()).top_level);
}

/// Cross-check against `llvm-bcanalyzer -dump`, clang's own reader, when it
/// is installed: every `INPUT_FILE` it prints has the same size, mtime and
/// name as the record parsed here.
#[test]
fn records_match_llvm_bcanalyzer() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let Ok(out) = Command::new("llvm-bcanalyzer")
        .arg("-dump")
        .arg(fixture.pch())
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let dump = String::from_utf8_lossy(&out.stdout);
    let mut expected = Vec::new();
    for line in dump.lines().filter(|line| line.contains("<INPUT_FILE ")) {
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(name))
                .map(|value| value.trim_end_matches("/>").parse::<u64>().unwrap())
                .unwrap()
        };
        let name = line.split("blob data = '").nth(1).unwrap();
        let name = name.strip_suffix('\'').unwrap();
        expected.push((field("op1="), field("op2="), name.to_owned()));
    }
    let parsed: Vec<_> = fixture
        .inputs()
        .into_iter()
        .map(|input| (input.size, input.mtime_secs, input.as_requested))
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(parsed, expected);
}

#[test]
fn non_pch_bytes_are_not_parsed() {
    assert!(parse_pch_inputs(b"gpch.013 not clang").unwrap().is_none());
    assert!(parse_pch_inputs(b"").is_err());
}

#[test]
fn truncated_pch_reports_truncated() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let bytes = fs::read(fixture.pch()).unwrap();
    assert!(matches!(
        parse_pch_inputs(&bytes[..64]),
        Err(PchInputsError::Truncated)
    ));
}

#[test]
fn untouched_inputs_serve_without_replay() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let plan = plan(&fixture, true).unwrap();
    assert!(plan.replays.is_empty());
    assert!(plan.inputs_checked >= 2);
}

#[test]
fn touched_identical_header_uses_private_overlay_for_strict_consumer() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    set_file_mtime(&fixture.header(), FileTime::now()).unwrap();
    let touched = FileTime::from_last_modification_time(&fs::metadata(fixture.header()).unwrap());

    let planned = plan(&fixture, true).unwrap();
    assert_eq!(planned.replays.len(), 1);
    assert_eq!(
        planned.replays[0].path,
        NormalizedPath::new(fixture.header())
    );
    assert_eq!(planned.replays[0].recorded_secs, OLD_SECS as u64);

    let private = tempfile::tempdir().unwrap();
    let overlay = write_pch_overlay(&planned, private.path()).unwrap();
    fs::write(
        fixture.root().join("consumer.cpp"),
        "#include \"a.h\"\nB<> b;\nA a;\n",
    )
    .unwrap();
    let output = Command::new(clang)
        .current_dir(fixture.root())
        .args([
            "-c",
            "consumer.cpp",
            "-Iinc",
            "-include-pch",
            "pch.h.pch",
            "-Werror=invalid-pch",
            "-fpch-validate-input-files-content",
            "-MD",
            "-MF",
            "consumer.d",
            "-o",
            "consumer.o",
            "-ivfsoverlay",
        ])
        .arg(overlay)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        FileTime::from_last_modification_time(&fs::metadata(fixture.header()).unwrap()),
        touched
    );
    assert_eq!(plan(&fixture, true).unwrap().replays.len(), 1);
    let dependencies = fs::read_to_string(fixture.root().join("consumer.d")).unwrap();
    assert!(dependencies.contains("a.h"), "{dependencies}");
    assert!(!dependencies.contains("input-0"), "{dependencies}");
}

#[test]
fn replay_must_preserve_source_nanosecond_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let header = dir.path().join("source.h");
    fs::write(&header, b"#pragma once\n").unwrap();
    let touched = FileTime::from_unix_time(OLD_SECS + 100, 123_456_789);
    set_file_mtime(&header, touched).unwrap();
    let touched = FileTime::from_last_modification_time(&fs::metadata(&header).unwrap());
    let plan = PchServePlan {
        inputs_checked: 1,
        replays: vec![PendingReplay {
            path: NormalizedPath::new(&header),
            recorded_secs: OLD_SECS as u64,
            current_secs: touched.unix_seconds(),
            size: 13,
            content_xxh3: Some(content_xxh3(b"#pragma once\n")),
        }],
    };
    let private = tempfile::tempdir().unwrap();
    write_pch_overlay(&plan, private.path()).unwrap();
    assert_eq!(
        FileTime::from_last_modification_time(&fs::metadata(&header).unwrap()),
        touched
    );
}

#[test]
fn all_changed_inputs_must_be_refused_even_with_recorded_mtimes() {
    let directory = tempfile::tempdir().unwrap();
    let root = canonical(directory.path()).unwrap();
    let mut inputs = Vec::new();
    for name in ["a.h", "b.h"] {
        let path = root.join(name);
        let original = b"int before;\n";
        fs::write(&path, b"int after_;\n").unwrap();
        set_file_mtime(&path, FileTime::from_unix_time(OLD_SECS, 0)).unwrap();
        inputs.push(PchInput {
            path: path.to_string_lossy().into_owned(),
            as_requested: name.to_owned(),
            size: original.len() as u64,
            mtime_secs: OLD_SECS as u64,
            content_xxh3: Some(content_xxh3(original)),
            overridden: false,
            transient: false,
            top_level: false,
            imported_pch: false,
        });
    }
    let refusal = plan_pch_serve(
        &inputs,
        PchServeOptions {
            replay_root: &root,
            expected_top_level: None,
            replay_enabled: true,
        },
    )
    .unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ContentMismatch);
}

#[test]
fn touched_header_is_refused_when_replay_is_disabled() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    let touched = FileTime::now();
    set_file_mtime(&fixture.header(), touched).unwrap();
    let refusal = plan(&fixture, false).unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ReplayDisabled);
    assert_eq!(mtime_secs_of(&fixture.header()), touched.unix_seconds());
}

#[test]
fn same_size_content_edit_is_refused() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    let original = fs::read_to_string(fixture.header()).unwrap();
    let edited = original.replace("int x;", "int y;");
    assert_eq!(edited.len(), original.len());
    fs::write(fixture.header(), edited).unwrap();
    // Even with the recorded mtime restored, the content check refuses.
    set_file_mtime(&fixture.header(), FileTime::from_unix_time(OLD_SECS, 0)).unwrap();
    let refusal = plan(&fixture, true).unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ContentMismatch);
}

#[test]
fn size_change_is_refused_before_hashing() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let mut text = fs::read_to_string(fixture.header()).unwrap();
    text.push_str("// grown\n");
    fs::write(fixture.header(), &text).unwrap();
    let refusal = plan(&fixture, true).unwrap_err();
    assert!(matches!(
        refusal.reason,
        PchRefusalReason::SizeMismatch { .. }
    ));
    assert_eq!(Path::new(&refusal.path), fixture.header());
}

#[test]
fn missing_input_is_refused() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    fs::remove_file(fixture.header()).unwrap();
    assert_eq!(
        plan(&fixture, true).unwrap_err().reason,
        PchRefusalReason::Missing
    );
}

#[test]
fn input_outside_replay_root_is_never_touched() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    let touched = FileTime::now();
    set_file_mtime(&fixture.header(), touched).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let refusal = plan_pch_serve(
        &fixture.inputs(),
        PchServeOptions {
            replay_root: elsewhere.path(),
            expected_top_level: None,
            replay_enabled: true,
        },
    )
    .unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::OutsideReplayRoot);
    assert_eq!(mtime_secs_of(&fixture.header()), touched.unix_seconds());
}

/// A PCH built in checkout A, offered to the same header in checkout B: its
/// recorded inputs point at A, so it is refused and A is left untouched.
#[test]
fn pch_from_another_checkout_is_refused() {
    let Some(clang) = clang() else { return };
    let checkout_a = build_fixture(clang);
    let checkout_b = build_fixture(clang);
    let header_b = checkout_b.pch_header();
    let root_b = checkout_b.root();
    let refusal = plan_pch_serve(
        &checkout_a.inputs(),
        PchServeOptions {
            replay_root: &root_b,
            expected_top_level: Some(&header_b),
            replay_enabled: true,
        },
    )
    .unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::TopLevelMismatch);
}

#[test]
fn change_between_plan_and_snapshot_is_refused_without_mtime_mutation() {
    let Some(clang) = xxh3_clang() else { return };
    let fixture = build_fixture(clang);
    set_file_mtime(&fixture.header(), FileTime::now()).unwrap();
    let planned = plan(&fixture, true).unwrap();
    fs::write(fixture.header(), "#pragma once\nstruct A { long x; };\n").unwrap();
    let changed_time =
        FileTime::from_last_modification_time(&fs::metadata(fixture.header()).unwrap());
    let private = tempfile::tempdir().unwrap();
    let refusal = write_pch_overlay(&planned, private.path()).unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ChangedDuringReplay);
    assert_eq!(
        FileTime::from_last_modification_time(&fs::metadata(fixture.header()).unwrap()),
        changed_time
    );
}

#[test]
fn replay_env_defaults_on() {
    assert!(pch_mtime_replay_enabled(None));
    assert!(pch_mtime_replay_enabled(Some("1")));
    assert!(pch_mtime_replay_enabled(Some("on")));
    for off in ["0", "false", "OFF", "no", " 0 "] {
        assert!(!pch_mtime_replay_enabled(Some(off)), "{off}");
    }
}

/// Chained PCH: `top.h.pch` built with `-include-pch pch.h.pch` records the
/// base PCH's size and mtime (`IMPORT`). A base PCH re-materialized with a
/// new mtime makes clang reject the chain ("module file has a different size
/// or modification time"); replaying the recorded mtime repairs it.
#[test]
fn chained_pch_base_uses_private_mtime_without_mutating_original() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let root = fixture.root();
    fs::write(
        root.join("top.h"),
        "#pragma once\n#include \"a.h\"\nstruct C { A a; };\n",
    )
    .unwrap();
    fs::write(
        root.join("use.cpp"),
        "#include \"a.h\"\n#include \"top.h\"\nB<> b;\nint main() { C c{}; return c.a.x; }\n",
    )
    .unwrap();
    let flags = [
        "-Iinc",
        "-Wno-unused-command-line-argument",
        "-Werror=invalid-pch",
        "-fpch-validate-input-files-content",
    ];
    let run = |args: &[&str]| {
        let out = Command::new(clang)
            .current_dir(&root)
            .args(args)
            .args(flags)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let built = run(&[
        "-x",
        "c++-header",
        "-c",
        "top.h",
        "-include-pch",
        "pch.h.pch",
        "-o",
        "top.h.pch",
        "-verify-pch",
    ]);
    assert!(built.0, "{}", built.1);
    let consume = || run(&["-c", "use.cpp", "-include-pch", "top.h.pch", "-o", "use.o"]);
    assert!(consume().0, "chain should load before any touch");

    let mut inputs = match read_pch_inputs(&root.join("top.h.pch")) {
        Ok(inputs) => inputs.unwrap(),
        // An IMPORTS layout this reader does not know (some vendor clang):
        // the chained PCH is refused, which is a miss, never a wrong serve.
        Err(PchInputsError::Malformed(reason)) if reason.contains("IMPORTS") => {
            eprintln!("skipping: unsupported IMPORTS layout ({reason})");
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let base = record_for(&inputs, &fixture.pch());
    assert!(base.imported_pch);
    assert_eq!(base.size, fs::metadata(fixture.pch()).unwrap().len());

    let recorded = base.mtime_secs;
    let expected_base_hash = content_xxh3(&fs::read(fixture.pch()).unwrap());
    inputs
        .iter_mut()
        .find(|input| input.imported_pch)
        .unwrap()
        .content_xxh3 = Some(expected_base_hash);
    set_file_mtime(
        &fixture.pch(),
        FileTime::from_unix_time(recorded as i64 + 100, 0),
    )
    .unwrap();
    let (ok, stderr) = consume();
    // clang >= 21: "different size or modification time"; older: "out of date".
    assert!(!ok && stderr.contains("pch.h.pch"), "{stderr}");

    let planned = plan_pch_serve(
        &inputs,
        PchServeOptions {
            replay_root: &root,
            expected_top_level: Some(&root.join("top.h")),
            replay_enabled: true,
        },
    )
    .unwrap();
    assert_eq!(planned.replays.len(), 1);
    assert_eq!(planned.replays[0].path, NormalizedPath::new(fixture.pch()));
    let original_time =
        FileTime::from_last_modification_time(&fs::metadata(fixture.pch()).unwrap());
    let private = tempfile::tempdir().unwrap();
    let overlay = write_pch_overlay(&planned, private.path()).unwrap();
    let overlay = overlay.to_str().unwrap();
    let (ok, stderr) = run(&[
        "-c",
        "use.cpp",
        "-include-pch",
        "top.h.pch",
        "-o",
        "use.o",
        "-ivfsoverlay",
        overlay,
    ]);
    assert!(ok, "overlay chain should load: {stderr}");
    assert_eq!(
        FileTime::from_last_modification_time(&fs::metadata(fixture.pch()).unwrap()),
        original_time
    );
}

/// A PCH whose hashes are not xxh3 (clang < 19 records an `llvm::hash_code`):
/// untouched inputs still serve on clang's own size + mtime check, but a
/// touched input cannot be verified, so it is refused and never replayed.
#[test]
fn legacy_hashes_never_replay() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    let legacy: Vec<PchInput> = fixture
        .inputs()
        .into_iter()
        .map(|mut input| {
            input.content_xxh3 = None;
            input
        })
        .collect();
    let (root, header) = fixture.options();
    let options = PchServeOptions {
        replay_root: &root,
        expected_top_level: Some(&header),
        replay_enabled: true,
    };
    assert!(plan_pch_serve(&legacy, options).unwrap().replays.is_empty());
    let touched = FileTime::now();
    set_file_mtime(&fixture.header(), touched).unwrap();
    assert_eq!(
        plan_pch_serve(&legacy, options).unwrap_err().reason,
        PchRefusalReason::MtimeUnverifiable
    );
    assert_eq!(mtime_secs_of(&fixture.header()), touched.unix_seconds());
}

#[test]
fn stat_match_detects_drift_in_either_direction() {
    let Some(clang) = clang() else { return };
    let fixture = build_fixture(clang);
    assert!(inputs_stat_match(&fixture.inputs()));
    set_file_mtime(&fixture.header(), FileTime::now()).unwrap();
    assert!(!inputs_stat_match(&fixture.inputs()));
    set_file_mtime(
        &fixture.header(),
        FileTime::from_unix_time(OLD_SECS - 60, 0),
    )
    .unwrap();
    assert!(!inputs_stat_match(&fixture.inputs()));
    set_file_mtime(&fixture.header(), FileTime::from_unix_time(OLD_SECS, 0)).unwrap();
    assert!(inputs_stat_match(&fixture.inputs()));
    fs::remove_file(fixture.header()).unwrap();
    assert!(!inputs_stat_match(&fixture.inputs()));
}
