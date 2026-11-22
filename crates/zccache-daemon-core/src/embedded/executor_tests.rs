use super::*;

#[test]
fn executor_contract_is_public_and_object_safe() {
    struct Decline;
    impl CompileExecutor for Decline {
        fn capacity(&self) -> usize {
            1
        }
        fn execute<'a>(&'a self, _job: &'a CompileJob) -> ExecutorFuture<'a> {
            Box::pin(async {
                ExecutorOutcome::Declined {
                    reason: "unavailable".into(),
                }
            })
        }
    }
    let executor: Arc<dyn CompileExecutor> = Arc::new(Decline);
    assert_eq!(executor.capacity(), 1);
    assert_eq!(CompileExecution::default(), CompileExecution::Local);
}

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};
use tempfile::TempDir;

struct Fake {
    compiler: PathBuf,
    root: PathBuf,
    mode: u8,
    calls: AtomicUsize,
    jobs: Mutex<Vec<CompileJob>>,
}

impl CompileExecutor for Fake {
    fn capacity(&self) -> usize {
        2
    }
    fn execute<'a>(&'a self, job: &'a CompileJob) -> ExecutorFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.jobs.lock().unwrap().push(job.clone());
        Box::pin(async move {
            match self.mode {
                1 => ExecutorOutcome::Declined {
                    reason: "no matching toolchain".into(),
                },
                2 => ExecutorOutcome::Executed {
                    exit_code: 42,
                    stdout: vec![],
                    stderr: b"remote failure".to_vec(),
                },
                _ => mirror_compile(&self.compiler, &self.root, job).await,
            }
        })
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "test executor runs a real compiler in an isolated mirror"
)]
async fn mirror_compile(compiler: &Path, root: &Path, job: &CompileJob) -> ExecutorOutcome {
    let sandbox = TempDir::new().unwrap();
    let old = root.to_string_lossy();
    let new = sandbox.path().to_string_lossy();
    let old_dot = old.replace('\\', "/");
    let new_dot = new.replace('\\', "/");
    let remap = |s: &str| s.replace(old.as_ref(), new.as_ref());
    let restore = |bytes: Vec<u8>| match String::from_utf8(bytes) {
        // Clang DOT labels normalize separators and omit the leading slash.
        Ok(text) => text
            .replace(new.as_ref(), old.as_ref())
            .replace(
                new_dot.trim_start_matches('/'),
                old_dot.trim_start_matches('/'),
            )
            .into_bytes(),
        Err(error) => error.into_bytes(),
    };
    copy_tree(&root.join("work"), &sandbox.path().join("work"));
    for output in &job.outputs {
        std::fs::create_dir_all(
            Path::new(&remap(&output.to_string_lossy()))
                .parent()
                .unwrap(),
        )
        .unwrap();
    }
    let mut builder = kernal_api::SpawnSpec::new(compiler)
        .args(job.args.iter().map(|s| remap(s)))
        .current_dir(remap(&job.cwd.to_string_lossy()))
        .clear_env(true);
    for (name, value) in &job.exec_env {
        builder = builder.env(name, value);
    }
    let result = crate::daemon::process::async_builder_output_with_priority(
        builder,
        crate::daemon::process::CompilePriority::Normal,
        crate::daemon::process::ProcessLiveness::new(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    if result.status.success() {
        for output in &job.outputs {
            let mirrored = PathBuf::from(remap(&output.to_string_lossy()));
            if mirrored.exists() {
                std::fs::create_dir_all(output.parent().unwrap()).unwrap();
                let bytes = std::fs::read(&mirrored).unwrap();
                let bytes = if output
                    .extension()
                    .is_some_and(|s| s == "d" || s == "headers")
                {
                    restore(bytes)
                } else {
                    bytes
                };
                std::fs::write(output, bytes).unwrap();
            }
        }
    }
    ExecutorOutcome::Executed {
        exit_code: result.status.code().unwrap_or(-1),
        stdout: restore(result.stdout),
        stderr: restore(result.stderr),
    }
}

async fn start(temp: &TempDir) -> ZccacheService {
    let mut audit = AuditConfig::default();
    audit.mode = crate::audit::AuditMode::Off;
    ZccacheService::start(ZccacheConfig {
        host: HostIdentity::default_for_product("executor-test"),
        cache_root: temp.path().join("cache").into(),
        audit,
        limits: ServiceLimits::default(),
        runtime: RuntimeHooks::default(),
        cancellation: None,
    })
    .await
    .unwrap()
}

fn request(temp: &TempDir, rust: bool) -> CompileRequest {
    let work = temp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let (compiler, args) = if rust {
        std::fs::write(work.join("unit.rs"), "pub fn value() -> u32 { 7 }\n").unwrap();
        (
            crate::test_support::find_rustc().expect("rustc"),
            vec![
                "--crate-name=unit".into(),
                "--crate-type=rlib".into(),
                "--emit=link,dep-info".into(),
                "--out-dir".into(),
                work.to_string_lossy().into_owned(),
                work.join("unit.rs").to_string_lossy().into_owned(),
            ],
        )
    } else {
        std::fs::write(work.join("unit.h"), "#define VALUE 7\n").unwrap();
        std::fs::write(
            work.join("unit.c"),
            "#include \"unit.h\"\nint value(void) { return VALUE; }\n",
        )
        .unwrap();
        (
            {
                crate::test_support::find_clang().expect("clang toolchain");
                crate::test_support::find_on_path("clang").expect("C driver")
            },
            vec![
                "-c".into(),
                work.join("unit.c").to_string_lossy().into_owned(),
                "-o".into(),
                work.join("unit.o").to_string_lossy().into_owned(),
            ],
        )
    };
    CompileRequest {
        audit: AuditContext::new(
            crate::audit::AuditId::new("executor-run").unwrap(),
            crate::audit::AuditId::new("executor-trace").unwrap(),
        ),
        compiler,
        args,
        cwd: work.into(),
        env: Vec::new(),
        stdin: Vec::new(),
    }
}

fn fake(temp: &TempDir, req: &CompileRequest, mode: u8) -> Arc<Fake> {
    Arc::new(Fake {
        compiler: req.compiler.as_path().to_path_buf(),
        root: temp.path().to_path_buf(),
        mode,
        calls: AtomicUsize::new(0),
        jobs: Mutex::new(Vec::new()),
    })
}

fn check_input(job: &CompileJob, path: &Path) {
    let input = job
        .inputs
        .iter()
        .find(|i| i.path == path)
        .expect("manifest contains input");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(input.digest, *crate::hash::hash_bytes(&bytes).as_bytes());
    assert_eq!(input.size, bytes.len() as u64);
}

#[test]
fn remote_result_is_stored_and_hit_makes_no_offer() {
    crate::test_support::init_test_tracing();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = TempDir::new().unwrap();
        let req = request(&temp, false);
        let executor = fake(&temp, &req, 0);
        let service = start(&temp).await.with_executor(executor.clone()).unwrap();
        let miss = service.compile(req.clone()).await.unwrap();
        assert_eq!(
            miss.exit_code,
            0,
            "{}",
            String::from_utf8_lossy(&miss.stderr)
        );
        assert_eq!(miss.execution, CompileExecution::Remote);
        let hit = service.compile(req).await.unwrap();
        service.shutdown(ShutdownMode::Graceful).await.unwrap();
        let root = crate::core::config::effective_cache_root_from_top_level(
            &temp.path().join("cache").into(),
        );
        let log_dir = crate::core::config::log_dir_from_cache_dir(&root);
        let journal_path = log_dir.as_path().join("compile_journal.jsonl");
        let journal = wait_for_journal(&journal_path, 2).await;
        assert!(hit.cached, "response={hit:?}; journal={journal}");
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(hit.execution, CompileExecution::Local);
        let rows: Vec<serde_json::Value> = journal
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows[0]["execution"]["kind"], "remote");
        assert_eq!(rows[1]["execution"]["kind"], "local");
    });
}

#[test]
fn decline_runs_locally_and_no_executor_stays_local() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for mode in [None, Some(1)] {
            let temp = TempDir::new().unwrap();
            let req = request(&temp, false);
            let service = start(&temp).await;
            let service = if let Some(mode) = mode {
                service.with_executor(fake(&temp, &req, mode)).unwrap()
            } else {
                service
            };
            let miss = service.compile(req.clone()).await.unwrap();
            assert_eq!(miss.exit_code, 0);
            assert_eq!(
                miss.execution,
                if mode.is_some() {
                    CompileExecution::Declined {
                        reason: "no matching toolchain".into(),
                    }
                } else {
                    CompileExecution::Local
                }
            );
            assert!(service.compile(req).await.unwrap().cached);
            service.shutdown(ShutdownMode::Graceful).await.unwrap();
        }
    });
}

#[test]
fn remote_failure_forwards_stderr_and_never_caches() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for rust in [false, true] {
            let temp = TempDir::new().unwrap();
            let req = request(&temp, rust);
            let executor = fake(&temp, &req, 2);
            let service = start(&temp).await.with_executor(executor.clone()).unwrap();
            for _ in 0..2 {
                let result = service.compile(req.clone()).await.unwrap();
                assert_eq!(result.exit_code, 42);
                assert_eq!(result.stderr, b"remote failure");
                assert!(!result.cached);
                assert_eq!(result.execution, CompileExecution::Remote);
            }
            assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
            service.shutdown(ShutdownMode::Graceful).await.unwrap();
        }
    });
}

#[test]
fn manifests_contain_c_headers_and_rust_sources_with_digests() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for rust in [false, true] {
            let temp = TempDir::new().unwrap();
            let req = request(&temp, rust);
            let executor = fake(&temp, &req, 1);
            let service = start(&temp).await.with_executor(executor.clone()).unwrap();
            assert_eq!(service.compile(req.clone()).await.unwrap().exit_code, 0);
            let compiler_name = req.compiler.as_path().file_name().unwrap().to_os_string();
            let path = temp
                .path()
                .join(if rust { "work/unit.rs" } else { "work/unit.h" });
            std::fs::write(
                &path,
                if rust {
                    "pub fn value() -> u32 { 9 }\n"
                } else {
                    "#define VALUE 9\n"
                },
            )
            .unwrap();
            assert_eq!(service.compile(req).await.unwrap().exit_code, 0);
            {
                let jobs = executor.jobs.lock().unwrap();
                assert_eq!(jobs.len(), 2);
                assert_eq!(jobs[1].compiler_name, compiler_name);
                assert!(!jobs[0].inputs_complete);
                check_input(
                    &jobs[1],
                    &temp
                        .path()
                        .join(if rust { "work/unit.rs" } else { "work/unit.c" }),
                );
                check_input(&jobs[1], &path);
                assert!(jobs[1].inputs.iter().all(|i| i.path.is_absolute()));
            }
            service.shutdown(ShutdownMode::Graceful).await.unwrap();
        }
    });
}

#[test]
fn rust_manifest_includes_extern_digest() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = TempDir::new().unwrap();
        let mut req = request(&temp, true);
        let work = temp.path().join("work");
        std::fs::write(work.join("dep.rs"), "pub fn dep() -> u32 { 5 }\n").unwrap();
        let mut dependency = req.clone();
        dependency.args = vec![
            "--crate-name=dep".into(),
            "--crate-type=rlib".into(),
            "--emit=link,dep-info".into(),
            "--out-dir".into(),
            work.to_string_lossy().into_owned(),
            work.join("dep.rs").to_string_lossy().into_owned(),
        ];
        let service = start(&temp).await;
        let result = service.compile(dependency).await.unwrap();
        assert_eq!(
            result.exit_code,
            0,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let external = work.join("libdep.rlib");
        std::fs::write(
            work.join("unit.rs"),
            "extern crate dep; pub fn value() -> u32 { dep::dep() }\n",
        )
        .unwrap();
        req.args
            .push(format!("--extern=dep={}", external.display()));
        let executor = fake(&temp, &req, 1);
        let service = service.with_executor(executor.clone()).unwrap();
        let result = service.compile(req).await.unwrap();
        assert_eq!(
            result.exit_code,
            0,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        {
            let jobs = executor.jobs.lock().unwrap();
            check_input(&jobs[0], &external);
        }
        service.shutdown(ShutdownMode::Graceful).await.unwrap();
    });
}

struct RejectLocal;
impl HostAdmissionClassifier for RejectLocal {
    fn requires_exclusive(
        &self,
        _: &HostCompilerRequest<'_>,
    ) -> std::result::Result<bool, HostAdmissionError> {
        Err(HostAdmissionError::new(
            "local memory admission must not run",
        ))
    }
}

#[test]
fn remote_execution_bypasses_local_admission() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = TempDir::new().unwrap();
        let req = request(&temp, false);
        let mut audit = AuditConfig::default();
        audit.mode = crate::audit::AuditMode::Off;
        let service = ZccacheService::start_with_host_admission_classifier(
            ZccacheConfig {
                host: HostIdentity::default_for_product("remote-admission"),
                cache_root: temp.path().join("cache").into(),
                audit,
                limits: ServiceLimits::default(),
                runtime: RuntimeHooks::default(),
                cancellation: None,
            },
            Arc::new(RejectLocal),
        )
        .await
        .unwrap();
        let executor = fake(&temp, &req, 0);
        let service = service.with_executor(executor).unwrap();
        let result = service.compile(req).await.unwrap();
        assert_eq!(
            result.exit_code,
            0,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.execution, CompileExecution::Remote);
        service.shutdown(ShutdownMode::Graceful).await.unwrap();
    });
}

struct Bounded {
    active: AtomicUsize,
    peak: AtomicUsize,
}
impl CompileExecutor for Bounded {
    fn capacity(&self) -> usize {
        1
    }
    fn execute<'a>(&'a self, _: &'a CompileJob) -> ExecutorFuture<'a> {
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ExecutorOutcome::Declined {
                reason: "done".into(),
            }
        })
    }
}

#[test]
fn executor_capacity_is_shared_across_service_clones() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = TempDir::new().unwrap();
        let bounded = Arc::new(Bounded {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let service = start(&temp).await.with_executor(bounded.clone()).unwrap();
        let clone = service.clone();
        let job = CompileJob {
            compiler_kind: crate::compiler::CompilerFamily::Clang,
            compiler_name: "clang".into(),
            compiler_identity: [0; 32],
            args: vec![],
            env: vec![],
            exec_env: vec![],
            request_args: vec![],
            cwd: temp.path().into(),
            stdin: vec![],
            inputs: vec![],
            inputs_complete: false,
            outputs: vec![],
        };
        let (a, b) = tokio::join!(
            service.executor.as_ref().unwrap().execute(&job),
            clone.executor.as_ref().unwrap().execute(&job)
        );
        assert!(matches!(a, ExecutorOutcome::Declined { .. }));
        assert!(matches!(b, ExecutorOutcome::Declined { .. }));
        assert_eq!(bounded.peak.load(Ordering::SeqCst), 1);
        service.shutdown(ShutdownMode::Graceful).await.unwrap();
    });
}

#[test]
fn host_dependent_native_cpu_jobs_stay_local() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for rust in [false, true] {
            let temp = TempDir::new().unwrap();
            let mut req = request(&temp, rust);
            req.args.push(
                if rust {
                    "-Ctarget-cpu=native"
                } else {
                    "-march=native"
                }
                .into(),
            );
            let executor = fake(&temp, &req, 2);
            let service = start(&temp).await.with_executor(executor.clone()).unwrap();
            let result = service.compile(req).await.unwrap();
            assert_eq!(
                result.exit_code,
                0,
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                result.execution,
                CompileExecution::Declined {
                    reason: "native CPU selection requires local execution".into()
                }
            );
            service.shutdown(ShutdownMode::Graceful).await.unwrap();
        }
    });
}

#[test]
fn execution_environment_is_private_and_does_not_change_keys() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for rust in [true, false] {
            let temp = TempDir::new().unwrap();
            let mut req = request(&temp, rust);
            let sentinel = "EXEC_ENV_PRIVATE_SENTINEL_28371";
            req.env = ["CARGO_MANIFEST_DIR", "OUT_DIR", "COLD_CUSTOM_INPUT"]
                .map(|name| (name.to_owned(), sentinel.to_owned()))
                .to_vec();
            let executor = fake(&temp, &req, 1);
            let service = start(&temp).await.with_executor(executor.clone()).unwrap();
            let baseline = service.compile(req.clone()).await.unwrap();
            assert_eq!(baseline.exit_code, 0);
            let job = executor.jobs.lock().unwrap()[0].clone();
            assert_eq!(job.exec_env, req.env);
            assert_eq!(job.request_args, req.args);
            assert_eq!(job.cwd, req.cwd);
            assert!(!job.env.iter().any(|(_, value)| value == sentinel));
            assert!(!format!("{job:?}").contains(sentinel));
            req.env.clear();
            let unchanged = service.compile(req).await.unwrap();
            assert!(unchanged.cached, "execution-only env must not move the key");
            service.shutdown(ShutdownMode::Graceful).await.unwrap();
            let root = crate::core::config::effective_cache_root_from_top_level(
                &temp.path().join("cache").into(),
            );
            let logs = crate::core::config::log_dir_from_cache_dir(&root);
            let journal = wait_for_journal(&logs.as_path().join("compile_journal.jsonl"), 2).await;
            assert!(!journal.contains(sentinel));
            let rows: Vec<serde_json::Value> = journal
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["outcome"], "miss");
            assert_eq!(rows[1]["outcome"], "hit");
        }
    });
}

async fn wait_for_journal(path: &Path, rows: usize) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if text.lines().count() >= rows && text.ends_with('\n') {
                break text;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("complete journal rows")
}
