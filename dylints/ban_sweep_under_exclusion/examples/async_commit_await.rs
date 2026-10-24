#![allow(dead_code)]
use zccache_core::sync::PublicationCommitPermit;
async fn held(permit: PublicationCommitPermit) {
    std::future::pending::<()>().await;
    drop(permit);
}
fn main() {}
