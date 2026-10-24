//! Guard leaks and erasure must not hide native exclusion from move analysis.
use super::guard_types::{contains, Scope};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty;

pub(super) fn check<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    let source = cx.typeck_results().expr_ty(expr);
    let adjusted = cx.typeck_results().expr_ty_adjusted(expr);
    let unsizing = cx
        .typeck_results()
        .expr_adjustments(expr)
        .iter()
        .any(|adjustment| {
            matches!(
                adjustment.kind,
                ty::adjustment::Adjust::Pointer(ty::adjustment::PointerCoercion::Unsize)
            )
        });
    // Deref/borrow adjustments provide ordinary protected-data access. Only
    // ownership-changing unsizing can hide the guard in a trait object here.
    let erased = unsizing && owns(cx, source) && !owns(cx, adjusted);
    let cast_erased = match expr.kind {
        ExprKind::Cast(value, _) => {
            owns(cx, cx.typeck_results().expr_ty(value)) && !owns(cx, source)
        }
        _ => false,
    };
    let leaking = leaks(cx, source);
    if erased || cast_erased || leaking {
        super::emit(
            cx,
            expr.span,
            "exclusion ownership must not be leaked or erased",
        );
    }
}

pub(super) fn leaks<'tcx>(cx: &LateContext<'tcx>, source: ty::Ty<'tcx>) -> bool {
    if let ty::FnDef(definition, arguments) = source.kind() {
        let path = cx.get_def_path(*definition);
        let name = path.last().map(|name| name.as_str()).unwrap_or("");
        matches!(
            cx.tcx.crate_name(definition.krate).as_str(),
            "std" | "core" | "alloc"
        ) && (matches!(
            name,
            "forget" | "transmute" | "transmute_copy" | "into_raw" | "leak"
        ) || name == "new"
            && path
                .iter()
                .any(|part| matches!(part.as_str(), "ManuallyDrop" | "MaybeUninit"))
            || name == "write" && path.iter().any(|part| part.as_str() == "MaybeUninit")
            // These unsafe operations bypass Rust's ordinary move/drop tracking.
            // Inspect their instantiated element type, not the raw pointer type.
            || path.iter().any(|part| part.as_str() == "ptr")
                && matches!(name, "read" | "read_unaligned" | "read_volatile"
                    | "write" | "write_unaligned" | "write_volatile"
                    | "copy" | "copy_nonoverlapping")
            || name == "set_len" && path.iter().any(|part| part.as_str() == "Vec"))
            && arguments.types().any(|value| owns(cx, value))
    } else {
        false
    }
}

fn owns<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>) -> bool {
    contains(cx, value, Scope::Ownership, &mut Vec::new())
}
