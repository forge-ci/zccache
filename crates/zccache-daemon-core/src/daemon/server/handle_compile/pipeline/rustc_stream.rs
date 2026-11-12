//! Live staged Rust output; captured/cache bytes retain their private spelling.
use super::super::super::*;
use crate::daemon::compile_output::{
    self, CapturedOutput, OutputChunk, OutputContext, OutputFilter, RawOutputChunk,
};
use kernal_api::async_engine::Receiver;
use std::io;

pub(super) async fn consume(
    receiver: Receiver<RawOutputChunk>,
    context: OutputContext,
    filter: OutputFilter<'_>,
    staged_plan: &mut Option<StagedCompilePlan>,
    is_rustc: bool,
    state: &Arc<SharedState>,
    mode: crate::core::config::MaterializationMode,
) -> io::Result<CapturedOutput> {
    if !is_rustc || staged_plan.is_none() {
        return compile_output::consume(receiver, context, filter).await;
    }
    let plan = Arc::new(
        staged_plan
            .take()
            .ok_or_else(|| io::Error::other("missing Rust staging plan"))?,
    );
    let (sender, live_receiver) = kernal_api::async_engine::channel(8);
    let capture = compile_output::consume_rustc(receiver, context.redirect(sender), filter);
    let relay = relay(live_receiver, &context, &plan, state, mode);
    let (captured, delivered) = tokio::join!(capture, relay);
    let mut plan = Arc::try_unwrap(plan)
        .map_err(|_| io::Error::other("metadata publisher retained staging plan"))?;
    plan.materialized_metadata = delivered?;
    *staged_plan = Some(plan);
    captured
}

async fn relay(
    mut receiver: Receiver<OutputChunk>,
    context: &OutputContext,
    plan: &Arc<StagedCompilePlan>,
    state: &Arc<SharedState>,
    mode: crate::core::config::MaterializationMode,
) -> io::Result<Vec<NormalizedPath>> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut delivered = Vec::new();
    loop {
        let chunk = match receiver
            .recv_within(zccache_core::wait::LIFECYCLE_POLL)
            .await
        {
            Ok(chunk) => chunk,
            Err(kernal_api::async_engine::RecvTimeoutError::TimedOut) => continue,
            Err(kernal_api::async_engine::RecvTimeoutError::Disconnected) => break,
        };
        let (pending, is_stderr, bytes) = match chunk {
            OutputChunk::Stdout(bytes) => (&mut stdout, false, bytes),
            OutputChunk::Stderr(bytes) => (&mut stderr, true, bytes),
        };
        pending.extend(bytes);
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=end).collect::<Vec<_>>();
            forward(context, plan, state, mode, is_stderr, &line, &mut delivered).await?;
        }
    }
    forward(context, plan, state, mode, false, &stdout, &mut delivered).await?;
    forward(context, plan, state, mode, true, &stderr, &mut delivered).await?;
    Ok(delivered)
}

async fn forward(
    context: &OutputContext,
    plan: &Arc<StagedCompilePlan>,
    state: &Arc<SharedState>,
    mode: crate::core::config::MaterializationMode,
    is_stderr: bool,
    bytes: &[u8],
    delivered: &mut Vec<NormalizedPath>,
) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if is_stderr {
        publish_metadata(plan, state, mode, bytes, delivered).await?;
    }
    let canonical = plan.canonicalize_output_bytes(bytes);
    let bytes = plan.rehydrate_output_bytes(&canonical);
    let chunk = if is_stderr {
        OutputChunk::Stderr(bytes)
    } else {
        OutputChunk::Stdout(bytes)
    };
    context.send(chunk).await
}

async fn publish_metadata(
    plan: &Arc<StagedCompilePlan>,
    state: &Arc<SharedState>,
    mode: crate::core::config::MaterializationMode,
    line: &[u8],
    delivered: &mut Vec<NormalizedPath>,
) -> io::Result<()> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Ok(());
    };
    if value.get("emit").and_then(serde_json::Value::as_str) != Some("metadata") {
        return Ok(());
    }
    let Some(path) = value.get("artifact").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let path = NormalizedPath::new(path);
    let output = plan
        .outputs
        .iter()
        .find(|output| output.staged == path)
        .ok_or_else(|| io::Error::other("metadata artifact is outside its staging plan"))?;
    if delivered.contains(&output.requested) {
        return Ok(());
    }
    let requested = output.requested.clone();
    let output = output.clone();
    // The blocking operation retains the private plan even if the compile is
    // cancelled. It cannot outlive its source's cleanup owner.
    let plan_owner = Arc::clone(plan);
    let state = Arc::clone(state);
    kernal_api::async_engine::launch_blocking(move || {
        let _plan_owner = plan_owner;
        let scope = OutputScope::for_state(&state)?;
        let destination = scope.output(output.requested.as_path())?;
        if let Some(parent) = output.requested.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stats =
            materialize_independent_with_mode_scoped(output.staged.as_path(), &destination, mode)?;
        use crate::daemon::staged_stats::{StagedBytes, StagedCounter};
        state
            .profiler
            .staged
            .add_count(StagedCounter::MaterializeReflink, stats.reflink_count);
        state
            .profiler
            .staged
            .add_count(StagedCounter::MaterializeCopy, stats.copy_count);
        state
            .profiler
            .staged
            .bytes(StagedBytes::Materialization, stats.copy_bytes);
        state.cache_system.apply_changes(vec![output.requested]);
        Ok::<_, io::Error>(())
    })
    .await
    .map_err(io::Error::other)??;
    delivered.push(requested);
    Ok(())
}
