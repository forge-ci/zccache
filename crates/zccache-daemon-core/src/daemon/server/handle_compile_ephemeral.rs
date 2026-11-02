//! Single-roundtrip ephemeral compile dispatch + direct (uncached) compiler
//! invocation.
//!
//! `handle_compile_ephemeral` is the wrapper-mode fast path: it inlines
//! session start, compile, and session end into one IPC roundtrip.
//!
//! `run_compiler_direct` is the bypass for non-cacheable invocations — it
//! shells out to the compiler with no caching, while still tracking lineage
//! and replaying the client's env.

use super::*;

/// Handle a single-roundtrip ephemeral compile: session start + compile + session end.
/// Avoids 3 IPC roundtrips for drop-in wrapper mode.
#[allow(clippy::too_many_arguments)] // Single dispatch hop; ergonomic refactor unblocked once we stop adding new client-side fields.
pub(super) async fn handle_compile_ephemeral(
    state: &Arc<SharedState>,
    client_pid: u32,
    working_dir: &Path,
    compiler: &Path,
    args: &[String],
    cwd: &Path,
    env: Option<Vec<(String, String)>>,
    stdin: Vec<u8>,
) -> Response {
    // 1. Start ephemeral session (inline, no IPC roundtrip)
    state.stats.record_session();
    let session_resp = handle_session_start(
        state,
        SessionStartArgs {
            client_pid,
            working_dir,
            // DIAG seam: when ZCCACHE_EPHEMERAL_LOG is set, route this
            // ephemeral session's log (incl. [DIAG] depgraph_check lines)
            // to that file so save/load warm-miss verdicts are observable.
            log_file: std::env::var("ZCCACHE_EPHEMERAL_LOG")
                .ok()
                .map(|s| crate::core::NormalizedPath::from(std::path::PathBuf::from(s))),
            track_stats: false,
            journal_path: None,
            profile: false,
            private_daemon: None,
        },
    )
    .await;
    let session_id = match session_resp {
        Response::SessionStarted { session_id, .. } => session_id,
        Response::Error { message } => return Response::Error { message },
        other => {
            return Response::Error {
                message: format!("unexpected session start response: {other:?}"),
            };
        }
    };

    // 2. Compile — pass the compiler from the ephemeral request
    let result = handle_compile(state, &session_id, args, cwd, compiler, env, stdin).await;

    // 3. End session (best-effort, no response needed)
    if let Ok(sid) = session_id.parse::<SessionId>() {
        state.session_worktree_roots.remove(&sid);
        state.sessions.end(&sid);
    }

    result
}

/// Run the compiler directly without caching.
///
/// `tmp_dir` is where the synthesized Windows response file lands when the
/// command line exceeds the OS limit. Production callers pass the daemon's
/// `state.depfile_tmpdir` (under the cache root) so the contents are
/// covered by the wrapper's Defender exclusion — see issue #275.
#[allow(clippy::too_many_arguments)] // Mirrors handle_compile's surface — refactor parked.
pub(super) async fn run_compiler_direct(
    state: &SharedState,
    compiler: &NormalizedPath,
    args: &[String],
    cwd: &Path,
    sessions: &SessionManager,
    sid: &SessionId,
    client_env: &Option<Vec<(String, String)>>,
    stdin_bytes: &[u8],
    tmp_dir: &Path,
) -> Response {
    // Family hint controls response-file dialect (#634): rustc reads
    // response files line-by-line with no quote handling, so the
    // GCC-style single-quote wrapping mangles structured args like
    // `--check-cfg cfg(feature, values("a","b"))`. Detect from the
    // compiler path; `detect_family` falls back to Gcc for unknown
    // names, which matches the historical behaviour.
    let family_hint = crate::compiler::detect_family(&compiler.to_string_lossy());
    run_compiler_direct_with_family(
        state,
        compiler,
        args,
        cwd,
        sessions,
        sid,
        client_env,
        stdin_bytes,
        tmp_dir,
        family_hint,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_compiler_direct_with_family(
    state: &SharedState,
    compiler: &NormalizedPath,
    args: &[String],
    cwd: &Path,
    sessions: &SessionManager,
    sid: &SessionId,
    client_env: &Option<Vec<(String, String)>>,
    stdin_bytes: &[u8],
    tmp_dir: &Path,
    family_hint: crate::compiler::CompilerFamily,
) -> Response {
    let lineage = super::super::lineage::Lineage::current(
        sessions.get(sid).map(|s| s.client_pid),
        Some(sid.to_string()),
    );

    // Keep a private view alive through this uncached compiler invocation too.
    let _pch_overlay = if matches!(
        family_hint,
        crate::compiler::CompilerFamily::Gcc | crate::compiler::CompilerFamily::Clang
    ) {
        let force_includes = crate::depgraph::args::parse_gnu_args(args, cwd).force_includes;
        let root = compile_worktree_root(state, sid, cwd, client_env.as_deref())
            .unwrap_or_else(|| cwd.into());
        match super::handle_compile::pch_overlay::prepare_loaded_pch_overlay(
            state,
            sid,
            &force_includes,
            root.as_path(),
            client_env.as_deref(),
            args,
            crate::depgraph::args::parse_gnu_args(args, cwd)
                .output_file
                .as_ref()
                .is_some_and(|output| super::handle_compile::pch_gate::is_pch_output(output)),
        ) {
            Ok(view) => view,
            Err(refusal) => {
                return Response::Error {
                    message: format!("PCH input validation failed; rebuild the PCH: {refusal}"),
                }
            }
        }
    } else {
        None
    };
    let mut execution_args = args.to_vec();
    if let Some(view) = &_pch_overlay {
        view.append_args(&mut execution_args);
    }
    let args = execution_args.as_slice();
    let _rsp_guard = match crate::compiler::response_file::write_response_file_if_needed(
        args,
        tmp_dir,
        family_hint,
    ) {
        Ok(guard) => guard,
        Err(e) => {
            return Response::Error {
                message: format!("failed to write response file: {e}"),
            };
        }
    };

    let builder = super::compiler_spawn::user_compile_spawn(
        compiler.as_path(),
        cwd,
        args,
        _rsp_guard.as_ref(),
    );
    let builder = apply_client_env_builder(
        builder,
        client_env,
        &lineage,
        crate::compiler::is_dylint_driver(&compiler.to_string_lossy()),
    );
    let compiler_priority = CompilePriority::from_client_env(client_env.as_deref());
    let stdin = if stdin_bytes.is_empty() {
        None
    } else {
        Some(stdin_bytes)
    };
    let built_in_exclusive =
        super::compile_resource_gate::requires_exclusive_access_from_args(family_hint, args, cwd);
    let (exclusive, host_admission) = match super::compile_resource_gate::host_admission(
        state,
        family_hint,
        args,
        None,
        built_in_exclusive,
    )
    .await
    {
        Ok(exclusive) => exclusive,
        Err(error) => {
            return Response::Error {
                message: format!("host compiler admission failed: {error}"),
            };
        }
    };
    let (compiler_admission, _) =
        super::compile_resource_gate::acquire_compiler_admission(state, exclusive, host_admission)
            .await;
    if exclusive {
        tracing::info!(
            event = "compile_exclusive",
            "direct amalgamated compiler unit acquired exclusive build access"
        );
    }
    if let Some(view) = &_pch_overlay {
        if let Err(error) = super::handle_compile::pch_overlay_aliases::extend_overlay_aliases(
            view.path(),
            compiler,
            args,
            cwd,
            client_env,
            &lineage,
            (!stdin_bytes.is_empty()).then_some(stdin_bytes),
        )
        .await
        {
            // #2027: fail open. The view without alias entries is still the
            // drifted-input repair; this compile is uncached either way.
            super::compiler_spawn::log_internal_scan_failure("PCH include alias scan", &error);
        }
    }
    let (result, streamed_output) = if let Some(context) = crate::daemon::compile_output::current()
    {
        let (sender, receiver) = kernal_api::async_engine::channel(8);
        let process = super::super::process::async_builder_output_with_priority_stdin(
            builder,
            compiler_priority,
            stdin,
            Some(sender),
            compiler.display().to_string(),
            crate::daemon::process::ProcessLiveness::configured(),
        );
        let consume = crate::daemon::compile_output::consume(
            receiver,
            context,
            crate::daemon::compile_output::OutputFilter::None,
        );
        let (process_result, capture_result) = tokio::join!(process, consume);
        (process_result, Some(capture_result))
    } else {
        (
            super::super::process::async_builder_output_with_priority_stdin(
                builder,
                compiler_priority,
                stdin,
                None,
                compiler.display().to_string(),
                crate::daemon::process::ProcessLiveness::configured(),
            )
            .await,
            None,
        )
    };

    let _resource_admission = compiler_admission.release_compiler_slot();

    match result {
        Ok(mut output) => {
            if let Some(capture) = streamed_output {
                match capture {
                    Ok(capture) => {
                        output.stdout = capture.stdout;
                        output.stderr = capture.stderr;
                    }
                    Err(e) => {
                        return Response::Error {
                            message: format!("failed to stream compiler output: {e}"),
                        };
                    }
                }
            }
            let exit_code = crate::platform::process::exit::outcome(&output.status).exit_code;
            if exit_code == 0 {
                if let Some(output_path) =
                    crate::depgraph::args::parse_gnu_args(args, cwd).output_file
                {
                    if super::handle_compile::pch_gate::is_pch_output(&output_path) {
                        if let Err(error) =
                            super::handle_compile::pch_provenance::record_pch_imports_for_output(
                                output_path.as_path(),
                                output_path.as_path(),
                                _pch_overlay.as_ref().map(|view| view.path()),
                            )
                        {
                            return Response::Error {
                                message: format!("failed to record PCH import provenance: {error}"),
                            };
                        }
                    }
                }
            }
            write_session_log(sessions, sid, &format!("[DIRECT] exit_code={exit_code}"));
            Response::CompileResult {
                exit_code,
                stdout: Arc::new(output.stdout),
                stderr: Arc::new(output.stderr),
                cached: false,
            }
        }
        Err(e) => Response::Error {
            message: format!("failed to run compiler: {e}"),
        },
    }
}
