//! Per-unit cache lookup and input snapshot validation for multi-file requests.
use super::*;

pub(super) fn check_unit_cache(
    state: &SharedState,
    compilation: &crate::compiler::CacheableCompilation,
    check: UnitCacheCheck<'_>,
) -> UnitCacheResult {
    let UnitCacheCheck {
        cwd_path,
        key_root,
        system_includes,
        shared_base,
        shared_dep_flags,
        scan_cache,
        cache_now,
        dependency_mode,
        materialization_mode,
    } = check;
    let t0 = std::time::Instant::now();
    let snap_clock = state.cache_system.current_clock();
    state.stats.record_compilation();

    let source_path = if compilation.source_file.is_absolute() {
        compilation.source_file.clone()
    } else {
        cwd_path.join(&compilation.source_file).into()
    };
    let output_path = if compilation.output_file.is_absolute() {
        compilation.output_file.clone()
    } else {
        cwd_path.join(&compilation.output_file).into()
    };

    let shared_context = shared_base.is_some();
    let (mut ctx, dep_flags) = if let Some(base) = shared_base {
        let mut ctx = base.clone();
        ctx.source_file = source_path.clone();
        (ctx, shared_dep_flags.cloned().unwrap_or_default())
    } else {
        match build_compile_context(
            compilation,
            cwd_path,
            system_includes,
            &[],
            &state.compiler_hash_cache,
        ) {
            BuildContextResult::Cc { ctx, dep_flags } => (ctx, dep_flags),
            BuildContextResult::Rustc { compat_ctx, .. } => (compat_ctx, UserDepFlags::default()),
        }
    };
    if !shared_context {
        dependency_mode.apply_to_cc_context(&mut ctx, &dep_flags);
    }
    DependencyDiscoveryMode::apply_user_depfile_content_to_cc_context(
        &mut ctx,
        &dep_flags,
        &compilation.original_args,
        // Multi-source hits do not rehydrate a logical worktree root in
        // their depfiles, so their salt keeps the raw spelling.
        None,
    );
    let t_ctx = t0.elapsed();
    let source_mode_for_key = if matches!(
        compilation
            .output_file
            .as_path()
            .extension()
            .and_then(|e| e.to_str()),
        Some("pch") | Some("gch")
    ) {
        crate::compiler::SourceMode::Header
    } else {
        crate::compiler::SourceMode::Normal
    };
    let worktree_salt = if cc_requires_worktree_salt(
        compilation.family,
        source_mode_for_key,
        &compilation.original_args,
        key_root.as_path(),
    ) {
        Some(key_root.as_path())
    } else {
        None
    };
    let context_key = state.dep_graph.load().register_with_root_and_salt(
        ctx.clone(),
        Some(key_root.clone()),
        worktree_salt,
    );
    let t_register = t0.elapsed();

    // ── Ultra-fast path: per-file freshness skip ────────────────────
    if state.watcher_active.load(Ordering::Acquire) {
        let entry = owned_fast_hit_entry(&state.fast_hit_cache, &context_key);
        if let Some(entry) = entry {
            if cache_entry_fresh_at(cache_now, entry.cached_at, FAST_HIT_MAX_AGE)
                && context_files_fresh(state, &context_key, &source_path, entry.clock)
            {
                let artifact_key_hex = &entry.artifact_key_hex;
                // Write outputs directly from the resolved payload Arc — no
                // DashMap guard is held here (issue #1180's short-lock model):
                // `lookup_artifact_with_disk_fallback` clones the owned
                // artifact out of the map before this runs, and
                // the delivery lease borrows that owned clone rather than
                // a map shard. Each check_unit_cache
                // runs in its own blocking task, so writes are already
                // parallel across units.
                if let Some(cached) = lookup_artifact_with_disk_fallback(state, artifact_key_hex) {
                    match ensure_payloads_for_materialization_for_state(
                        state,
                        &cached,
                        artifact_key_hex,
                    ) {
                        Ok(payloads) => {
                            record_artifact_access(
                                state,
                                artifact_key_hex,
                                &cached,
                                std::time::Instant::now(),
                            );
                            let names = Arc::clone(&cached.meta.output_names);
                            let artifact_bytes: u64 = cached.meta.total_size;
                            let stdout = cached.stdout.clone();
                            let stderr = cached.stderr.clone();

                            let targets: Vec<NormalizedPath> = (0..payloads.len())
                                .map(|i| {
                                    let out: NormalizedPath = if i == 0 {
                                        output_path.clone()
                                    } else {
                                        cwd_path.join(&names[i]).into()
                                    };
                                    out
                                })
                                .collect();
                            payloads.record_staged_pre_materialization(&state.profiler.staged);
                            let materialization =
                                materialize_multi_hit(&targets, &payloads, materialization_mode);
                            payloads.record_staged_lock_timings(&state.profiler.staged);
                            drop(payloads);
                            if let Err(failure) = &materialization {
                                report_materialization_failure(
                                    &state.cache_dir,
                                    artifact_key_hex,
                                    "multi-fast-hit",
                                    failure,
                                );
                            }
                            if materialization.is_ok() {
                                state.stats.record_hit(0, artifact_bytes);
                                state.profiler.record_hit(&HitPhases {
                                    parse_args_ns: 0,
                                    build_context_ns: t_ctx.as_nanos() as u64,
                                    hash_source_ns: 0,
                                    hash_headers_ns: 0,
                                    depgraph_check_ns: 0,
                                    request_cache_lookup_ns: 0,
                                    cross_root_validate_ns: 0,
                                    artifact_lookup_ns: 0,
                                    write_output_ns: 0,
                                    bookkeeping_ns: 0,
                                    total_ns: t0.elapsed().as_nanos() as u64,
                                });
                                return UnitCacheResult::Hit {
                                    stdout,
                                    stderr,
                                    artifact_bytes,
                                    source_path,
                                };
                            }
                            // A caller-owned destination failure is a soft miss:
                            // recompile this TU but preserve the valid shared
                            // artifact for its siblings.  Only an absent blob is
                            // structural evidence that permits eviction.
                            if let Err(failure) = &materialization {
                                invalidate_multi_artifact_after_failure(
                                    state,
                                    artifact_key_hex,
                                    failure,
                                );
                            }
                            state.fast_hit_cache.remove(&context_key);
                        }
                        Err(failure) => {
                            report_materialization_failure(
                                &state.cache_dir,
                                artifact_key_hex,
                                "multi-fast-hit",
                                &failure,
                            );
                            invalidate_multi_artifact_after_failure(
                                state,
                                artifact_key_hex,
                                &failure,
                            );
                            state.fast_hit_cache.remove(&context_key);
                        }
                    }
                } else {
                    let failure = cache_blob_missing(
                        &state.artifact_dir.join(artifact_key_hex),
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "cached multi-source artifact metadata is unavailable",
                        ),
                    );
                    report_materialization_failure(
                        &state.cache_dir,
                        artifact_key_hex,
                        "multi-fast-hit",
                        &failure,
                    );
                    invalidate_multi_artifact_after_failure(state, artifact_key_hex, &failure);
                    state.fast_hit_cache.remove(&context_key);
                }
            }
        }
    }

    let source_hash = match hash_file(&state.cache_system, &source_path, snap_clock) {
        Ok(h) => h,
        Err(_) => {
            return UnitCacheResult::Miss {
                source_path,
                output_path,
                context_key,
                ctx: Box::new(ctx),
                input_snapshot: InputSnapshot::incomplete(snap_clock),
                reason: miss_reason::INPUT_FINGERPRINT_MISMATCH,
            };
        }
    };
    let t_hash_source = t0.elapsed();

    let mut hash_map: HashMap<NormalizedPath, ContentHash> = HashMap::new();
    hash_map.insert(source_path.clone(), source_hash);
    {
        use rayon::prelude::*;
        let includes = state.dep_graph.load().get_includes(&context_key);
        let include_iter = includes.iter().flat_map(|v| v.iter());
        let all_paths: Vec<&NormalizedPath> =
            include_iter.chain(ctx.force_includes.iter()).collect();
        let hashes: Vec<_> = all_paths
            .par_iter()
            .filter_map(|path| {
                hash_file(&state.cache_system, path, snap_clock)
                    .ok()
                    .map(|h| ((*path).clone(), h))
            })
            .collect();
        for (path, h) in hashes {
            hash_map.insert(path, h);
        }
    }
    let t_hash_headers = t0.elapsed();

    let verdict = {
        let is_fresh = |p: &Path| {
            let path = NormalizedPath::new(p);
            journal_proves_fresh(state.cache_system.journal(), &path, snap_clock)
        };
        let get_hash = |p: &Path| {
            let path = NormalizedPath::new(p);
            hash_map.get(&path).copied()
        };
        state
            .dep_graph
            .load()
            .check(&context_key, is_fresh, get_hash)
    };
    let t_depgraph = t0.elapsed();
    let mut miss_attribution = match &verdict {
        crate::depgraph::CacheVerdict::Hit { .. } => miss_reason::NO_ARTIFACT_FOR_KEY,
        crate::depgraph::CacheVerdict::SourceChanged { .. }
        | crate::depgraph::CacheVerdict::HeadersChanged { .. }
        | crate::depgraph::CacheVerdict::NeedsPreprocessor => {
            miss_reason::INPUT_FINGERPRINT_MISMATCH
        }
        crate::depgraph::CacheVerdict::Cold => miss_reason::CONTEXT_NOT_FOUND,
    };

    let depgraph_claimed_hit = matches!(verdict, crate::depgraph::CacheVerdict::Hit { .. });
    if let crate::depgraph::CacheVerdict::Hit { artifact_key }
    | crate::depgraph::CacheVerdict::SourceChanged { artifact_key } = verdict
    {
        let artifact_key_hex = artifact_key.hash().to_hex();
        if let Some(cached) = lookup_artifact_with_disk_fallback(state, &artifact_key_hex) {
            let t_lookup = t0.elapsed();
            match ensure_payloads_for_materialization_for_state(state, &cached, &artifact_key_hex) {
                Ok(payloads) => {
                    record_artifact_access(
                        state,
                        &artifact_key_hex,
                        &cached,
                        std::time::Instant::now(),
                    );
                    let names = Arc::clone(&cached.meta.output_names);
                    let artifact_bytes: u64 = cached.meta.total_size;
                    let stdout = cached.stdout.clone();
                    let stderr = cached.stderr.clone();

                    let targets: Vec<NormalizedPath> = (0..payloads.len())
                        .map(|i| {
                            let out: NormalizedPath = if i == 0 {
                                output_path.clone()
                            } else {
                                cwd_path.join(&names[i]).into()
                            };
                            out
                        })
                        .collect();
                    payloads.record_staged_pre_materialization(&state.profiler.staged);
                    let materialization =
                        materialize_multi_hit(&targets, &payloads, materialization_mode);
                    payloads.record_staged_lock_timings(&state.profiler.staged);
                    drop(payloads);
                    if let Err(failure) = &materialization {
                        report_materialization_failure(
                            &state.cache_dir,
                            &artifact_key_hex,
                            "multi-depgraph-hit",
                            failure,
                        );
                        miss_attribution = match failure {
                            MaterializationFailure::CacheBlobMissing(_) => {
                                miss_reason::NO_ARTIFACT_FOR_KEY
                            }
                            MaterializationFailure::CacheRead(_) => {
                                miss_reason::NO_ARTIFACT_FOR_KEY
                            }
                            MaterializationFailure::DestinationWrite(_) => {
                                miss_reason::DESTINATION_WRITE_FAILED
                            }
                        };
                    }
                    if depgraph_claimed_hit {
                        if let Err(failure) = &materialization {
                            invalidate_multi_artifact_after_failure(
                                state,
                                &artifact_key_hex,
                                failure,
                            );
                        }
                    }
                    if materialization.is_ok() {
                        state.stats.record_hit(0, artifact_bytes);

                        // Populate fast-hit cache for future requests
                        let tracked_paths =
                            request_cache_input_paths(state, &context_key, &source_path, &ctx);
                        state.cache_system.register_tracked(&tracked_paths);
                        let current_clock = state.cache_system.current_clock();
                        state.fast_hit_cache.insert(
                            context_key,
                            FastHitEntry {
                                clock: current_clock,
                                artifact_key_hex: artifact_key_hex.clone(),
                                cached_at: std::time::Instant::now(),
                            },
                        );

                        let total_ns = t0.elapsed().as_nanos() as u64;
                        state.profiler.record_hit(&HitPhases {
                            parse_args_ns: 0,
                            build_context_ns: t_ctx.as_nanos() as u64,
                            hash_source_ns: (t_hash_source - t_register).as_nanos() as u64,
                            hash_headers_ns: (t_hash_headers - t_hash_source).as_nanos() as u64,
                            depgraph_check_ns: (t_depgraph - t_hash_headers).as_nanos() as u64,
                            request_cache_lookup_ns: 0,
                            cross_root_validate_ns: 0,
                            artifact_lookup_ns: (t_lookup - t_depgraph).as_nanos() as u64,
                            write_output_ns: 0,
                            bookkeeping_ns: 0,
                            total_ns,
                        });

                        return UnitCacheResult::Hit {
                            stdout,
                            stderr,
                            artifact_bytes,
                            source_path,
                        };
                    }
                }
                Err(failure) => {
                    report_materialization_failure(
                        &state.cache_dir,
                        &artifact_key_hex,
                        "multi-depgraph-hit",
                        &failure,
                    );
                    miss_attribution = match &failure {
                        MaterializationFailure::DestinationWrite(_) => {
                            miss_reason::DESTINATION_WRITE_FAILED
                        }
                        MaterializationFailure::CacheBlobMissing(_)
                        | MaterializationFailure::CacheRead(_) => miss_reason::NO_ARTIFACT_FOR_KEY,
                    };
                    if depgraph_claimed_hit {
                        invalidate_multi_artifact_after_failure(state, &artifact_key_hex, &failure);
                    }
                }
            }
        } else if depgraph_claimed_hit {
            let failure = cache_blob_missing(
                &state.artifact_dir.join(artifact_key_hex.as_str()),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cached multi-source artifact metadata is unavailable",
                ),
            );
            report_materialization_failure(
                &state.cache_dir,
                artifact_key_hex.as_str(),
                "multi-depgraph-hit",
                &failure,
            );
            invalidate_multi_artifact_after_failure(state, artifact_key_hex.as_str(), &failure);
            miss_attribution = miss_reason::NO_ARTIFACT_FOR_KEY;
        }
    }

    state.fast_hit_cache.remove(&context_key);
    let input_snapshot = InputSnapshot::capture(
        state,
        &source_path,
        &ctx,
        hash_map,
        scan_cache,
        dependency_mode,
    );
    UnitCacheResult::Miss {
        source_path,
        output_path,
        context_key,
        ctx: Box::new(ctx),
        input_snapshot,
        reason: miss_attribution,
    }
}
