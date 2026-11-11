// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod wal {
            include!("auxiliary/server/index_clear_transfer.rs");
            impl IndexClearReceiver {
                async fn receive_for(self) -> Option<PublicationCommitPermit> {
                    self.ready.try_send(()).ok()?;
                    let token =
                        kernal_api::async_engine::timeout(Duration::from_secs(1), self.commit)
                            .await
                            .ok()?
                            .ok()?;
                    Some(token.into_publication())
                }
            }
        }
    }
}
fn main() {}
