#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, AmbigArg, Expr, ExprKind, Ty, TyKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::fs` outside the approved platform adapters and the explicit
    /// legacy allowlist, including module-qualified calls
    /// (`std::fs::read_dir`), imported items (`use std::fs::File`), and
    /// `std::fs` types.
    ///
    /// ### Why is this bad?
    ///
    /// `std::fs` exposes the raw mechanism, not the semantics zccache needs.
    /// Durability (parent-directory sync, shared append), opaque file identity
    /// and change markers, atomic replacement, private-directory permissions,
    /// mapped-file access and link classification are owned by kernal-api
    /// capabilities (see `docs/architecture/kernal-api-migration.toml`).
    /// Calling `std::fs` directly re-derives those semantics per call site,
    /// which is where cross-platform cache and watcher bugs come from: the
    /// Windows branches silently diverge because the POSIX branch "works".
    ///
    /// ### Known problems
    ///
    /// The workspace pre-dates the kernal-api integration, so ~390 files still
    /// call `std::fs` directly. Those files are allowlisted per file and the
    /// allowlist is burned down incrementally. The allowlist is file-granular:
    /// it does not stop a *new* `std::fs` call inside an already-listed file.
    /// Migration to exact-occurrence baselines (as in
    /// `enforce_platform_boundary`) happens once the file list is small.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let entries = std::fs::read_dir(&path)?;
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let entries = zccache_core::fs::read_dir(&path)?; // kernal-api capability
    /// ```
    pub BAN_STD_FS_OUTSIDE_ADAPTERS,
    Deny,
    "ban std::fs outside the approved platform adapters"
}

const STD_FS_PATH_PREFIX: &[&str] = &["std", "fs"];

/// The only production sources allowed to name `std::fs` directly: these are
/// where a native filesystem handle crosses into a kernal-api facade type.
/// Every entry exists in the tree and is checked by
/// `ci/check_dylint_wiring.py` via the sibling lint's fixture.
const ADAPTER_EXEMPT_PATHS: &[&str] = &[
    "crates/zccache-ipc/src/platform.rs",
    "crates/zccache-daemon-core/src/platform.rs",
    "crates/zccache-cli-core/src/platform.rs",
    "crates/zccache/src/platform.rs",
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for BanStdFsOutsideAdapters {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        let TyKind::Path(qpath) = ty.kind else {
            return;
        };
        let res = cx.qpath_res(&qpath, ty.hir_id);
        if is_banned_path(cx, res) && !is_exempt(cx, ty.span) {
            emit_lint(cx, ty.span);
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let ExprKind::Path(qpath) = expr.kind else {
            return;
        };
        let res = cx.qpath_res(&qpath, expr.hir_id);
        if is_banned_path(cx, res) && !is_exempt(cx, expr.span) {
            emit_lint(cx, expr.span);
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: Span) {
    cx.opt_span_lint(
        BAN_STD_FS_OUTSIDE_ADAPTERS,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`std::fs` is not allowed outside the approved platform adapters; \
                 use the kernal-api filesystem capability that owns durability, \
                 identity and permission semantics",
            );
        }),
    );
}

fn is_banned_path(cx: &LateContext<'_>, res: Res) -> bool {
    let Res::Def(_, def_id) = res else {
        return false;
    };
    let def_path = cx.get_def_path(def_id);
    def_path.len() > STD_FS_PATH_PREFIX.len()
        && def_path
            .iter()
            .take(STD_FS_PATH_PREFIX.len())
            .zip(STD_FS_PATH_PREFIX.iter())
            .all(|(actual, expected)| *actual == Symbol::intern(expected))
}

fn is_exempt(cx: &LateContext<'_>, span: Span) -> bool {
    let normalized = normalize_slashes(&source_filename(cx, span));
    is_adapter_source(&normalized)
        || ALLOWLIST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .any(|allowed| normalized.ends_with(allowed))
}

fn is_adapter_source(path: &str) -> bool {
    let normalized = normalize_slashes(path);
    ADAPTER_EXEMPT_PATHS
        .iter()
        .any(|adapter| normalized == *adapter || normalized.ends_with(&format!("/{adapter}")))
}

fn source_filename(cx: &LateContext<'_>, span: Span) -> String {
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

fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{is_adapter_source, ADAPTER_EXEMPT_PATHS};

    #[test]
    fn adapter_sources_are_exempt_on_relative_and_absolute_paths() {
        assert!(is_adapter_source("crates/zccache-ipc/src/platform.rs"));
        assert!(is_adapter_source(
            "/checkout/crates/zccache-daemon-core/src/platform.rs"
        ));
        assert!(is_adapter_source(
            r"C:\checkout\crates\zccache-cli-core\src\platform.rs"
        ));
    }

    #[test]
    fn similarly_named_paths_outside_the_platform_adapters_are_not_exempt() {
        assert!(!is_adapter_source(
            "crates/zccache-ipc/src/platform/test.rs"
        ));
        assert!(!is_adapter_source("crates/zccache-core/src/platform.rs"));
        assert!(!is_adapter_source("crates/zccache-ipc/src/platform.rs.bak"));
    }

    #[test]
    fn adapter_exemption_list_is_unique() {
        for (index, adapter) in ADAPTER_EXEMPT_PATHS.iter().enumerate() {
            assert!(
                ADAPTER_EXEMPT_PATHS[..index]
                    .iter()
                    .all(|previous| previous != adapter),
                "duplicate adapter exemption: {adapter}",
            );
        }
    }
}

#[cfg(test)]
mod ui_test_support {
    include!("../../ui_test_support.rs");
}

#[test]
fn ui() {
    ui_test_support::run(env!("CARGO_PKG_NAME"), env!("CARGO_MANIFEST_DIR"));
}
