//! Embedded executor adapter; local admission remains owned by compile_exec.
use super::super::super::*;
use crate::embedded::{CompileExecution, CompileInput, CompileJob, ExecutorOutcome};

pub(super) struct Manifest {
    paths: Vec<NormalizedPath>,
    identity: [u8; 32],
    env: Vec<(String, String)>,
    complete: bool,
    request: RequestInvocation,
}

/// Captured before request preparation can modify environment or arguments.
pub(super) struct RequestInvocation {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

pub(super) fn capture(
    args: &[String],
    env: Option<&[(String, String)]>,
) -> Option<RequestInvocation> {
    crate::embedded::executor::current()?;
    // Both copies outlive preparation and are owned only by the executor offer.
    Some(RequestInvocation {
        args: args.to_vec(),
        env: env.unwrap_or_default().to_vec(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    state: &SharedState,
    context_key: &ContextKey,
    ctx: &CompileContext,
    compiler: &NormalizedPath,
    args: &[String],
    env: Option<&[(String, String)]>,
    complete: bool,
    request: Option<RequestInvocation>,
) -> Option<Manifest> {
    let request = request?;
    let paths = request_cache_input_paths(state, context_key, &ctx.source_file, ctx);
    let mut keyed: Vec<(String, String)> =
        request_env_fingerprint_vars(env, &compiler.to_string_lossy(), args)
            .into_iter()
            .filter(|(k, _)| !k.starts_with("ZCCACHE_"))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    if let Some(deps) = state.dep_graph.load().get_rustc_env_deps(context_key) {
        for (name, _) in deps {
            if let Some((_, value)) = env
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|(k, _)| k == &name)
            {
                keyed.push((name, value.clone()));
            }
        }
    }
    keyed.sort();
    keyed.dedup();
    let safe = crate::daemon::compile_journal::sanitize_journal_env(Some(keyed));
    let env = safe.map_or_else(Vec::new, |s| s.as_slice().to_vec());
    Some(Manifest {
        paths,
        request,
        identity: *ctx.compiler_hash.as_bytes(),
        complete,
        env,
    })
}

pub(super) async fn offer(
    manifest: Option<Manifest>,
    req: &super::compile_exec::CompileExecRequest<'_>,
    args: &[String],
    outputs: &[NormalizedPath],
    strategy: &DepfileStrategy,
    trace: Option<&NormalizedPath>,
) -> Option<super::verify_exec::Offer> {
    let executor = crate::embedded::executor::current()?;
    let mut manifest = manifest?;
    // Journal privacy filters only the diagnostic/key subset. The trusted
    // embedded executor selects transport-safe values from the transient exec_env.
    let refusal = if req
        .effective_args
        .iter()
        .any(|arg| crate::depgraph::native_cpu::is_cxx_native_cpu_flag(arg))
        || req.rustc_args_opt.is_some_and(|rust| {
            rust.codegen_flags
                .iter()
                .any(|flag| crate::depgraph::native_cpu::is_rustc_native_cpu_flag(flag))
        }) {
        Some("native CPU selection requires local execution")
    } else if req.is_rustc
        && !crate::daemon::server::compile_resource_gate::rust_crate_types_are_non_linking(
            req.effective_args,
        )
    {
        Some("final Rust linking stays local")
    } else if req.pch_overlay.is_some() {
        Some("private PCH overlay requires local execution")
    } else if args.iter().any(|arg| arg.starts_with('@')) {
        Some("literal response-file arguments require local encoding")
    } else {
        None
    };
    if let Some(reason) = refusal {
        let reason = reason.to_string();
        crate::embedded::executor::record(CompileExecution::Declined {
            reason: reason.clone(),
        });
        return Some(super::verify_exec::Offer::Outcome(
            ExecutorOutcome::Declined { reason },
        ));
    }
    manifest
        .paths
        .extend(req.rustc_extern_paths.iter().cloned());
    manifest.paths.sort();
    manifest.paths.dedup();
    let verification_outputs = outputs;
    let mut outputs = outputs.to_vec();
    match strategy {
        DepfileStrategy::Injected { path }
        | DepfileStrategy::InjectedMmd { path }
        | DepfileStrategy::UserSpecified { path, .. }
        | DepfileStrategy::UserDefault { path, .. } => outputs.push(path.clone()),
        _ => {}
    }
    if let Some(trace) = trace {
        outputs.push(trace.clone());
    }
    let mut job = CompileJob {
        compiler_kind: req.compilation.family,
        compiler_name: req.compiler.as_path().file_name()?.to_os_string(),
        compiler_identity: manifest.identity,
        args: args.to_vec(),
        env: manifest.env,
        exec_env: manifest.request.env,
        request_args: manifest.request.args,
        cwd: req.cwd_path.clone(),
        stdin: req.stdin.to_vec(),
        inputs: Vec::new(),
        inputs_complete: manifest.complete,
        outputs,
    };
    let (inputs, complete) = kernal_api::async_engine::launch_blocking(move || {
        let mut complete = true;
        let inputs = manifest
            .paths
            .into_iter()
            .filter_map(|path| {
                let result = input(path);
                complete &= result.is_ok();
                result.ok()
            })
            .collect();
        (inputs, complete)
    })
    .await
    .ok()?;
    job.inputs = inputs;
    job.inputs_complete &= complete;
    let outcome = executor.execute(&job).await;
    Some(
        super::verify_exec::complete_offer(
            executor,
            job,
            outcome,
            verification_outputs,
            req.state_arc.depfile_tmpdir.as_path(),
        )
        .await,
    )
}

fn input(path: NormalizedPath) -> std::io::Result<CompileInput> {
    let metadata = std::fs::metadata(path.as_path())?;
    let executable = crate::platform::fs::permissions::mode(&metadata) & 0o111 != 0;
    let digest = *crate::hash::hash_file(path.as_path())?.as_bytes();
    Ok(CompileInput {
        path,
        digest,
        size: metadata.len(),
        executable,
    })
}
