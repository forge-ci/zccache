// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod persist {
            pub mod staged_plan {
                include!("auxiliary/server/persist/staged_plan.rs");
            }
        }
    }
}
fn main() {}
