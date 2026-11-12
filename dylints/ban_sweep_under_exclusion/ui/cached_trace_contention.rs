// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;

// These harmless helper summaries contend with the other UI compiler processes
// appending callback, destructor and dynamic-source records to the same trace.
macro_rules! helpers {
    ($($name:ident),+ $(,)?) => {
        $(fn $name() {})+
        fn held(guard: Guard<'_, ()>) {
            $($name();)+
            traced_callback(|| {});
            traced_callback(|| {});
            traced_callback(|| {});
            drop(guard);
        }
    };
}
fn traced_callback<F: FnOnce()>(callback: F) {
    callback();
}
helpers!(
    trace_contention_00,
    trace_contention_01,
    trace_contention_02,
    trace_contention_03,
    trace_contention_04,
    trace_contention_05,
    trace_contention_06,
    trace_contention_07,
    trace_contention_08,
    trace_contention_09,
    trace_contention_10,
    trace_contention_11,
    trace_contention_12,
    trace_contention_13,
    trace_contention_14,
    trace_contention_15,
    trace_contention_16,
    trace_contention_17,
    trace_contention_18,
    trace_contention_19,
    trace_contention_20,
    trace_contention_21,
    trace_contention_22,
    trace_contention_23,
    trace_contention_24,
    trace_contention_25,
    trace_contention_26,
    trace_contention_27,
    trace_contention_28,
    trace_contention_29,
    trace_contention_30,
    trace_contention_31,
);
fn main() {}
