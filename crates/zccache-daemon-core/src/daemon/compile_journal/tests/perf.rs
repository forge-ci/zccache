//! Perf budget tests for the compile journal hot path.
//!
//! Issue #459: `JournalEntry::new` + `CompileJournal::log` together used to
//! cost ~2–4 µs on Windows (SystemTime::now + format_timestamp +
//! serde_json::to_string) and the client used to wait on that work before
//! `conn.send` returned. The fix reorders the daemon dispatch loop so the
//! response is sent first; these tests pin the per-call cost so a future
//! regression that adds more synchronous work to the hot path is caught.
//!
//! Budget is set ~10× looser than typical, and each guard passes on its fastest
//! short batch so host load doesn't false-positive — the gate is "did someone accidentally add a syscall
//! or unbounded allocation per call", not micro-benchmarking.

use std::time::{Duration, Instant};

use super::super::{JournalContext, JournalEntry};
use super::make_ctx;

/// Pass on the first batch within `budget_per_call` per call, up to `calls`.
///
/// One long batch fails whenever the host preempts this thread (#2081), and
/// even the median of short batches failed a third of runs at load 25-30, since
/// contention slows a batch without preempting it. Load only adds time to this
/// CPU-bound work, so one fast batch proves the cost, while a cost paid by every
/// call fails every batch. The limit: a cost paid less often than once per batch
/// can escape, so each batch stays as large as fits in a scheduler slice.
fn assert_some_batch_within_budget(
    label: &str,
    calls: u32,
    batch: u32,
    budget_per_call: Duration,
    mut call: impl FnMut(),
) {
    let budget = budget_per_call * batch;
    let mut fastest = Duration::MAX;
    for _ in 0..calls / batch {
        let start = Instant::now();
        for _ in 0..batch {
            call();
        }
        fastest = fastest.min(start.elapsed());
        if fastest < budget {
            return;
        }
    }
    panic!(
        "{label} regressed beyond budget: fastest of {} batches took {} ns for {batch} calls \
         (budget {} ns per call)",
        calls / batch,
        fastest.as_nanos(),
        budget_per_call.as_nanos()
    );
}

/// Building a `JournalEntry` should stay allocation-bounded. The old
/// implementation paid `SystemTime::now()` + a 24-byte `format!` heap alloc
/// per call. Any future change that adds a fresh syscall or string
/// allocation per call would push this past budget.
#[test]
fn journal_entry_new_stays_under_budget() {
    // Budget: 10 us / call. The pre-fix path measured ~3-4 us / call on
    // Windows and ~0.3 us / call on Linux, so this sits at ~3x the worst-case
    // pre-fix cost.
    assert_some_batch_within_budget(
        "JournalEntry::new",
        10_000,
        320,
        Duration::from_micros(10),
        || {
            let ctx = make_ctx(vec!["--crate-name", "x", "--crate-type", "lib"]);
            let _e = JournalEntry::new(ctx, "hit", 0, 1_000_000, None);
        },
    );
}

#[test]
fn env_sanitization_and_serialization_stay_under_budget() {
    let mut request_env = vec![
        ("CC".to_string(), "clang".to_string()),
        ("CARGO_CRATE_NAME".to_string(), "demo".to_string()),
        ("CARGO_CFG_TARGET_ARCH".to_string(), "x86_64".to_string()),
        ("RUSTFLAGS".to_string(), "-Cdebuginfo=1".to_string()),
        ("TARGET".to_string(), "x86_64-unknown-linux-gnu".to_string()),
        ("GITHUB_TOKEN".to_string(), "ghp_synthetic".to_string()),
        ("AWS_SECRET_ACCESS_KEY".to_string(), "synthetic".to_string()),
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
    ];
    request_env
        .extend((0..32).map(|index| (format!("UNRELATED_{index}"), format!("ordinary-{index}"))));

    // Budget: 375 us / call.
    assert_some_batch_within_budget(
        "env-present journal path",
        2_000,
        8,
        Duration::from_micros(375),
        || {
            let ctx = JournalContext::new(
                "rustc".to_string(),
                vec!["--crate-name".to_string(), "demo".to_string()],
                "/project".to_string(),
                Some(request_env.clone()),
                None,
            );
            let entry = JournalEntry::new(ctx, "hit", 0, 1_000_000, None);
            let _json = serde_json::to_vec(&entry).unwrap();
        },
    );
}
