//! Opt-in miss execution for embedded hosts.
use kernal_api::async_engine::Semaphore;
use std::{cell::RefCell, future::Future, pin::Pin, sync::Arc, time::Duration};
use zccache_core::path::NormalizedPath;

/// Object-safe asynchronous execution result. Implementations must bound their I/O
/// and finish or cancel all output writes before returning or dropping this future.
pub type ExecutorFuture<'a> = Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>>;

/// Executes cache misses on an embedder-owned execution service.
pub trait CompileExecutor: Send + Sync + 'static {
    /// Maximum simultaneous offers. Captured when registering; must be nonzero.
    fn capacity(&self) -> usize;
    /// Materialize outputs at the declared local paths before returning Executed.
    /// Declining must leave those paths untouched. An incomplete manifest should
    /// be declined unless the implementation can independently resolve its inputs.
    fn execute<'a>(&'a self, job: &'a CompileJob) -> ExecutorFuture<'a>;
}

/// One input file identified by its raw BLAKE3 content digest.
#[derive(Debug, Clone)]
pub struct CompileInput {
    /// Absolute client path.
    pub path: NormalizedPath,
    /// BLAKE3 digest, without cache-key domain separation.
    pub digest: [u8; 32],
    /// File length in bytes.
    pub size: u64,
    /// Whether any executable permission bit is set (false on Windows).
    pub executable: bool,
}

/// Resolved compiler invocation. Toolchain files are supplied by the executor.
#[derive(Debug, Clone)]
pub struct CompileJob {
    /// Compiler family detected by zccache.
    pub compiler_kind: crate::compiler::CompilerFamily,
    /// Executable basename, preserving driver mode such as clang versus clang++.
    /// This contains no client toolchain directory.
    pub compiler_name: std::ffi::OsString,
    /// The exact compiler identity digest used in the local cache key.
    pub compiler_identity: [u8; 32],
    /// Expanded argv, including zccache's depfile/remap/staging injections.
    pub args: Vec<String>,
    /// Key-relevant environment only; excludes daemon lineage and jobserver fds.
    pub env: Vec<(String, String)>,
    /// Absolute client working directory.
    pub cwd: NormalizedPath,
    /// Standard input supplied by the caller.
    pub stdin: Vec<u8>,
    /// Known input files; toolchain/sysroot contents are not transferred.
    pub inputs: Vec<CompileInput>,
    /// True only when dependency discovery covers this exact invocation.
    pub inputs_complete: bool,
    /// Absolute local output paths, including dependency-discovery sidecars.
    pub outputs: Vec<NormalizedPath>,
}

/// An executor must report infrastructure failure as Declined before writing
/// outputs, or as a nonzero Executed result. Failed compiles are never stored.
#[derive(Debug)]
pub enum ExecutorOutcome {
    /// Outputs have been written to their declared local destinations.
    Executed {
        exit_code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// Run locally with the normal memory and exclusive-access admission.
    Declined { reason: String },
}

/// Where a miss ran. A hit reports Local because no executor was offered work.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompileExecution {
    /// Normal local execution, or a hit with no compiler execution.
    #[default]
    Local,
    /// The registered executor completed the invocation, including failed exits.
    Remote,
    /// The executor declined and normal local execution followed.
    Declined { reason: String },
}

pub(crate) struct Registration {
    executor: Arc<dyn CompileExecutor>,
    slots: Semaphore,
}

struct Scope {
    registration: Option<Arc<Registration>>,
    execution: RefCell<CompileExecution>,
}

kernal_api::task_local! { static ACTIVE: Scope = ACTIVE_EXECUTOR_TLS; }

impl super::ZccacheService {
    /// Register an executor before sharing the service. Cloned handles share its
    /// capacity limit. No executor preserves the existing local compile path.
    pub fn with_executor(mut self, executor: Arc<dyn CompileExecutor>) -> super::Result<Self> {
        let capacity = executor.capacity();
        if capacity == 0 || capacity > Semaphore::MAX_PERMITS {
            return Err(super::EmbeddedError::Start(
                "invalid executor capacity".into(),
            ));
        }
        self.executor = Some(Arc::new(Registration {
            executor,
            slots: Semaphore::new(capacity),
        }));
        Ok(self)
    }
}

pub(crate) async fn scope<F: Future>(
    registration: Option<Arc<Registration>>,
    future: F,
) -> F::Output {
    ACTIVE
        .scope(
            Scope {
                registration,
                execution: RefCell::new(CompileExecution::Local),
            },
            future,
        )
        .await
}

pub(crate) fn current() -> Option<Arc<Registration>> {
    ACTIVE.try_with(|s| s.registration.clone()).ok().flatten()
}

pub(crate) fn execution() -> CompileExecution {
    ACTIVE
        .try_with(|s| s.execution.borrow().clone())
        .unwrap_or_default()
}

pub(crate) fn record(value: CompileExecution) {
    let _ = ACTIVE.try_with(|s| *s.execution.borrow_mut() = value);
}

impl Registration {
    pub(crate) async fn execute(&self, job: &CompileJob) -> ExecutorOutcome {
        let permit = match self.slots.acquire_within(Duration::from_secs(300)).await {
            Ok(permit) => permit,
            _ => {
                return ExecutorOutcome::Declined {
                    reason: "executor admission timed out".into(),
                }
            }
        };
        let outcome = self.executor.execute(job).await;
        drop(permit);
        outcome
    }
}
