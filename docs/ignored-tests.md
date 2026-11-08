# Ignored-test selection audit (#2117)

Audited against main 8516b749, plus this change. 310 Rust ignore attributes;
comments and the README example are not tests. No tests were deleted.

The premise of #2117 is incomplete: integration.yml already runs a blanket
ignored suite on schedule and workflow_dispatch, with the integration binary
features. It excludes names containing bench_, perf_, profile_,
cold_path_stress_profile, cold_path_concurrent_stress, cold_path_first_file_penalty
and persist_pool_bench. A normal nextest run does not execute these tests.

The readonly Cargo acceptance now shares MSRV's existing zccache-bin build
with false_green_check. The local gate records that step and the legacy
container harness uses the same command. Attested PRs may skip MSRV after
local proof; main pushes always run it.

Selection is not proof of successful execution: platform cfgs, tool availability
and test-local early returns still apply. Scheduled integration is Linux;
the Windows-only cl tests have their own native job. Six other native-only
regressions (macOS dsymutil, MSVC LTCG, Windows response spilling and
clang-cl staging) remain manual because the blanket job cannot compile them.
Their ignore reasons name the tool/OS prerequisite and exact invocation.
Reflink tests require
the dedicated btrfs job's filesystem variables. Corpus scanning gets
ZCCACHE_SCAN_CORPUS_DIR=/usr/include in the scheduled job. Helper-process
tests are intentionally inert unless their nonignored parent plants them.

Manual-onlys keep their ignore attribute with a reason and exact
Soldr invocation. Emscripten diagnostics require emsdk; other timing/profile
probes need a quiet machine. Timing probes are diagnostic only: PERF.md's local Docker
matrix remains the authoritative performance gate. No perf behavior or
threshold changes are part of this audit.

## Selection totals

| Selection | Tests |
|---|---:|
| Scheduled integration | 248 |
| Integration (parent-spawned helper) | 2 |
| Miss overhead | 2 |
| Manual-only | 23 |
| Integration (push + scheduled) | 2 |
| Integration (termination proof) | 2 |
| MSVC cl cache (Windows) | 4 |
| MSRV (Cargo cache batch) | 2 |
| MSRV (nested Dylint) | 6 |
| Dylint (argfile regressions) | 3 |
| Reflink e2e (btrfs) | 2 |
| Integration (wrapper contract) | 5 |
| Perf Guard | 9 |

## Complete inventory

Paths are relative to the repository root. Each row names one ignored test.

| Source | Test | Selection |
|---|---|---|
| crates/zccache-artifact/src/kv/tests.rs | i6_too_large_rejected | Scheduled integration |
| crates/zccache-cli-core/src/cli/commands/tests/daemon.rs | ensure_daemon_auto_recovers_on_comm_error | Scheduled integration |
| crates/zccache-cli-core/src/cli/commands/tests/daemon.rs | ensure_daemon_is_bounded_when_status_probe_never_answers | Scheduled integration |
| crates/zccache-cli-core/src/cli/runtime/unreachable_tests.rs | pid_holder_process | Integration (parent-spawned helper) |
| crates/zccache-cli-core/src/download_client/liveness_tests.rs | download_pid_holder_process | Integration (parent-spawned helper) |
| crates/zccache-daemon-core/src/daemon/server/tests/clear_handler.rs | handle_clear_preserves_system_includes | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/exec_probe.rs | probe_miss_then_store_then_probe_hit | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/exec_probe.rs | stored_bytes_survive_daemon_restart | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/miss_overhead_budget.rs | miss_overhead_budget_depfile | Miss overhead |
| crates/zccache-daemon-core/src/daemon/server/tests/miss_overhead_budget.rs | miss_overhead_budget_include_scan | Miss overhead |
| crates/zccache-daemon-core/src/daemon/server/tests/release_worktree_handles.rs | release_drops_sessions_under_path_and_leaves_siblings | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/release_worktree_handles.rs | release_refuses_to_release_cache_root | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/release_worktree_handles.rs | release_with_no_matches_returns_zero_counts | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | cli_clear_resets_cache | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | cli_compile_unknown_uuid_is_idempotent | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | cli_multi_file_compilation_runs_directly | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | cli_session_lifecycle | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | perf_shutdown_request_exits_idle_daemon_promptly | Manual-only |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | private_session_start_accepts_top_level_cache_root | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | private_session_start_registers_redacted_status_and_owner_refs | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | staged_publication_fault_salvages_or_fails_closed_with_forensics | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | test_server_clear_empty | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | test_server_ping_pong | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | test_server_shutdown_request | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | test_server_status | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/server_ipc.rs | test_server_status_reports_explicit_daemon_namespace | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/session_errors.rs | cli_session_end_invalid_id | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/session_errors.rs | cli_session_end_unknown_uuid_is_idempotent | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_empp_multi_source_hits_after_include_heavy_cold_compile | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_multi_source_clang_cl_restores_directory_outputs_from_response_file | Manual-only |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_multi_source_compiler_failure_publishes_and_materializes_nothing | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_multi_source_hits_after_clear_and_include_heavy_cold_compile | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_multi_source_materialization_failure_reports_error_before_index_visibility | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | staged_multi_source_publication_failure_salvages_and_retries_one_unit | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | unsupported_mode_detaches_default_depfiles_from_prior_staged_outputs | Scheduled integration |
| crates/zccache-daemon-core/src/daemon/server/tests/staged_compiler_sets.rs | unsupported_shared_depfile_always_runs_original_batch_and_detaches_outputs | Scheduled integration |
| crates/zccache-daemon-core/src/embedded/tests.rs | staged_custom_depfile_survives_flush_shutdown_and_restart_hit | Scheduled integration |
| crates/zccache-daemon-core/tests/legacy_path_validation.rs | legacy_multi_source_hits_warm_after_restart | Integration (push + scheduled) |
| crates/zccache-daemon-core/tests/legacy_path_validation.rs | strict_layout_validation_aggregates_all_runtime_flows | Integration (push + scheduled) |
| crates/zccache-depgraph/src/scanner/lex_tests.rs | matches_legacy_on_corpus_dir | Scheduled integration |
| crates/zccache-test-support/tests/test_child.rs | planted_hang_is_terminated_by_name | Integration (termination proof) |
| crates/zccache-test-support/tests/test_child.rs | planted_unrelated_test_still_passes | Integration (termination proof) |
| crates/zccache/tests/cache_native/daemon_burst_link_test.rs | burst_compile_does_not_wedge_daemon | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_dll_cache_test.rs | test_clang_link_cache_miss_then_hit | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_dll_cache_test.rs | test_dll_cache_invalidated_on_input_change | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_dll_cache_test.rs | test_dll_cache_miss_then_hit | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_dll_cache_test.rs | test_dll_non_deterministic_warning | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_dll_cache_test.rs | test_exe_cache_miss_then_hit | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_ar_cache_invalidated_on_input_change | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_ar_cache_miss_then_hit | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_ar_non_cacheable_passthrough | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_ar_non_deterministic_warning | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_link_hit_restores_explicit_map_destination | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_link_path_remap_auto_hits_across_sibling_git_roots | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_link_cache_test.rs | test_link_stats_in_status | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_msvc_cl_cache_test.rs | msvc_cl_caller_supplied_show_includes_survives_a_cache_hit | MSVC cl cache (Windows) |
| crates/zccache/tests/cache_native/daemon_msvc_cl_cache_test.rs | msvc_cl_cmake_fd_flag_compile_is_cacheable | MSVC cl cache (Windows) |
| crates/zccache/tests/cache_native/daemon_msvc_cl_cache_test.rs | msvc_cl_header_edit_invalidates_the_cached_object | MSVC cl cache (Windows) |
| crates/zccache/tests/cache_native/daemon_msvc_cl_cache_test.rs | msvc_cl_second_identical_compile_is_a_cache_hit | MSVC cl cache (Windows) |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | bench_medium_project_warm_iterations | Manual-only |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_clear_forces_cold_rebuild | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_cold_then_warm_rebuild_cli | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_cold_then_warm_rebuild_ipc | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_concurrent_cold_build | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_header_change_invalidates_dependents | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | ninja_persistent_artifacts_survive_restart | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_direct_test.rs | stress_large_project_cold_warm | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_meson_test.rs | meson_ninja_bench_warm_iterations | Manual-only |
| crates/zccache/tests/cache_native/daemon_ninja_rebuild_meson_test.rs | meson_ninja_cold_then_warm_rebuild | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_basic_test.rs | pch_generation_is_cacheable | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_basic_test.rs | pch_usage_is_cacheable | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_invalidation_test.rs | pch_build_dir_separation_sub_header_change | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_invalidation_test.rs | pch_chained_sub_header_change | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_invalidation_test.rs | pch_rebuild_no_spurious_output_in_source_tree | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_cache_invalidation_test.rs | pch_sub_header_change_invalidates_cache | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_gcc_implicit_test.rs | gcc_cmake_shaped_pch_consumer_hits_after_daemon_restart | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_pch_gcc_implicit_test.rs | gcc_implicit_pch_sub_header_change_misses | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | all_args_in_rsp | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | circular_rsp_falls_back | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | daemon_spill_rsp_preserves_compile_success | Manual-only |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | daemon_spill_rsp_preserves_fbuild_style_args | Manual-only |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | daemon_spill_rsp_preserves_multi_file_partial_hits | Manual-only |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | empty_rsp_cacheable | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | missing_rsp_falls_back | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | multiple_rsp_files | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | nested_rsp_deep_chain | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | nested_rsp_inner_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_added_flag_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_content_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_define_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_include_path_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_removed_flag_invalidates | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_vs_inline_equivalent | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_whitespace_irrelevant | Scheduled integration |
| crates/zccache/tests/cache_native/daemon_response_file_cache.rs | rsp_with_quoted_args | Scheduled integration |
| crates/zccache/tests/cache_native/link_bundle_integration_test.rs | real_dsymutil_bundle_miss_delete_hit | Manual-only |
| crates/zccache/tests/cache_native/link_bundle_integration_test.rs | real_lld_semantic_outputs_miss_delete_hit | Scheduled integration |
| crates/zccache/tests/cache_native/link_bundle_integration_test.rs | real_msvc_ltcg_output_miss_delete_hit | Manual-only |
| crates/zccache/tests/cache_rust/cow_mediated_contracts.rs | cc_and_cxx_preserve_argv_exit_and_streams | Scheduled integration |
| crates/zccache/tests/cache_rust/cow_readonly_cargo.rs | readonly_cache_hit_survives_cargo_clean | MSRV (Cargo cache batch) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | dylint_metadata_compat_hit_uses_current_cargo_destination | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | nested_dylint_hits_across_sibling_worktrees | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | nested_dylint_hits_and_invalidates_every_external_input | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | perf_dylint_library_cdylib_restores_primary_and_toolchain_sidecar | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | perf_dylint_tests_tree_cdylib_restores_primary_and_toolchain_sidecar | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_dylint_cache_test.rs | plain_and_dylint_share_artifact_bytes_but_not_verdicts | MSRV (nested Dylint) |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_concurrency_test.rs | rustc_cache_persists_but_extern_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_concurrency_test.rs | rustc_concurrent_different_externs | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_concurrency_test.rs | rustc_missing_extern_not_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_concurrency_test.rs | rustc_thundering_herd_same_file | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_cache_persists_across_sessions | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_env_vars_affect_cache_key | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_failed_compile_not_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_multiple_cargo_env_vars | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_non_cargo_env_dep_affects_cache_key | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_remap_path_prefix_affects_cache_key | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_corner_cases_test.rs | rustc_z_flag_with_value_in_cache_key | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_add_new_file_no_interference | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_cfg_flag_toggle | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_delete_recreate_same_content | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_edition_change | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_opt_level_change | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_adversarial_mutations_test.rs | rustc_mutation_touch_source_no_invalidation | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_packed_linux_dwarf_sidecar_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_check_metadata_hits_build_metadata_link | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_different_source_different_artifact | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_emit_metadata_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_explicit_emit_paths_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_extern_change_invalidates | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_lib_compile_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_basic_test.rs | test_rustc_multi_output_cached | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_path_remap_test.rs | test_rustc_include_only_out_dir_hits_across_sibling_git_roots | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_path_remap_test.rs | test_rustc_path_remap_auto_file_macro_hits_across_sibling_git_roots | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_path_remap_test.rs | test_rustc_path_remap_conservative_cross_root_misses | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_worktree_test.rs | test_rustc_git_worktrees_share_with_different_cargo_target_dir_shapes | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_cache_worktree_test.rs | test_rustc_sibling_git_worktree_equivalent_cache_sharing | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_issue_210_async_populate_test.rs | issue_210_build_and_check_warm_hits_are_preserved | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_issue_210_async_populate_test.rs | issue_210_clear_leaves_no_stale_rust_artifacts | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_issue_210_async_populate_test.rs | issue_210_same_context_immediate_recompile_hits_after_miss | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_issue_210_async_populate_test.rs | issue_210_shutdown_after_just_populated_rust_artifact | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_issue_210_async_populate_test.rs | issue_210_status_and_list_reflect_populated_rust_artifacts | Scheduled integration |
| crates/zccache/tests/cache_rust/daemon_rustc_response_file_test.rs | test_rustc_argfile_does_not_expand_nested_at_during_execution | Dylint (argfile regressions) |
| crates/zccache/tests/cache_rust/daemon_rustc_response_file_test.rs | test_rustc_cargo_argfile_compiles_and_caches | Dylint (argfile regressions) |
| crates/zccache/tests/cache_rust/daemon_rustc_response_file_test.rs | test_rustc_literal_at_search_path_caches_but_raw_at_is_rejected | Dylint (argfile regressions) |
| crates/zccache/tests/cache_rust/false_green_check.rs | edit_after_test_build_fails_check_like_uncached_control | MSRV (Cargo cache batch) |
| crates/zccache/tests/cache_rust/reflink_worktree_e2e.rs | reflink_e2e_cross_device_falls_back_to_copy | Reflink e2e (btrfs) |
| crates/zccache/tests/cache_rust/reflink_worktree_e2e.rs | reflink_e2e_parent_child_worktrees_per_mode | Reflink e2e (btrfs) |
| crates/zccache/tests/cli/cli_daemon_start.rs | concurrent_starts_all_complete | Scheduled integration |
| crates/zccache/tests/cli/cli_daemon_start.rs | start_completes_with_captured_pipes | Scheduled integration |
| crates/zccache/tests/cli/cli_daemon_start.rs | stop_kills_locked_process_when_ipc_is_unreachable | Integration (wrapper contract) |
| crates/zccache/tests/cli/cli_daemon_start.rs | stop_refuses_to_kill_unverified_locked_process_when_ipc_is_unreachable | Integration (wrapper contract) |
| crates/zccache/tests/cli/cli_kv.rs | kv_clear_namespace_empties_ls | Scheduled integration |
| crates/zccache/tests/cli/cli_kv.rs | kv_ls_lists_keys | Scheduled integration |
| crates/zccache/tests/cli/cli_kv.rs | kv_put_then_get_round_trips_binary | Scheduled integration |
| crates/zccache/tests/cli/cli_kv.rs | kv_put_value_from_stdin | Scheduled integration |
| crates/zccache/tests/cli/cli_kv.rs | kv_rm_then_get_exits_two | Scheduled integration |
| crates/zccache/tests/cli/cli_kv.rs | kv_stats_reports_nonzero_total_after_put | Scheduled integration |
| crates/zccache/tests/cli/cli_materialization_mode.rs | invalid_zccache_mode_fails_before_dispatch | Scheduled integration |
| crates/zccache/tests/cli/cli_probe_bypass_cwd.rs | probe_bypass_child_runs_in_the_caller_cwd | Scheduled integration |
| crates/zccache/tests/cli/cli_probe_bypass_cwd.rs | probe_bypass_recognises_a_relative_source | Scheduled integration |
| crates/zccache/tests/cli/cli_rust_plan_lifecycle.rs | rust_plan_lifecycle_keeps_path_dep_fresh_and_rebuilds_workspace_crate | Scheduled integration |
| crates/zccache/tests/cli/cli_session_end.rs | session_end_with_unreachable_daemon_is_idempotent | Scheduled integration |
| crates/zccache/tests/cli/cli_single_daemon_per_session.rs | n_serial_wrappers_share_one_daemon | Scheduled integration |
| crates/zccache/tests/cli/cli_wrapper_failure_boundaries.rs | a_fresh_cache_root_never_needs_its_deploy_dir_tightened | Integration (wrapper contract) |
| crates/zccache/tests/cli/cli_wrapper_failure_boundaries.rs | ephemeral_pre_dispatch_failure_is_a_hard_error_with_the_infra_exit_code | Integration (wrapper contract) |
| crates/zccache/tests/cli/cli_wrapper_failure_boundaries.rs | session_pre_dispatch_failure_refuses_without_double_reading_stdin | Integration (wrapper contract) |
| crates/zccache/tests/cli/cli_wrapper_passthrough.rs | wrapper_passthrough_ephemeral_exit_nonzero | Scheduled integration |
| crates/zccache/tests/cli/cli_wrapper_passthrough.rs | wrapper_passthrough_ephemeral_exit_zero | Scheduled integration |
| crates/zccache/tests/cli/cli_wrapper_passthrough.rs | wrapper_passthrough_session_path | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_cli_flow_test.rs | cli_binary_compiler_override_cpp_session_c_file | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_cli_flow_test.rs | cli_binary_ephemeral_session | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_cli_flow_test.rs | cli_binary_session_round_trip | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_crash_minidump_test.rs | stack_overflow_writes_signal_dump | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_endpoint_scoped_lock_test.rs | start_reports_failure_when_another_daemon_holds_the_cache_root | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_endpoint_scoped_lock_test.rs | stop_only_terminates_the_daemon_at_its_own_endpoint | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_endpoint_scoped_lock_test.rs | two_endpoints_keep_independent_lock_files | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_client_connects_and_pings_daemon | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_compile_hello_cpp_cached_with_forwarded_depfile | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_full_client_flow | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_multiple_clients_concurrent | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_session_start_with_clang_toolchain | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_integration_test.rs | test_session_start_with_nonexistent_compiler | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_session_stats_test.rs | session_stats_mid_session_query | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_session_stats_test.rs | session_stats_not_tracked_returns_none | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_session_stats_test.rs | session_stats_unknown_session | Scheduled integration |
| crates/zccache/tests/daemon_lifecycle/daemon_spawn_storm_test.rs | parallel_wrappers_must_share_one_daemon | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_persistence_test.rs | fresh_daemon_reports_not_persisted | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_persistence_test.rs | preloaded_graph_reports_persisted | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_persistence_test.rs | shutdown_save_restore_roundtrip | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_warm_classify_test.rs | corrupt_depgraph_emits_warning_in_session_log | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_warm_classify_test.rs | loaded_graph_with_missing_artifact_is_still_warm | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_warm_classify_test.rs | missing_depgraph_emits_no_warning | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_warm_classify_test.rs | valid_depgraph_makes_session_warm | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_depgraph_warm_classify_test.rs | version_mismatch_depgraph_emits_warning_in_session_log | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_fingerprint_test.rs | test_fingerprint_check_miss_then_skip | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_fingerprint_test.rs | test_fingerprint_invalidate | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_fingerprint_test.rs | test_fingerprint_mark_failure_forces_rerun | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/daemon_fingerprint_test.rs | test_fingerprint_two_watches_independent | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_basic | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_computed_include | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_nested_includes | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_parity_with_scanner | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_strategy_injected_works_end_to_end | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_depfile_integration_test.rs | depfile_system_headers_with_md | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_all_includes_unresolved | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_circular_includes | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_computed_include_propagates | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_deep_include_chain | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_deterministic_artifact_keys | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_empty_compile_commands | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_no_includes | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_rapid_state_transitions | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_shared_header_many_contexts | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_trim_cleans_orphaned_files | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_adversarial_test.rs | adversarial_watch_set_large_graph | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_concurrent_test.rs | stress_concurrent_register_and_trim | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_concurrent_test.rs | stress_concurrent_register_scan_check | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_concurrent_test.rs | stress_concurrent_session_operations | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_concurrent_test.rs | stress_concurrent_shadow_detection | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_integration_test.rs | integration_full_build_pipeline | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_integration_test.rs | integration_ingest_and_scan | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_integration_test.rs | integration_new_resolve_real_files | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_integration_test.rs | integration_session_lifecycle | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/depgraph_stress_integration_test.rs | integration_shadow_detection_real_files | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_concurrent.rs | parallel_check_mark_interleave | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_concurrent.rs | parallel_check_same_hash_cache | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_concurrent.rs | parallel_invalidate_while_checking | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_concurrent.rs | parallel_two_layer_check | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | large_file_set_hash_cache | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | large_file_set_two_layer | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | large_files_hash_cache | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | many_glob_patterns | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | mixed_modifications_many_cycles | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | rapid_cycles_hash_cache | Scheduled integration |
| crates/zccache/tests/depgraph_fingerprint/fingerprint_stress.rs | rapid_cycles_two_layer | Scheduled integration |
| crates/zccache/tests/perf_bench/tests_c.rs | perf_c_zccache_vs_bare | Perf Guard |
| crates/zccache/tests/perf_bench/tests_cpp.rs | perf_warm_cache_zccache_vs_sccache | Perf Guard |
| crates/zccache/tests/perf_bench/tests_cpp.rs | profile_single_cpp_cold_miss | Manual-only |
| crates/zccache/tests/perf_bench/tests_emcc.rs | perf_emcc_sibling_remap_warm | Manual-only |
| crates/zccache/tests/perf_bench/tests_emcc.rs | perf_emcc_warm_cache_zccache_vs_sccache | Manual-only |
| crates/zccache/tests/perf_bench/tests_link.rs | perf_c_archive_link | Perf Guard |
| crates/zccache/tests/perf_bench/tests_link.rs | perf_cpp_driver_link | Perf Guard |
| crates/zccache/tests/perf_bench/tests_link.rs | perf_emcc_link | Manual-only |
| crates/zccache/tests/perf_bench/tests_link.rs | perf_rust_workspace_link | Perf Guard |
| crates/zccache/tests/perf_bench/tests_probe_storm_wrapper.rs | perf_meson_probe_storm_wrapper | Manual-only |
| crates/zccache/tests/perf_bench/tests_probe_storm.rs | perf_meson_probe_storm | Manual-only |
| crates/zccache/tests/perf_bench/tests_response_file.rs | perf_response_file | Perf Guard |
| crates/zccache/tests/perf_bench/tests_rust.rs | perf_rustc_zccache_vs_sccache | Perf Guard |
| crates/zccache/tests/perf_bench/tests_sibling_remap.rs | perf_cpp_sibling_remap_warm | Perf Guard |
| crates/zccache/tests/perf_bench/tests_sibling_remap.rs | perf_rustc_sibling_remap_warm | Perf Guard |
| crates/zccache/tests/perf/daemon_cold_path_profile_test.rs | cold_path_concurrent_stress | Manual-only |
| crates/zccache/tests/perf/daemon_cold_path_profile_test.rs | cold_path_first_file_penalty | Manual-only |
| crates/zccache/tests/perf/daemon_cold_path_profile_test.rs | cold_path_stress_profile | Manual-only |
| crates/zccache/tests/perf/daemon_perf_test.rs | perf_full_benchmark | Manual-only |
| crates/zccache/tests/perf/daemon_perf_test.rs | perf_sanity_check | Manual-only |
| crates/zccache/tests/perf/daemon_persist_pool_bench.rs | persist_pool_bench | Manual-only |
| crates/zccache/tests/perf/daemon_profile_multi_test.rs | profile_multi_file_warm_path | Manual-only |
| crates/zccache/tests/perf/daemon_profile_test.rs | profile_compile_phases | Manual-only |
| crates/zccache/tests/stress/artifact_kv_stress.rs | c1_thundering_herd_same_key | Scheduled integration |
| crates/zccache/tests/stress/artifact_kv_stress.rs | c2_distinct_key_parallel_writers | Scheduled integration |
| crates/zccache/tests/stress/artifact_kv_stress.rs | c3_reader_writer_race | Scheduled integration |
| crates/zccache/tests/stress/artifact_kv_stress.rs | c5_clear_while_writes_keeps_consistency | Scheduled integration |
| crates/zccache/tests/stress/artifact_kv_stress.rs | d3_repeated_open_close | Scheduled integration |
| crates/zccache/tests/stress/artifact_kv_stress.rs | i5_value_at_max_accepted | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_corner_cases.rs | corner_cache_survives_session_restart | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_corner_cases.rs | corner_failed_compile_not_cached_then_header_appears | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_corner_cases.rs | corner_syntax_error_not_cached | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_corner_cases.rs | corner_thundering_herd_all_warm | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_corner_cases.rs | corner_thundering_herd_same_file | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_add_new_file_no_interference | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_cpp_system_header_forces_miss | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_define_flag_toggle | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_delete_recreate_same_content | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_include_path_creates_different_cache_entry | Scheduled integration |
| crates/zccache/tests/stress/daemon_adversarial_mutations.rs | mutation_touch_source_no_invalidation | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_compiler_test.rs | compiler_override_uses_wrapped_compiler | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_compile_errors_never_cached | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_concurrent_different_files | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_concurrent_same_file | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_cross_session_cache_sharing | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_define_changes_invalidate_cache | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_correctness_test.rs | adversarial_different_flags_different_cache_entries | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_empty_source_file | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_invalid_session_id | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_non_cacheable_passthrough | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_output_path_does_not_affect_cache_key | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_rapid_recompile_cycle | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_spaces_in_filename | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_warnings_still_cached | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_werror_vs_no_werror | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | adversarial_workspace_rename_hits_cache | Scheduled integration |
| crates/zccache/tests/stress/daemon_stress_edges_test.rs | path_remap_auto_compiles_file_macro_stably_across_git_roots | Scheduled integration |
| crates/zccache/tests/stress/daemon_watcher_adversarial.rs | deeply_nested_dir_watched | Scheduled integration |
| crates/zccache/tests/stress/daemon_watcher_adversarial.rs | rapid_create_delete_no_crash | Scheduled integration |
| crates/zccache/tests/stress/daemon_watcher_adversarial.rs | two_sessions_same_dir_share_watcher | Scheduled integration |
| crates/zccache/tests/stress/daemon_watcher_adversarial.rs | watcher_header_edit_detected | Scheduled integration |
| crates/zccache/tests/stress/daemon_watcher_adversarial.rs | watcher_touch_same_content_hits | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_concurrent_writers_and_readers | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_create_and_immediate_delete | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_empty_and_binary_files | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_many_apply_overflow_cycles | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_rapid_file_mutations | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_rename_chain | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | adversarial_same_size_different_content | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | integration_full_pipeline | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | stress_cache_system_many_files | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | stress_concurrent_lookups_during_changes | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | stress_journal_overflow_recovery | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | stress_rapid_apply_changes | Scheduled integration |
| crates/zccache/tests/stress/watcher_stress_test.rs | stress_settle_buffer_high_throughput | Scheduled integration |
