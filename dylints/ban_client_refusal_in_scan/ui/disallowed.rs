// Mirrors the daemon's refusal types; the lint matches them by path.
mod pch_inputs {
    pub struct PchRefusal {
        pub path: String,
        pub reason: PchRefusalReason,
    }
    pub enum PchRefusalReason {
        Unreadable(String),
        ChangedDuringReplay,
    }
}
enum Response {
    Error { message: String },
}
enum CompileExecResult {
    Error(Response),
}

use pch_inputs::{PchRefusal, PchRefusalReason};

fn main() {
    let _ = PchRefusal {
        path: String::new(),
        reason: PchRefusalReason::Unreadable(String::new()),
    };
    let _ = PchRefusalReason::ChangedDuringReplay;
    let _ = CompileExecResult::Error(Response::Error {
        message: String::new(),
    });
}
