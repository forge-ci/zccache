//! The existing Shutdown response acknowledges completed persistence (#1984).

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShutdownError {
    BudgetExceeded,
    Persistence,
}

impl ShutdownError {
    pub(super) fn response(self) -> Response {
        let message = match self {
            Self::BudgetExceeded => {
                "shutdown_budget_exceeded: persistence drain exceeded its duration budget"
            }
            Self::Persistence => {
                "shutdown_persistence_failed: cache state is incomplete; see daemon log"
            }
        };
        Response::Error {
            message: message.to_string(),
        }
    }
}

pub(super) struct Completion {
    result: AsyncState<Option<Result<(), ShutdownError>>>,
    ready: Notify,
    replies: std::sync::atomic::AtomicUsize,
    reply_sent: Notify,
    pub(super) failed: AtomicBool,
    pub(super) budget: Duration,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            result: AsyncState::new(None),
            ready: Notify::new(),
            replies: std::sync::atomic::AtomicUsize::new(0),
            reply_sent: Notify::new(),
            failed: AtomicBool::new(false),
            budget: crate::core::wait::HELPER_JOIN * 2,
        }
    }
}

pub(super) struct Reply<'a>(&'a Completion);

pub(super) struct UnwindFailure<'a>(&'a Completion);

impl Drop for UnwindFailure<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.fail();
        }
    }
}

impl Drop for Reply<'_> {
    fn drop(&mut self) {
        self.0.replies.fetch_sub(1, Ordering::AcqRel);
        self.0.reply_sent.notify_waiters();
    }
}

impl Completion {
    pub(super) fn watch_unwind(&self) -> UnwindFailure<'_> {
        UnwindFailure(self)
    }

    pub(super) fn fail(&self) {
        self.failed.store(true, Ordering::Release);
    }

    /// Register before signalling shutdown; keep the owner alive through send.
    pub(super) fn reply(&self) -> Reply<'_> {
        self.replies.fetch_add(1, Ordering::AcqRel);
        Reply(self)
    }

    pub(super) async fn response(&self) -> Response {
        let result =
            kernal_api::async_engine::timeout(self.budget + crate::core::wait::PROBE, async {
                loop {
                    let ready = self.ready.notified();
                    let mut ready = std::pin::pin!(ready);
                    ready.as_mut().enable();
                    if let Some(result) = self.result.with_for(STATE_TIMEOUT, |slot| *slot).await {
                        return result;
                    }
                    ready.await;
                }
            })
            .await
            .unwrap_or(Err(ShutdownError::BudgetExceeded));
        match result {
            Ok(()) => Response::ShuttingDown,
            Err(error) => error.response(),
        }
    }

    pub(super) async fn finish(&self, result: Result<(), ShutdownError>) {
        self.result
            .with_for(STATE_TIMEOUT, |slot| *slot = Some(result))
            .await;
        self.ready.notify_waiters();
        // run() must not return and tear down the runtime before the response
        // reaches the socket. A disconnected caller drops its Reply as well.
        let _ = kernal_api::async_engine::timeout(crate::core::wait::PROBE, async {
            loop {
                let sent = self.reply_sent.notified();
                let mut sent = std::pin::pin!(sent);
                sent.as_mut().enable();
                if self.replies.load(Ordering::Acquire) == 0 {
                    break;
                }
                sent.await;
            }
        })
        .await;
    }
}
