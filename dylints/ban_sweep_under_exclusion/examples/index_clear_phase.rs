// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
use zccache_core::sync::PublicationCommitPermit;
mod daemon {
    pub mod server {
        pub mod wal {
            pub struct PendingIndexClear;
            impl PendingIndexClear {
                pub fn wait_for(self) {}
            }
            pub fn reserve_index_clear_blocking() {}
        }
    }
}
use daemon::server::wal::{reserve_index_clear_blocking as reserve, PendingIndexClear};
fn held_reservation(permit: PublicationCommitPermit) {
    reserve();
    drop(permit);
}
fn held_completion(permit: PublicationCommitPermit, pending: PendingIndexClear) {
    let wait = PendingIndexClear::wait_for;
    wait(pending);
    drop(permit);
}
fn released(permit: PublicationCommitPermit, pending: PendingIndexClear) {
    drop(permit);
    reserve();
    PendingIndexClear::wait_for(pending);
}
fn main() {}
