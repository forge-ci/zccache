#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, FnKind, Visitor};
use rustc_hir::{Arm, BinOpKind, Body, Expr, ExprKind, FnDecl, Pat, PatExprKind, PatKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::{FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans hand-rolled boolean parsing of environment values outside
    /// `zccache_core::config::env_policy`: sets of boolean spellings, and
    /// `"0"`/`"1"` truthiness beside an environment read or emptiness check.
    ///
    /// ### Why is this bad?
    ///
    /// Each copy looks correct alone and the defect lives between them:
    /// `ZCCACHE_QUIET=false` used to enable quiet mode because its parser only
    /// excluded empty and `0`.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// std::env::var("X").is_ok_and(|v| !v.is_empty() && v != "0")
    /// ```
    ///
    /// Register the switch and use its typed accessor instead:
    ///
    /// ```rust,ignore
    /// zccache_core::config::quiet_enabled()
    /// ```
    pub BAN_RAW_ENV_FLAG,
    Deny,
    "interpret environment booleans through the typed zccache_core::config accessors"
}

const SPELLINGS: &[&str] = &[
    "1", "0", "true", "false", "yes", "no", "on", "off", "y", "n",
];
const POLICY_OWNER: &str = "crates/zccache-core/src/config/env_policy.rs";
/// Foreign vocabularies, plus unresolved owned grammars. Each `#2119` row must
/// go when that research issue decides the site's grammar.
const EXEMPT_FILES: &[&str] = &[
    // Foreign: setup-soldr's normalization for SOLDR_* (#184).
    "crates/zccache-cli-core/src/cli/commands/util.rs",
    // Foreign: Dylint's own truthiness rule.
    "crates/zccache-daemon-core/src/daemon/server/rustc.rs",
    // Foreign: rustfmt's config grammar.
    "crates/zccache-cli-core/src/formatter.rs",
    // Foreign: the running-process broker's escape hatch.
    "crates/zccache-ipc/src/lib.rs",
    // #2119
    "crates/zccache-fingerprint/src/pch_inputs/mod.rs",
    "crates/zccache-cli-core/src/download_client/artifact/mod.rs",
    "crates/zccache-cli-core/src/cli/commands/daemon.rs",
    "crates/zccache-daemon-core/src/daemon/process.rs",
    "crates/zccache-artifact/src/layout.rs",
    "crates/zccache-artifact/src/rust_plan/local.rs",
    "crates/zccache-cli-core/src/cli/commands/wrap/env.rs",
    "crates/zccache-daemon-core/src/daemon/server/dependency_policy.rs",
    "crates/zccache-daemon-core/src/daemon/server/persist/fs_caps.rs",
];

impl<'tcx> LateLintPass<'tcx> for BanRawEnvFlag {
    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: FnKind<'tcx>,
        _: &'tcx FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        span: Span,
        _: LocalDefId,
    ) {
        if matches!(kind, FnKind::Closure) || span.from_expansion() {
            return;
        }
        if is_exempt(&normalize_slashes(&source_filename(cx, span))) {
            return;
        }
        let mut scan = Scan {
            cx,
            compared: Vec::new(),
            sets: Vec::new(),
            reads_env: false,
            checks_empty: false,
        };
        scan.visit_body(body);
        scan.report();
    }
}

struct Scan<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    compared: Vec<(&'static str, Span)>,
    sets: Vec<Span>,
    reads_env: bool,
    checks_empty: bool,
}

impl Scan<'_, '_> {
    fn report(&self) {
        for span in &self.sets {
            self.emit(*span);
        }
        let Some((first_spelling, first_span)) = self.compared.first() else {
            return;
        };
        if self
            .compared
            .iter()
            .any(|(spelling, _)| spelling != first_spelling)
        {
            self.emit(*first_span);
            return;
        }
        if matches!(*first_spelling, "0" | "1") && (self.reads_env || self.checks_empty) {
            self.emit(*first_span);
        }
    }

    fn emit(&self, span: Span) {
        self.cx.opt_span_lint(
            BAN_RAW_ENV_FLAG,
            Some(span),
            DiagDecorator(|diag| {
                diag.primary_message(
                    "hand-rolled environment boolean parsing; register the switch in \
                     zccache_core::config::env_policy and use its typed accessor",
                );
            }),
        );
    }

    fn note_compared(&mut self, operand: &Expr<'_>) {
        if let Some(spelling) = spelling_of(self.cx, operand) {
            self.compared.push((spelling, operand.span));
        }
    }

    fn note_call(&mut self, def_id: DefId, receiver: Option<&Expr<'_>>, arguments: &[Expr<'_>]) {
        let path = self.cx.tcx.def_path_str(def_id);
        if path.ends_with("env::var") || path.ends_with("env::var_os") {
            self.reads_env = true;
        } else if path.ends_with("::is_empty") {
            self.checks_empty = true;
        } else if is_comparison_path(&path) {
            if let Some(receiver) = receiver {
                self.note_compared(receiver);
            }
            for argument in arguments {
                self.note_compared(argument);
            }
        }
    }

    fn note_match(&mut self, span: Span, arms: &[Arm<'_>]) {
        let mut literals = Vec::new();
        for arm in arms {
            collect_pattern_strings(arm.pat, &mut literals);
        }
        let spellings = literals
            .iter()
            .filter(|literal| SPELLINGS.contains(&literal.as_str()))
            .count();
        if spellings >= 2 && spellings == literals.len() {
            self.sets.push(span);
        }
    }
}

impl<'tcx> Visitor<'tcx> for Scan<'_, 'tcx> {
    type NestedFilter = nested_filter::OnlyBodies;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.cx.tcx
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match &expr.kind {
            ExprKind::Binary(op, left, right)
                if matches!(op.node, BinOpKind::Eq | BinOpKind::Ne) =>
            {
                self.note_compared(left);
                self.note_compared(right);
            }
            ExprKind::MethodCall(_, receiver, arguments, _) => {
                let def_id = self
                    .cx
                    .tcx
                    .typeck(expr.hir_id.owner.def_id)
                    .type_dependent_def_id(expr.hir_id);
                if let Some(def_id) = def_id {
                    self.note_call(def_id, Some(receiver), arguments);
                }
            }
            ExprKind::Call(callee, arguments) => {
                if let ExprKind::Path(qpath) = &callee.kind {
                    if let Res::Def(_, def_id) = self.cx.qpath_res(qpath, callee.hir_id) {
                        self.note_call(def_id, None, arguments);
                    }
                }
            }
            ExprKind::Match(_, arms, _) => self.note_match(expr.span.source_callsite(), arms),
            ExprKind::Array(items) => {
                let spellings = items
                    .iter()
                    .filter(|item| spelling_of(self.cx, item).is_some())
                    .count();
                if spellings >= 2 && spellings == items.len() {
                    self.sets.push(expr.span);
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, expr);
    }
}

fn is_comparison_path(path: &str) -> bool {
    path.ends_with("::eq_ignore_ascii_case") || path.ends_with("::eq") || path.ends_with("::ne")
}

/// The boolean spelling this operand denotes: a literal, a borrow of one, or a
/// constant resolved by value so an alias cannot hide it.
fn spelling_of(cx: &LateContext<'_>, expr: &Expr<'_>) -> Option<&'static str> {
    let value = match &expr.kind {
        ExprKind::AddrOf(_, _, inner) => return spelling_of(cx, inner),
        ExprKind::Lit(literal) => match literal.node {
            LitKind::Str(value, _) => value.as_str().to_owned(),
            _ => return None,
        },
        ExprKind::Path(qpath) => {
            let Res::Def(DefKind::Const { .. }, def_id) = cx.qpath_res(qpath, expr.hir_id) else {
                return None;
            };
            let declared = cx.tcx.type_of(def_id).skip_binder();
            if !matches!(declared.peel_refs().kind(), ty::Str) {
                return None;
            }
            let value = cx.tcx.const_eval_poly(def_id).ok()?;
            let bytes = value.try_get_slice_bytes_for_diagnostics(cx.tcx)?;
            std::str::from_utf8(bytes).ok()?.to_owned()
        }
        _ => return None,
    };
    SPELLINGS.iter().copied().find(|known| *known == value)
}

fn collect_pattern_strings(pat: &Pat<'_>, literals: &mut Vec<String>) {
    match &pat.kind {
        PatKind::Or(alternatives) => {
            for alternative in *alternatives {
                collect_pattern_strings(alternative, literals);
            }
        }
        PatKind::Expr(expression) => {
            if let PatExprKind::Lit { lit, .. } = expression.kind {
                if let LitKind::Str(value, _) = lit.node {
                    literals.push(value.as_str().to_owned());
                }
            }
        }
        _ => {}
    }
}

fn is_exempt(filename: &str) -> bool {
    filename.ends_with(POLICY_OWNER) || EXEMPT_FILES.iter().any(|file| filename.ends_with(file))
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
    use super::{is_exempt, POLICY_OWNER};

    #[test]
    fn exemptions_match_workspace_suffixes() {
        assert!(is_exempt(&format!("/workspace/zccache/{POLICY_OWNER}")));
        assert!(!is_exempt("crates/zccache-core/src/defender.rs"));
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
