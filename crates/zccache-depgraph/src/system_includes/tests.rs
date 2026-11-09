use super::*;

// ── Issue #1530: MSVC %INCLUDE% parsing ─────────────────────────────

#[test]
fn msvc_include_env_splits_on_semicolons() {
    assert_eq!(
        parse_msvc_include_env("C:\\VC\\include;C:\\Kits\\10\\ucrt"),
        vec![
            NormalizedPath::new("C:\\VC\\include"),
            NormalizedPath::new("C:\\Kits\\10\\ucrt"),
        ]
    );
}

#[test]
fn msvc_include_env_drops_empty_and_trims_segments() {
    // vcvars emits a trailing `;`, and hand-edited values pick up spaces.
    assert_eq!(
        parse_msvc_include_env(";C:\\a ; ;  C:\\b with spaces\\inc ;"),
        vec![
            NormalizedPath::new("C:\\a"),
            NormalizedPath::new("C:\\b with spaces\\inc"),
        ]
    );
    assert!(parse_msvc_include_env("").is_empty());
    assert!(parse_msvc_include_env(";;  ;").is_empty());
}

#[test]
fn msvc_include_env_lookup_is_case_insensitive() {
    let block = vec![("Include".to_string(), "C:\\a".to_string())];
    assert_eq!(
        msvc_system_includes_from_env(&block),
        vec![NormalizedPath::new("C:\\a")]
    );
}

#[test]
fn msvc_include_env_lookup_takes_the_last_duplicate() {
    let block = vec![
        ("INCLUDE".to_string(), "C:\\stale".to_string()),
        ("include".to_string(), "C:\\live".to_string()),
    ];
    assert_eq!(
        msvc_system_includes_from_env(&block),
        vec![NormalizedPath::new("C:\\live")]
    );
}

#[test]
fn msvc_include_env_absent_is_empty() {
    let block = vec![("PATH".to_string(), "C:\\bin".to_string())];
    assert!(msvc_system_includes_from_env(&block).is_empty());
    assert!(msvc_system_includes_from_env(&[]).is_empty());
}

#[test]
fn parse_gcc_output() {
    let output = r#"Using built-in specs.
COLLECT_GCC=g++
COLLECT_LTO_WRAPPER=/usr/libexec/gcc/x86_64-linux-gnu/12/lto-wrapper
#include "..." search starts here:
#include <...> search starts here:
 /usr/include/c++/12
 /usr/include/x86_64-linux-gnu/c++/12
 /usr/include/c++/12/backward
 /usr/lib/gcc/x86_64-linux-gnu/12/include
 /usr/local/include
 /usr/include/x86_64-linux-gnu
 /usr/include
End of search list.
"#;
    let paths = parse_system_include_output(output);
    assert_eq!(paths.len(), 7);
    assert_eq!(paths[0], NormalizedPath::new("/usr/include/c++/12"));
}

fn touch(path: &Path, content: &[u8]) {
    std::fs::write(path, content).unwrap();
}

#[test]
fn cache_get_returns_paths_after_insert() {
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    touch(&compiler, b"#!/bin/sh\nexec /usr/bin/clang++ \"$@\"\n");

    let mut cache = SystemIncludeCache::new();
    cache.insert(NormalizedPath::new(&compiler), vec!["/usr/include".into()]);

    let got = cache.get(&compiler).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0], NormalizedPath::new("/usr/include"));
}

#[test]
fn cache_get_invalidates_on_binary_change() {
    // Issue #541: stat-verify must catch in-place compiler upgrades
    // (apt upgrade clang, brew upgrade, etc.) so the cached entry
    // doesn't outlive the binary that produced it.
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    touch(&compiler, b"#!/bin/sh\n# v1\n");
    kernal_api::platform::fs::set_file_mtime(
        &compiler,
        kernal_api::platform::fs::FileTime::from_unix_time(1_000_000_000, 0),
    )
    .unwrap();

    let mut cache = SystemIncludeCache::new();
    cache.insert(NormalizedPath::new(&compiler), vec!["/old/include".into()]);
    assert!(cache.get(&compiler).is_some(), "fresh insert must verify");

    // Simulate `apt upgrade clang` — same path, new binary.
    touch(
        &compiler,
        b"#!/bin/sh\n# v2 with a totally different size\n",
    );
    kernal_api::platform::fs::set_file_mtime(
        &compiler,
        kernal_api::platform::fs::FileTime::from_unix_time(1_000_001_000, 0),
    )
    .unwrap();

    assert!(
        cache.get(&compiler).is_none(),
        "binary change must invalidate the cached entry",
    );
}

#[test]
fn cache_get_or_discover_caches() {
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    touch(&compiler, b"#!/bin/sh\n# v1\n");

    let mut cache = SystemIncludeCache::new();
    let discover_calls = std::cell::RefCell::new(0);

    let paths = cache.get_or_discover(&compiler, |_| {
        *discover_calls.borrow_mut() += 1;
        vec!["/usr/include".into()]
    });
    assert_eq!(paths.len(), 1);

    // Second call must NOT re-run discovery (entry is still fresh).
    let _ = cache.get_or_discover(&compiler, |_| {
        *discover_calls.borrow_mut() += 1;
        vec!["/should/not/happen".into()]
    });
    assert_eq!(*discover_calls.borrow(), 1);
}

#[test]
fn cache_save_then_load_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    let snapshot = tmp.path().join("system_includes.bin");
    touch(&compiler, b"#!/bin/sh\nexec /usr/bin/clang++\n");

    let mut original = SystemIncludeCache::new();
    original.insert(
        NormalizedPath::new(&compiler),
        vec!["/usr/include".into(), "/usr/local/include".into()],
    );
    original.save_to_disk(&snapshot).unwrap();
    assert!(snapshot.exists(), "non-empty cache must produce a file");

    let restored = SystemIncludeCache::load_from_disk(&snapshot).unwrap();
    let got = restored.get(&compiler).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0], NormalizedPath::new("/usr/include"));
    assert_eq!(got[1], NormalizedPath::new("/usr/local/include"));
}

#[test]
fn cache_load_missing_file_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.bin");
    let cache = SystemIncludeCache::load_from_disk(&missing).unwrap();
    assert_eq!(cache.len(), 0);
}

#[test]
fn cache_save_empty_does_not_create_file() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot = tmp.path().join("system_includes.bin");
    let cache = SystemIncludeCache::new();
    cache.save_to_disk(&snapshot).unwrap();
    assert!(!snapshot.exists());
}

#[test]
fn empty_discovery_entries_are_never_reused_or_persisted() {
    // Issue #1167: snapshots from older releases may contain an empty
    // result. It is degraded probe state, not a reusable assertion that
    // the compiler has no system includes.
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    let snapshot = tmp.path().join("system_includes.bin");
    touch(&compiler, b"#!/bin/sh\n# compiler\n");
    let metadata = std::fs::metadata(&compiler).unwrap();
    let mut cache = SystemIncludeCache::new();
    cache.cache.insert(
        NormalizedPath::new(&compiler),
        SystemIncludeEntry {
            mtime: metadata.modified().unwrap(),
            size: metadata.len(),
            paths: Vec::new(),
            known_empty: false,
        },
    );

    assert!(cache.get(&compiler).is_none());
    cache.save_to_disk(&snapshot).unwrap();
    assert!(
        !snapshot.exists(),
        "empty discoveries must not be written back to disk"
    );
}

#[test]
fn cache_load_rejects_version_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot = tmp.path().join("system_includes.bin");

    let bad = PersistedSystemIncludes {
        version: FORMAT_VERSION + 999,
        entries: Vec::new(),
    };
    let bytes = bincode::serialize(&bad).unwrap();
    std::fs::write(&snapshot, bytes).unwrap();

    let err = SystemIncludeCache::load_from_disk(&snapshot).unwrap_err();
    assert!(err.to_string().contains("version mismatch"));
}

#[test]
fn cache_load_then_get_rehashes_when_binary_changes_after_save() {
    // Round-trip safety net: a stale on-disk snapshot must never
    // substitute for a real discovery after the compiler changed.
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang++");
    let snapshot = tmp.path().join("system_includes.bin");
    touch(&compiler, b"#!/bin/sh\n# original\n");
    kernal_api::platform::fs::set_file_mtime(
        &compiler,
        kernal_api::platform::fs::FileTime::from_unix_time(1_000_000_000, 0),
    )
    .unwrap();

    let mut original = SystemIncludeCache::new();
    original.insert(NormalizedPath::new(&compiler), vec!["/old/include".into()]);
    original.save_to_disk(&snapshot).unwrap();

    // Simulate compiler upgrade.
    touch(&compiler, b"#!/bin/sh\n# upgraded - different size\n");
    kernal_api::platform::fs::set_file_mtime(
        &compiler,
        kernal_api::platform::fs::FileTime::from_unix_time(1_000_001_000, 0),
    )
    .unwrap();

    let restored = SystemIncludeCache::load_from_disk(&snapshot).unwrap();
    assert!(
        restored.get(&compiler).is_none(),
        "stale snapshot must not survive a binary change",
    );
}

#[test]
fn discovery_args_returns_nonempty() {
    assert!(!discovery_args().is_empty());
}

#[test]
fn c_discovery_args_select_the_c_frontend() {
    let args = discovery_args_c();
    assert_eq!(
        args.windows(2).find(|pair| pair[0] == "-x"),
        Some(&["-x", "c"][..])
    );
}

#[test]
fn discovery_args_fast_contains_triple_hash() {
    // Sanity: the fast path uses `-###` so clang prints the cc1
    // command without spawning the real preprocessor.
    assert!(discovery_args_fast().contains(&"-###"));
    assert!(discovery_args_fast().contains(&"-E"));
    assert!(discovery_args_fast().contains(&"-x"));
    assert!(discovery_args_fast().contains(&"c++"));
}

#[test]
fn parse_cc1_output_extracts_internal_isystem_paths() {
    // Realistic clang 18 -### output on Ubuntu 24.04. The cc1 line
    // is at the bottom, all args quoted with double quotes.
    let output = r#"clang version 18.1.3 (1ubuntu1)
Target: x86_64-pc-linux-gnu
Thread model: posix
InstalledDir: /usr/bin
Found candidate GCC installation: /usr/lib/gcc/x86_64-linux-gnu/13
Selected GCC installation: /usr/lib/gcc/x86_64-linux-gnu/13
Candidate multilib: .;@m64
Selected multilib: .;@m64
 "/usr/lib/llvm-18/bin/clang-18" "-cc1" "-triple" "x86_64-pc-linux-gnu" "-E" "-disable-free" "-disable-llvm-verifier" "-main-file-name" "null" "-mrelocation-model" "pic" "-pic-level" "2" "-mframe-pointer=all" "-fmath-errno" "-ffp-contract=on" "-fno-rounding-math" "-mconstructor-aliases" "-funwind-tables=2" "-target-cpu" "x86-64" "-debugger-tuning=gdb" "-fcoverage-compilation-dir=/tmp" "-resource-dir" "/usr/lib/llvm-18/lib/clang/18" "-internal-isystem" "/usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13" "-internal-isystem" "/usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/x86_64-linux-gnu/c++/13" "-internal-isystem" "/usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13/backward" "-internal-isystem" "/usr/lib/llvm-18/lib/clang/18/include" "-internal-isystem" "/usr/local/include" "-internal-externc-isystem" "/usr/include/x86_64-linux-gnu" "-internal-externc-isystem" "/include" "-internal-externc-isystem" "/usr/include" "-fdeprecated-macro" "-ferror-limit" "19" "-fgnuc-version=4.2.1" "-fcxx-exceptions" "-fexceptions" "-fcolor-diagnostics" "-target-feature" "+cx8" "-target-feature" "+fxsr" "-target-feature" "+mmx" "-target-feature" "+sse" "-target-feature" "+sse2" "-target-feature" "+x87" "-faddrsig" "-D__GCC_HAVE_DWARF2_CFI_ASM=1" "-o" "-" "-x" "c++" "/dev/null"
"#;

    let paths = parse_cc1_system_include_output(output);
    assert_eq!(
        paths.len(),
        8,
        "expected 5 isystem + 3 externc-isystem entries, got {paths:?}",
    );
    // First captures must include the GCC C++ headers (driver
    // canonical-order):
    assert_eq!(
        paths[0],
        NormalizedPath::new("/usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13"),
    );
    // Last must be /usr/include (the catch-all):
    assert_eq!(paths[7], NormalizedPath::new("/usr/include"));
}

#[test]
fn parse_cc1_output_empty_when_no_cc1_line() {
    // Output without a cc1 line (e.g. gcc -### or a compiler that
    // doesn't support `-###`) must yield an empty path list so the
    // caller can fall back to the slow `-v -E` discovery.
    let output = "gcc version 13.2.0 (Ubuntu 13.2.0-23ubuntu4)\nCOLLECT_GCC_OPTIONS='-### -E -x c++' '-mtune=generic' '-march=x86-64'\n";
    let paths = parse_cc1_system_include_output(output);
    assert!(paths.is_empty(), "expected empty, got {paths:?}");
}

#[test]
fn parse_cc1_output_handles_bare_unquoted_args() {
    // Issue #544: Linux Ubuntu clang-18 emits `-###` output with
    // only the binary path quoted; individual args are bare. The
    // parser previously matched only `"-cc1"` (quoted) and missed
    // this entire format, falling back to the slow `-v -E` probe.
    // Sample lifted from /usr/bin/clang-18 -### -E -x c++ /dev/null
    // on Ubuntu 24.04 (the local performance runner image).
    let output = r#"clang version 18.1.3 (1ubuntu1)
Target: x86_64-pc-linux-gnu
Thread model: posix
InstalledDir: /usr/bin
 "/usr/lib/llvm-18/bin/clang-18" -cc1 -triple x86_64-pc-linux-gnu -E -disable-free -main-file-name null -mrelocation-model pic -pic-level 2 -mframe-pointer=all -fmath-errno -ffp-contract=on -fno-rounding-math -mconstructor-aliases -funwind-tables=2 -target-cpu x86-64 -debugger-tuning=gdb -resource-dir /usr/lib/llvm-18/lib/clang/18 -internal-isystem /usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13 -internal-isystem /usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/x86_64-linux-gnu/c++/13 -internal-isystem /usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13/backward -internal-isystem /usr/lib/llvm-18/lib/clang/18/include -internal-isystem /usr/local/include -internal-externc-isystem /usr/include/x86_64-linux-gnu -internal-externc-isystem /include -internal-externc-isystem /usr/include -fdeprecated-macro -fcolor-diagnostics -o - -x c++ /dev/null
"#;

    let paths = parse_cc1_system_include_output(output);
    assert_eq!(
        paths.len(),
        8,
        "expected 5 isystem + 3 externc-isystem from bare-arg output, got {paths:?}",
    );
    assert_eq!(
        paths[0],
        NormalizedPath::new("/usr/bin/../lib/gcc/x86_64-linux-gnu/13/../../../../include/c++/13"),
    );
    assert_eq!(paths[7], NormalizedPath::new("/usr/include"));
}

#[test]
fn parse_cc1_output_handles_paths_with_spaces() {
    // Driver quoting preserves spaces inside paths — the tokenizer
    // must read the whole quoted segment as one token.
    let output =
            "\"/usr/bin/clang\" \"-cc1\" \"-E\" \"-internal-isystem\" \"/opt/Custom Include Path/v1\" \"-x\" \"c++\" \"/dev/null\"\n";
    let paths = parse_cc1_system_include_output(output);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], NormalizedPath::new("/opt/Custom Include Path/v1"));
}

#[test]
fn parse_cc1_output_resolves_emscripten_iwithsysroot_in_search_order() {
    let output = r#" "/emsdk/upstream/bin/clang-20" "-cc1" "-isysroot" "/emsdk/upstream/emscripten/cache/sysroot" "-internal-isystem" "/emsdk/upstream/emscripten/cache/sysroot/include/c++/v1" "-internal-isystem" "/emsdk/upstream/lib/clang/20/include" "-iwithsysroot/include/fakesdl" "-iwithsysroot/include/compat" "-x" "c++" "/dev/null"
"#;

    let paths = parse_cc1_system_include_output(output);

    assert_eq!(
        paths,
        vec![
            NormalizedPath::new("/emsdk/upstream/emscripten/cache/sysroot/include/fakesdl"),
            NormalizedPath::new("/emsdk/upstream/emscripten/cache/sysroot/include/compat"),
            NormalizedPath::new("/emsdk/upstream/emscripten/cache/sysroot/include/c++/v1"),
            NormalizedPath::new("/emsdk/upstream/lib/clang/20/include"),
        ]
    );
}

#[test]
fn parse_cc1_output_skips_isystem_when_value_missing() {
    // Truncated cc1 line — the -internal-isystem flag with no value
    // following must not panic and must not produce an entry.
    let output = "\"/usr/bin/clang\" \"-cc1\" \"-internal-isystem\"\n";
    let paths = parse_cc1_system_include_output(output);
    assert!(paths.is_empty());
}

// zccache#1775: dirty-flag tracking lets a shutdown save skip the
// system-includes cache snapshot when nothing has changed.
mod dirty_flag {
    use super::*;

    #[test]
    fn starts_clean() {
        let mut cache = SystemIncludeCache::new();
        assert!(!cache.is_dirty());
        assert!(!cache.take_dirty());
    }

    #[test]
    fn insert_marks_dirty_until_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let compiler = tmp.path().join("cc");
        std::fs::write(&compiler, b"fake compiler").unwrap();
        let mut cache = SystemIncludeCache::new();

        cache.insert(
            NormalizedPath::new(&compiler),
            vec![NormalizedPath::new(tmp.path())],
        );
        assert!(cache.is_dirty());
        assert!(cache.take_dirty());
        assert!(!cache.is_dirty());
    }

    #[test]
    fn insert_with_empty_paths_does_not_mark_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let compiler = tmp.path().join("cc");
        std::fs::write(&compiler, b"fake compiler").unwrap();
        let mut cache = SystemIncludeCache::new();

        cache.insert(NormalizedPath::new(&compiler), Vec::new());
        assert!(
            !cache.is_dirty(),
            "an empty result is deliberately not cached"
        );
    }

    #[test]
    fn merge_from_does_not_mark_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let compiler = tmp.path().join("cc");
        std::fs::write(&compiler, b"fake compiler").unwrap();
        let mut loaded = SystemIncludeCache::new();
        loaded.insert(
            NormalizedPath::new(&compiler),
            vec![NormalizedPath::new(tmp.path())],
        );

        let mut live = SystemIncludeCache::new();
        live.merge_from(loaded);

        assert!(
            !live.is_dirty(),
            "a background disk load must not require an immediate re-save"
        );
    }
}

#[test]
fn complete_empty_search_list_is_distinct_from_degraded_output() {
    assert_eq!(
        parse_system_include_output_checked(
            "#include <...> search starts here:\nEnd of search list.\n"
        ),
        Some(Vec::new())
    );
    for output in [
        "",
        "frontend missing",
        "End of search list.\n",
        "#include <...> search starts here:\n",
        "#include <...> search starts here:\n /sdk\n",
    ] {
        assert_eq!(parse_system_include_output_checked(output), None);
    }
}

#[test]
fn confirmed_empty_roots_survive_disk_reload_and_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("gcc");
    touch(&compiler, b"compiler");
    let snapshot = tmp.path().join("system_includes.bin");
    let mut cache = SystemIncludeCache::new();
    cache.insert_known_empty(NormalizedPath::new(&compiler));
    assert!(cache.is_dirty());
    assert_eq!(cache.get(&compiler), Some([].as_slice()));
    cache.save_to_disk(&snapshot).unwrap();
    let restored = SystemIncludeCache::load_from_disk(&snapshot).unwrap();
    let mut merged = SystemIncludeCache::new();
    merged.merge_from(restored);
    assert_eq!(merged.get(&compiler), Some([].as_slice()));
    merged.get_or_discover(&compiler, |_| panic!("confirmed empty must not re-probe"));
    touch(&compiler, b"different compiler binary");
    assert!(merged.get(&compiler).is_none());
}

#[test]
fn legacy_empty_snapshot_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshot = tmp.path().join("system_includes.bin");
    let legacy = PersistedSystemIncludes {
        version: 1,
        entries: Vec::new(),
    };
    std::fs::write(&snapshot, bincode::serialize(&legacy).unwrap()).unwrap();
    assert!(SystemIncludeCache::load_from_disk(&snapshot).is_err());
}
