#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

dylint_linting::dylint_library!();

use rustc_errors::DiagDecorator;
use rustc_hir::def::Res;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

rustc_session::declare_lint! {
    /// Raw waits and locks must go through the bounded product facades.
    pub BAN_UNBOUNDED_SYNC,
    Deny,
    "use zccache_core bounded synchronization/wait facades"
}
rustc_session::declare_lint! {
    /// Protected-data guards must not survive an async suspension.
    pub BAN_GUARD_ACROSS_AWAIT,
    Deny,
    "a data lock guard is held across await; use sync::State's synchronous callback"
}
struct BoundedSync;
rustc_session::impl_lint_pass!(BoundedSync => [BAN_UNBOUNDED_SYNC, BAN_GUARD_ACROSS_AWAIT]);
#[unsafe(no_mangle)]
pub fn register_lints(_: &rustc_session::Session, store: &mut rustc_lint::LintStore) {
    store.register_lints(&[BAN_UNBOUNDED_SYNC, BAN_GUARD_ACROSS_AWAIT]);
    store.register_late_lint_pass(Box::new(|_| Box::new(BoundedSync)));
}

/// `(crate, type, method)` triples. The crate is the first def-path segment;
/// the type and method are the last two, so private module paths inside the
/// crate (`std::sync::poison::mutex::Mutex::lock`) still match.
const BANNED: &[(&str, &str, &str)] = &[
    ("std", "Mutex", "new"),
    ("std", "Mutex", "lock"),
    ("std", "Mutex", "try_lock"),
    ("std", "RwLock", "new"),
    ("std", "RwLock", "try_read"),
    ("std", "RwLock", "try_write"),
    ("std", "RwLock", "read"),
    ("std", "RwLock", "write"),
    ("std", "Condvar", "wait"),
    ("std", "Condvar", "wait_while"),
    ("tokio", "Mutex", "try_lock"),
    ("tokio", "Mutex", "try_lock_owned"),
    ("tokio", "Mutex", "new"),
    ("tokio", "Mutex", "const_new"),
    ("tokio", "Mutex", "lock_owned"),
    ("tokio", "Mutex", "lock"),
    ("tokio", "Mutex", "blocking_lock"),
    ("tokio", "RwLock", "try_read"),
    ("tokio", "RwLock", "try_write"),
    ("tokio", "RwLock", "try_read_owned"),
    ("tokio", "RwLock", "try_write_owned"),
    ("tokio", "RwLock", "new"),
    ("tokio", "RwLock", "const_new"),
    ("tokio", "RwLock", "read_owned"),
    ("tokio", "RwLock", "write_owned"),
    ("tokio", "RwLock", "read"),
    ("tokio", "RwLock", "write"),
    ("tokio", "RwLock", "blocking_read"),
    ("tokio", "RwLock", "blocking_write"),
    ("kernal_api", "Mutex", "new"),
    ("kernal_api", "Mutex", "lock"),
    ("kernal_api", "Mutex", "lock_owned"),
    ("kernal_api", "Mutex", "blocking_lock"),
    ("kernal_api", "Mutex", "try_lock"),
    ("kernal_api", "Mutex", "try_lock_owned"),
    ("kernal_api", "RwLock", "new"),
    ("kernal_api", "RwLock", "read"),
    ("kernal_api", "RwLock", "write"),
    ("kernal_api", "RwLock", "read_owned"),
    ("kernal_api", "RwLock", "write_owned"),
    ("kernal_api", "RwLock", "blocking_read"),
    ("kernal_api", "RwLock", "blocking_write"),
    ("lock_api", "Mutex", "lock"),
    ("lock_api", "RwLock", "read"),
    ("lock_api", "RwLock", "write"),
];

/// Raw blocking waits with no bound, each with its timeout-only replacement in
/// `zccache_core::wait` (or soldr-core's bounded command runners).
/// `(crate, type-or-module, method, replacement)`.
const BANNED_WAITS: &[(&str, &str, &str, &str)] = &[
    ("std", "Receiver", "recv", "wait::recv_for"),
    ("std", "SyncSender", "send", "wait::send_for"),
    ("std", "JoinHandle", "join", "wait::join_for"),
    ("std", "Child", "wait", "wait::child_for"),
    (
        "std",
        "Child",
        "wait_with_output",
        "wait::output_for (capture when spawning, with a mandatory budget)",
    ),
    ("std", "Command", "output", "wait::output_for"),
    ("std", "Command", "status", "wait::output_for"),
    ("std", "File", "lock", "wait::lock_exclusive_for"),
    ("std", "File", "lock_shared", "wait::lock_shared_for"),
    (
        "fs2",
        "FileExt",
        "lock_exclusive",
        "wait::lock_exclusive_for",
    ),
    ("fs2", "FileExt", "lock_shared", "wait::lock_shared_for"),
    (
        "std",
        "Barrier",
        "wait",
        "a timeout-bounded channel rendezvous",
    ),
    ("tokio", "Runtime", "block_on", "wait::block_on_for"),
    ("tokio", "Handle", "block_on", "wait::block_on_for"),
    (
        "futures_executor",
        "local_pool",
        "block_on",
        "wait::block_on_for",
    ),
    (
        "tokio",
        "Receiver",
        "blocking_recv",
        "wait::recv_for on a std channel",
    ),
    (
        "tokio",
        "Sender",
        "blocking_send",
        "wait::send_for on a std channel",
    ),
];

impl<'tcx> LateLintPass<'tcx> for BoundedSync {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !span_is_in_scope(cx, expr.span) {
            return;
        }
        check_coroutine(cx, expr);
        let def_id = match expr.kind {
            ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
            // UFCS calls and function items resolve through a path; matching
            // only method-call syntax would be bypassable mechanically.
            ExprKind::Path(qpath) => match cx.qpath_res(&qpath, expr.hir_id) {
                Res::Def(_, def_id) => Some(def_id),
                _ => None,
            },
            _ => None,
        };
        let Some(def_id) = def_id else {
            return;
        };
        let path = cx.get_def_path(def_id);
        if path.first() == Some(&Symbol::intern("kernal_api"))
            && path
                .last()
                .is_some_and(|name| name.as_str() == "run_bounded_command")
        {
            emit(cx, expr.span, BAN_UNBOUNDED_SYNC,
                "optional-timeout command runner bypasses the mandatory budget; use zccache_core::wait::output_for".into());
            return;
        }
        if path.first() == Some(&Symbol::intern("kernal_api"))
            && matches!(
                path.last().map(Symbol::as_str),
                Some(
                    "lock_shared" | "lock_exclusive" | "lock_shared_owned" | "lock_exclusive_owned"
                )
            )
        {
            emit(cx, expr.span, BAN_UNBOUNDED_SYNC, "unbounded file lock bypasses zccache_core::wait; pass a mandatory budget to the facade".into());
            return;
        }
        let async_body = cx
            .tcx
            .coroutine_kind(cx.tcx.hir_enclosing_body_owner(expr.hir_id))
            .is_some();
        let name = path.last().map(Symbol::as_str);
        let blocking_facade = path.first() == Some(&Symbol::intern("zccache_core"))
            && ((path.iter().any(|part| part.as_str() == "wait")
                && matches!(
                    name,
                    Some(
                        "output_for"
                            | "send_for"
                            | "child_for"
                            | "recv_for"
                            | "join_for"
                            | "block_on_for"
                            | "lock_shared_for"
                            | "lock_exclusive_for"
                            | "lock_shared_owned_for"
                            | "lock_exclusive_owned_for"
                    )
                ))
                || matches!(
                    name,
                    Some(
                        "with_blocking_for"
                            | "blocking_read_for"
                            | "blocking_write_for"
                            | "blocking_acquire_for"
                    )
                ));
        if async_body && blocking_facade {
            emit(
                cx,
                expr.span,
                BAN_UNBOUNDED_SYNC,
                "blocking facade called in async code; move this work to the blocking lane".into(),
            );
            return;
        }
        if path.first() == Some(&Symbol::intern("zccache_core"))
            && path.iter().any(|part| part.as_str() == "blocking_sync")
            && matches!(
                path.last().map(Symbol::as_str),
                Some("lock_for" | "read_for" | "write_for" | "try_lock" | "try_read" | "try_write")
            )
            && cx
                .tcx
                .coroutine_kind(cx.tcx.hir_enclosing_body_owner(expr.hir_id))
                .is_some()
        {
            emit(
                cx,
                expr.span,
                BAN_UNBOUNDED_SYNC,
                "blocking lock acquisition in async code; use sync::State::with_for".into(),
            );
            return;
        }
        if let Some((krate, ty, method)) = BANNED.iter().find(|(krate, ty, method)| {
            path.first() == Some(&Symbol::intern(krate)) && path_ends_with(&path, &[ty, method])
        }) {
            let shown = match *krate {
                "std" => "std::sync",
                "tokio" => "tokio::sync",
                "kernal_api" => "kernal_api::async_engine",
                _ => "parking_lot",
            };
            let message = if *method == "new" {
                format!(
                    "raw `{shown}::{ty}` constructed outside the bounded-wait boundary; use \
                     `zccache_core::sync::State` or `zccache_core::blocking_sync` (soldr#3836)"
                )
            } else if method.starts_with("try_") {
                format!(
                    "raw `{shown}::{ty}::{method}` bypasses the bounded-wait boundary; use \
                     `zccache_core::sync` (`try_lock`, or `lock_for`/`read_for`/`write_for` \
                     for a timeout) (soldr#3836)"
                )
            } else {
                format!(
                    "raw `{shown}::{ty}::{method}` can wait without a bound; use \
                     `zccache_core::sync` or `zccache_core::blocking_sync`, with a mandatory timeout \
                     (soldr#3836)"
                )
            };
            cx.opt_span_lint(
                BAN_UNBOUNDED_SYNC,
                Some(expr.span),
                DiagDecorator(move |diag| {
                    diag.primary_message(message);
                }),
            );
            return;
        }
        if let Some((krate, ty, method, replacement)) =
            BANNED_WAITS.iter().find(|(krate, ty, method, _)| {
                path.first() == Some(&Symbol::intern(krate)) && path_ends_with(&path, &[ty, method])
            })
        {
            let message = format!(
                "raw `{krate}::..::{ty}::{method}` blocks without a bound; use `{replacement}`, \
                 which takes a mandatory timeout (soldr#3836)"
            );
            cx.opt_span_lint(
                BAN_UNBOUNDED_SYNC,
                Some(expr.span),
                DiagDecorator(move |diag| {
                    diag.primary_message(message);
                }),
            );
        }
    }
}

/// Production sources of every workspace crate, minus the boundary module
/// itself and test-only files. The lint's own UI fixtures stay in scope.
///
/// A test crate (`--test` harness build) is out of scope, as in
/// `ban_swallowed_child_stdio`: inline `#[cfg(test)]` modules exist only
/// there, and every production item it contains is also compiled -- and
/// linted -- by the same crate's non-test build. Test barriers such as
/// `TEST_PROCESS_ENV_LOCK` are held for a whole test body, which a
/// critical-section watchdog must not bound.
fn span_is_in_scope(cx: &LateContext<'_>, span: rustc_span::Span) -> bool {
    let filename = source_filename(cx, span).replace('\\', "/");
    // Dependency-bearing UI examples also exercise every deny rule even
    // though their source paths are outside production crate directories.
    let is_ui_fixture = cx.sess().opts.unstable_opts.ui_testing
        || filename.starts_with("ui/")
        || filename.contains("/ui/");
    if is_ui_fixture {
        return true;
    }
    if cx.sess().is_test_crate() {
        return false;
    }
    let in_crate_src = filename.contains("crates/") && filename.contains("/src/");
    let is_boundary = [
        "sync.rs",
        "sync_barrier.rs",
        "sync_watchdog.rs",
        "blocking_sync.rs",
        "wait.rs",
    ]
    .iter()
    .any(|name| filename.ends_with(&format!("crates/zccache-core/src/{name}")));
    let is_test_only = filename.contains("/tests/")
        || filename.ends_with("_tests.rs")
        || filename.ends_with("/tests.rs");
    in_crate_src && !is_boundary && !is_test_only
}

fn source_filename(cx: &LateContext<'_>, span: rustc_span::Span) -> String {
    match cx.sess().source_map().span_to_filename(span) {
        FileName::Real(real_filename) => real_filename
            .local_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real_filename
                    .path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        filename => filename
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    }
}

fn path_ends_with(path: &[Symbol], expected: &[&&str]) -> bool {
    path.len() >= expected.len()
        && path[path.len() - expected.len()..]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| *actual == Symbol::intern(expected))
}

fn check_coroutine(cx: &LateContext<'_>, expr: &Expr<'_>) {
    let ExprKind::Closure(rustc_hir::Closure {
        kind:
            rustc_hir::ClosureKind::Coroutine(rustc_hir::CoroutineKind::Desugared(
                rustc_hir::CoroutineDesugaring::Async,
                _,
            )),
        def_id,
        ..
    }) = expr.kind
    else {
        return;
    };
    let Some(layout) = cx.tcx.mir_coroutine_witnesses(*def_id) else {
        return;
    };
    for cause in &layout.field_tys {
        let holds_data_guard = contains_data_guard(cx, cause.ty, &mut Vec::new());
        if holds_data_guard {
            emit(cx, cause.source_info.span, BAN_GUARD_ACROSS_AWAIT,
                "a protected-data guard survives await; extract owned data in sync::State::with_for before awaiting".into());
        }
    }
}

fn contains_data_guard<'tcx>(
    cx: &LateContext<'tcx>,
    ty: rustc_middle::ty::Ty<'tcx>,
    seen: &mut Vec<rustc_middle::ty::Ty<'tcx>>,
) -> bool {
    if seen.contains(&ty) {
        return false;
    }
    seen.push(ty);
    ty.walk().filter_map(|arg| arg.as_type()).any(|nested| {
        let rustc_middle::ty::Adt(adt, args) = nested.kind() else {
            return false;
        };
        let path = cx.get_def_path(adt.did());
        let Some(name) = path.last().map(Symbol::as_str) else {
            return false;
        };
        let krate = cx.tcx.crate_name(adt.did().krate);
        let guard = match krate.as_str() {
            "zccache_core" => {
                path.iter().any(|part| part.as_str() == "blocking_sync")
                    && matches!(name, "MutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard")
            }
            "std" | "lock_api" => matches!(
                name,
                "MutexGuard" | "MappedMutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard"
            ),
            "tokio" | "kernal_api" => {
                name.contains("MutexGuard")
                    || (name.contains("RwLock")
                        && name.contains("Guard")
                        && !args.types().any(|arg| arg.is_unit()))
            }
            "dashmap" => matches!(
                name,
                "Ref" | "RefMut" | "RefMulti" | "RefMutMulti" | "OccupiedEntry" | "VacantEntry"
            ),
            _ => false,
        };
        guard
            || (adt.did().is_local()
                && adt.all_fields().any(|field| {
                    contains_data_guard(cx, field.ty(cx.tcx, args).skip_norm_wip(), seen)
                }))
    })
}

fn emit(
    cx: &LateContext<'_>,
    span: rustc_span::Span,
    lint: &'static rustc_lint::Lint,
    message: String,
) {
    cx.opt_span_lint(
        lint,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(message);
        }),
    );
}

#[cfg(test)]
mod ui_test_support {
    include!("../../ui_test_support.rs");
}
#[cfg(test)]
mod ui_dependencies;
#[test]
fn ui() {
    ui_test_support::run(env!("CARGO_PKG_NAME"), env!("CARGO_MANIFEST_DIR"));
    let _directory =
        ui_test_support::CurrentDirGuard::set(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let flags = ui_dependencies::core_flags();
    dylint_testing::ui::Test::src_base(env!("CARGO_PKG_NAME"), "examples")
        .rustc_flags(flags)
        .run();
}
