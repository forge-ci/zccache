//! Verification owns no compiler spawn: it resumes the normal admitted path.
use super::super::super::*;
use crate::embedded::executor::Registration;
use crate::embedded::{
    CompileExecution, CompileJob, ExecutorFault, ExecutorFaultKind, ExecutorOutcome,
};
use std::io::{self, BufRead};

pub(super) enum Offer {
    Outcome(ExecutorOutcome),
    Verify(Box<Verification>),
}

pub(super) struct Verification {
    executor: Arc<Registration>,
    job: CompileJob,
    snapshot: Snapshot,
}

struct Snapshot {
    root: NormalizedPath,
    retirer: crate::daemon::server::PrivateRootRetirer,
    files: Vec<(NormalizedPath, Option<NormalizedPath>)>,
}

impl Snapshot {
    fn capture(
        root: &Path,
        outputs: &[NormalizedPath],
        cleanup: &[NormalizedPath],
        retirer: crate::daemon::server::PrivateRootRetirer,
    ) -> io::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("executor-verification-")
            .tempdir_in(root)?;
        let mut files = Vec::with_capacity(outputs.len());
        for (index, path) in outputs.iter().enumerate() {
            let saved = match std::fs::symlink_metadata(path.as_path()) {
                Ok(metadata) if metadata.is_file() => {
                    let saved: NormalizedPath = root.path().join(index.to_string()).into();
                    std::fs::copy(path.as_path(), saved.as_path())?;
                    Some(saved)
                }
                Ok(_) => return Err(io::Error::other("remote output is not a regular file")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            files.push((path.clone(), saved));
        }
        // Never let a file left by the remote compile masquerade as local output.
        clear_outputs(cleanup)?;
        Ok(Self {
            root: root.keep().into(),
            retirer,
            files,
        })
    }

    fn matches(&self) -> io::Result<bool> {
        for (path, saved) in &self.files {
            match (saved, std::fs::symlink_metadata(path.as_path())) {
                (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
                (Some(saved), Ok(metadata)) if metadata.is_file() => {
                    if !same_bytes(saved, path)? {
                        return Ok(false);
                    }
                }
                (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // Retirement only sends a message, including on cancellation/admission errors.
        self.retirer.retire(self.root.as_path());
    }
}

fn same_bytes(left: &NormalizedPath, right: &NormalizedPath) -> io::Result<bool> {
    let mut left = io::BufReader::new(std::fs::File::open(left.as_path())?);
    let mut right = io::BufReader::new(std::fs::File::open(right.as_path())?);
    loop {
        let a = left.fill_buf()?;
        let b = right.fill_buf()?;
        let count = a.len().min(b.len());
        if count == 0 {
            return Ok(a.is_empty() && b.is_empty());
        }
        if a[..count] != b[..count] {
            return Ok(false);
        }
        left.consume(count);
        right.consume(count);
    }
}

fn clear_outputs(paths: &[NormalizedPath]) -> io::Result<()> {
    for path in paths {
        match std::fs::remove_file(path.as_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn io_fault(detail: impl std::fmt::Display) -> ExecutorFault {
    ExecutorFault {
        kind: ExecutorFaultKind::VerificationIo,
        detail: detail.to_string(),
    }
}

async fn terminal(executor: &Registration, job: &CompileJob, mut fault: ExecutorFault) -> Offer {
    let paths = job.outputs.clone();
    let cleanup = kernal_api::async_engine::launch_blocking(move || clear_outputs(&paths))
        .await
        .map_err(io_fault)
        .and_then(|result| result.map_err(io_fault));
    if let Err(error) = cleanup {
        fault.detail.push_str(&format!(
            "; partial-output cleanup failed: {}",
            error.detail
        ));
    }
    executor.report_fault(job, &fault);
    Offer::Outcome(ExecutorOutcome::Faulted {
        kind: fault.kind,
        detail: fault.detail,
    })
}

pub(super) async fn complete_offer(
    executor: Arc<Registration>,
    job: CompileJob,
    outcome: ExecutorOutcome,
    outputs: &[NormalizedPath],
    state: &SharedState,
) -> Offer {
    match outcome {
        ExecutorOutcome::Faulted { kind, detail } => {
            return terminal(&executor, &job, ExecutorFault { kind, detail }).await;
        }
        ExecutorOutcome::Declined { ref reason } => {
            crate::embedded::executor::record(CompileExecution::Declined {
                reason: reason.clone(),
            });
        }
        ExecutorOutcome::Executed { exit_code: 0, .. } => {
            match executor.verification_policy(&job).selects(&job) {
                Ok(true) => {
                    let paths = outputs.to_vec();
                    let cleanup = job.outputs.clone();
                    let Some(retirer) = state.staging.retirer() else {
                        return terminal(
                            &executor,
                            &job,
                            io_fault("verification cleanup owner unavailable"),
                        )
                        .await;
                    };
                    let root = state.depfile_tmpdir.clone();
                    let snapshot = kernal_api::async_engine::launch_blocking(move || {
                        Snapshot::capture(root.as_path(), &paths, &cleanup, retirer)
                    })
                    .await;
                    return match snapshot {
                        Ok(Ok(snapshot)) => Offer::Verify(Box::new(Verification {
                            executor,
                            job,
                            snapshot,
                        })),
                        Ok(Err(error)) => terminal(&executor, &job, io_fault(error)).await,
                        Err(error) => terminal(&executor, &job, io_fault(error)).await,
                    };
                }
                Err(fault) => return terminal(&executor, &job, fault).await,
                Ok(false) => crate::embedded::executor::record(CompileExecution::Remote),
            }
        }
        ExecutorOutcome::Executed { .. } => {
            crate::embedded::executor::record(CompileExecution::Remote)
        }
    }
    Offer::Outcome(outcome)
}

impl Verification {
    pub(super) async fn finish(self, exit_code: i32) {
        let Self {
            executor,
            job,
            snapshot,
        } = self;
        // Snapshot cleanup, reads and comparison all stay on the blocking lane.
        let compared = kernal_api::async_engine::launch_blocking(move || {
            if exit_code == 0 {
                snapshot.matches()
            } else {
                Ok(false)
            }
        })
        .await;
        let fault = match compared {
            Ok(Ok(true)) => {
                crate::embedded::executor::record(CompileExecution::Verified);
                executor.report_verified(&job);
                return;
            }
            Ok(Ok(false)) => ExecutorFault {
                kind: ExecutorFaultKind::OutputMismatch,
                detail: format!(
                    "remote outputs differ from admitted local compiler (local exit {exit_code})"
                ),
            },
            Ok(Err(error)) => io_fault(error),
            Err(error) => io_fault(error),
        };
        executor.report_fault(&job, &fault);
    }
}

pub(super) async fn uncached_fault_result(
    state: &Arc<SharedState>,
    plan: &mut Option<StagedCompilePlan>,
    exit_code: i32,
    stdout: &Arc<Vec<u8>>,
    stderr: &Arc<Vec<u8>>,
    mode: crate::core::config::MaterializationMode,
) -> Option<Response> {
    if !matches!(
        crate::embedded::executor::execution(),
        CompileExecution::Faulted { .. }
    ) {
        return None;
    }
    if let Some(plan) = plan.take() {
        let state = Arc::clone(state);
        let materialized = kernal_api::async_engine::launch_blocking(move || {
            if exit_code == 0 {
                plan.materialize_for_state(&state, mode).map(|_| ())
            } else {
                plan.cleanup()
            }
        })
        .await;
        if let Err(error) = materialized
            .map_err(io_fault)
            .and_then(|result| result.map_err(io_fault))
        {
            return Some(Response::Error {
                message: format!(
                    "failed to materialize trusted local outputs: {}",
                    error.detail
                ),
            });
        }
    }
    Some(Response::CompileResult {
        exit_code,
        stdout: Arc::clone(stdout),
        stderr: Arc::clone(stderr),
        cached: false,
    })
}
