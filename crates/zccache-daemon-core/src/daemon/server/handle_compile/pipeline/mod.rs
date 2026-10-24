//! Compile request pipeline orchestrator.
//!
//! The per-request pipeline (parse, build context, hash, depgraph check,
//! hit-branch dispatch, miss exec, store) was originally a single 1.2k LOC
//! function. The implementation is now split per phase under this directory:
//!
//! - `request_prep.rs` — validation, Dylint/time-macro prep, invocation parse
//! - `system_includes.rs` — per-compiler system include discovery + watch
//! - `hash_verify.rs` — source + header hashing and depgraph verdict
//! - `compile_exec.rs` — depfile/response-file prep + compiler spawn
//! - `store_outcome.rs` — successful-compile post path (hash all, store, profiles)
//! - `store_outcome_scan.rs` — depfile/include scan collection and fallback policy
//!
//! This module is the orchestrator: it threads local timings + per-phase
//! results through the early-return tree and finally returns the `Response`.

mod compile_exec;
mod hash_verify;
mod request_prep;
mod store_outcome;
mod system_includes;
mod time_macros;

use super::super::*;
use super::cached_hit::{
    materialize_cached_compile_hit_offloaded, CachedHitFailure, CachedHitPhases,
    OwnedCachedHitMaterializeRequest,
};
use super::error_cache::maybe_store_rustc_error_artifact;
use super::hit_branches::{
    await_artifact_publication_if_needed, try_depgraph_cached_hit, try_fast_hit,
    try_request_cache_hit, DepgraphHitProbe, FastHitProbe, RequestCacheHitProbe,
};
use super::request::CompileRequest;

use compile_exec::{run_compile_exec, CompileExecOutcome, CompileExecRequest, CompileExecResult};
use hash_verify::{
    hash_and_verify, miss_reason_for_verdict, HashSourceOutcome, HashVerifyInput, HashVerifyOutcome,
};
use request_prep::{
    begin_prepared_request, discover_request_system_includes, invalidate_missing_depgraph_artifact,
    parse_single_compile_request, prepare_request_arguments, record_dylint_input_hash,
    wait_for_startup_depgraph_load, ParsedRequestArguments, ParsedSingleRequest,
};
use store_outcome::{store_successful_compile, StoreOutcomeRequest};
use system_includes::SystemIncludesOutcome;

mod request;
pub(super) use request::handle_compile_request;
