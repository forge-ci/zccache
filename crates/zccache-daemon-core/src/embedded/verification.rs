//! Executor verification policy and faults; independent of cache-key policy.
use super::CompileJob;

/// Distributed execution failed, independently of compiler diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorFaultKind {
    OutputMismatch,
    WorkerCrash,
    Drained,
    Transport,
    VerificationIo,
    InvalidPolicy,
}

/// A fault that must remain visible to the embedding build coordinator.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutorFault {
    pub kind: ExecutorFaultKind,
    pub detail: String,
}

/// Verification of successful remote offers. Executors can force the first job
/// on each worker/toolchain to Always and sample subsequent offers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerificationPolicy {
    #[default]
    Off,
    /// A deterministic percentage in 0..=100; invalid values fail terminally.
    Sampled {
        percent: u8,
    },
    Always,
}

impl VerificationPolicy {
    /// Selection is independent of offer order, private staging paths and the
    /// execution environment. This digest is never used as a cache key.
    pub fn selects(self, job: &CompileJob) -> Result<bool, ExecutorFault> {
        let percent = match self {
            Self::Off => return Ok(false),
            Self::Always => return Ok(true),
            Self::Sampled { percent } if percent <= 100 => percent,
            Self::Sampled { .. } => {
                return Err(ExecutorFault {
                    kind: ExecutorFaultKind::InvalidPolicy,
                    detail: "verification percentage must be in 0..=100".into(),
                })
            }
        };
        let mut hash = crate::hash::StreamHasher::new();
        hash.update(b"zccache-executor-verification-v1\0");
        hash.update(&job.compiler_identity);
        for arg in &job.request_args {
            hash.update(&(arg.len() as u64).to_le_bytes());
            hash.update(arg.as_bytes());
        }
        for input in &job.inputs {
            hash.update(&input.digest);
        }
        let digest = hash.finalize();
        let [a, b, c, d, ..] = *digest.as_bytes();
        let bucket = u32::from_le_bytes([a, b, c, d]) % 100;
        Ok(bucket < u32::from(percent))
    }
}
