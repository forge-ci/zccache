// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod wal {
            include!("auxiliary/server/wal.rs");
        }
    }
}
fn main() {}
