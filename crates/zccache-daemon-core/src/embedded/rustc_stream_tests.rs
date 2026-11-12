//! #1983: cargo must receive usable metadata while code generation continues.
use super::*;
use std::time::{Duration, Instant};
use tempfile::TempDir;

async fn start(temp: &TempDir) -> ZccacheService {
    let mut audit = AuditConfig::default();
    audit.mode = crate::audit::AuditMode::Off;
    ZccacheService::start(ZccacheConfig {
        host: HostIdentity::default_for_product("rustc-stream-test"),
        cache_root: temp.path().join("cache").into(),
        audit,
        limits: ServiceLimits::default(),
        runtime: RuntimeHooks::default(),
        cancellation: None,
    })
    .await
    .expect("start")
}

fn request(temp: &TempDir, crate_type: &str) -> CompileRequest {
    let compiler = crate::test_support::find_rustc().expect("real rustc required");
    CompileRequest {
        audit: AuditContext::new(
            crate::audit::AuditId::new("stream-run").unwrap(),
            crate::audit::AuditId::new("stream-trace").unwrap(),
        ),
        compiler,
        args: vec![
            "--crate-name=streamcrate".into(),
            format!("--crate-type={crate_type}"),
            "--emit=metadata,link,dep-info".into(),
            "--error-format=json".into(),
            "--json=artifacts".into(),
            "-Copt-level=2".into(),
            "--out-dir".into(),
            temp.path().join("out").to_string_lossy().into_owned(),
            temp.path()
                .join("streamcrate.rs")
                .to_string_lossy()
                .into_owned(),
        ],
        cwd: temp.path().into(),
        env: Vec::new(),
        stdin: Vec::new(),
    }
}

fn write_source(temp: &TempDir) {
    // Exported functions keep real LLVM codegen busy after metadata is complete.
    // Ordering is the regression assertion; the timeout only detects a hang.
    let source = (0..1500)
        .map(|i| format!("#[no_mangle] pub fn f{i}(mut x: u64) -> u64 {{ for _ in 0..100 {{ x = x.wrapping_mul({}).rotate_left(3); }} x }}\n", i * 2 + 1))
        .collect::<String>();
    let warnings = (0..700)
        .map(|i| format!("fn unused{i}() {{}}\n"))
        .collect::<String>();
    std::fs::write(temp.path().join("streamcrate.rs"), warnings + &source).unwrap();
    std::fs::create_dir(temp.path().join("out")).unwrap();
}

fn metadata_line(bytes: &[u8], scanned: &mut usize) -> Option<std::path::PathBuf> {
    let unread = &bytes[*scanned..];
    let end = unread.iter().rposition(|byte| *byte == b'\n')? + 1;
    *scanned += end;
    unread[..end].split(|byte| *byte == b'\n').find_map(|line| {
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        (value.get("emit")?.as_str()? == "metadata").then(|| {
            value
                .get("artifact")?
                .as_str()
                .map(std::path::PathBuf::from)
        })?
    })
}

fn contains_staged_library(root: &std::path::Path, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    std::fs::read_dir(root).unwrap().any(|entry| {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        (kind.is_file() && entry.file_name() == "libstreamcrate.rlib")
            || (kind.is_dir() && contains_staged_library(&entry.path(), depth - 1))
    })
}

#[tokio::test]
async fn perf_rustc_metadata_precedes_exit_and_hit_replays_identically() {
    let temp = TempDir::new().unwrap();
    write_source(&temp);
    let service = start(&temp).await;
    let request = request(&temp, "rlib");
    let metadata = temp.path().join("out/libstreamcrate.rmeta");
    let library = temp.path().join("out/libstreamcrate.rlib");
    let mut stderr = Vec::new();
    let mut scanned = 0;
    let mut delivered = None;
    let mut early_bytes = None;
    let mut outcome = None;
    tokio::time::timeout(
        Duration::from_secs(180),
        service.compile_streaming(request.clone(), |chunk| match chunk {
            CompileChunk::Stderr(bytes) => {
                stderr.extend(bytes);
                if delivered.is_none() {
                    if let Some(path) = metadata_line(&stderr, &mut scanned) {
                        assert_eq!(path, metadata, "live JSON must name the requested path");
                        early_bytes = Some(
                            std::fs::read(path).expect("complete metadata before notification"),
                        );
                        assert!(
                            !library.exists(),
                            "metadata must precede final link delivery"
                        );
                        delivered = Some(Instant::now());
                        assert!(
                            !contains_staged_library(&temp.path().join("cache"), 8),
                            "metadata must precede rustc creating its private linked artifact"
                        );
                    }
                }
            }
            CompileChunk::Done {
                exit_code,
                cache_outcome,
                ..
            } => {
                assert_eq!(exit_code, 0);
                outcome = Some(cache_outcome);
            }
            CompileChunk::Stdout(_) => {}
        }),
    )
    .await
    .expect("compile hang detector")
    .expect("miss");
    assert_eq!(outcome, Some(CacheOutcome::Miss));
    let margin = delivered.expect("metadata notification").elapsed();
    assert!(
        margin >= Duration::from_millis(10),
        "metadata must precede exit by a codegen margin: {margin:?}"
    );
    let metadata_bytes = std::fs::read(&metadata).unwrap();
    assert_eq!(early_bytes.unwrap(), metadata_bytes);
    let library_bytes = std::fs::read(&library).unwrap();
    std::fs::remove_file(&metadata).unwrap();
    std::fs::remove_file(&library).unwrap();
    let hit = service.compile(request).await.expect("hit");
    assert_eq!(hit.cache_outcome, CacheOutcome::Hit);
    assert_eq!(hit.stderr, stderr);
    assert_eq!(std::fs::read(metadata).unwrap(), metadata_bytes);
    assert_eq!(std::fs::read(library).unwrap(), library_bytes);
    service.shutdown(ShutdownMode::Graceful).await.unwrap();
}

#[tokio::test]
async fn rustc_failure_after_metadata_does_not_publish_a_successful_entry() {
    let temp = TempDir::new().unwrap();
    write_source(&temp);
    let service = start(&temp).await;
    let mut request = request(&temp, "dylib");
    request.args.push(format!(
        "-Clinker={}",
        temp.path().join("missing-linker").display()
    ));
    for _ in 0..2 {
        let mut stderr = Vec::new();
        let mut scanned = 0;
        let mut saw_metadata = false;
        service
            .compile_streaming(request.clone(), |chunk| match chunk {
                CompileChunk::Stderr(bytes) => {
                    stderr.extend(bytes);
                    if let Some(path) = metadata_line(&stderr, &mut scanned) {
                        assert_eq!(path, temp.path().join("out/libstreamcrate.rmeta"));
                        assert!(!std::fs::read(path).unwrap().is_empty());
                        saw_metadata = true;
                    }
                }
                CompileChunk::Done {
                    exit_code, cached, ..
                } => {
                    assert_ne!(exit_code, 0);
                    assert!(
                        !cached,
                        "failed link must not publish a successful artifact"
                    );
                }
                CompileChunk::Stdout(_) => {}
            })
            .await
            .expect("failed compiler response");
        assert!(saw_metadata);
    }
    service.shutdown(ShutdownMode::Graceful).await.unwrap();
}
