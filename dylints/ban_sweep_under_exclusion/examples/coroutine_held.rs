#![feature(coroutines, stmt_expr_attributes)]
#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
fn generator(permit: PublicationReadPermit, retired: DetachedKvNamespace) {
    let _task = #[coroutine]
    move || {
        yield ();
        let _ = retired.sweep_for(Duration::from_secs(1));
        drop(permit);
    };
}
fn released(permit: PublicationReadPermit, retired: DetachedKvNamespace) {
    let _task = #[coroutine]
    move || {
        yield ();
        drop(permit);
        yield ();
        let _ = retired.sweep_for(Duration::from_secs(1));
    };
}
fn main() {}
