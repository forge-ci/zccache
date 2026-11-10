// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod wal {
            include!("auxiliary/server/index_clear_transfer.rs");
            impl IndexClearReceiver {
                async fn receive_for(self) -> Option<PublicationCommitPermit> {
                    self.ready.try_send(()).ok()?;
                    let _ = std::fs::remove_file("retired");
                    let token =
                        kernal_api::async_engine::timeout(Duration::from_secs(1), self.commit)
                            .await
                            .ok()?
                            .ok()?;
                    Some(token.into_publication())
                }
            }
            fn unauthorized_constructor(permit: PublicationCommitPermit) {
                let _ = IndexClearCommit::new(permit);
            }
            fn unauthorized_literal(publication: PublicationCommitPermit) {
                let _ = IndexClearCommit { publication };
            }
            fn unauthorized_field(token: IndexClearCommit) {
                drop(token.publication);
            }
            fn unauthorized_pattern(token: IndexClearCommit) {
                let IndexClearCommit { publication } = token;
                drop(publication);
            }
            type Alias = IndexClearCommit;
            fn alias_literal(publication: PublicationCommitPermit) {
                let _ = Alias { publication };
            }
            fn alias_pattern(token: Alias) {
                let Alias { publication } = token;
                drop(publication);
            }
            impl IndexClearCommit {
                fn wrong_self(publication: PublicationCommitPermit) -> Self {
                    Self { publication }
                }
                fn wrong_self_pattern(self) {
                    let Self { publication } = self;
                    drop(publication);
                }
            }
            fn forge_readiness(receiver: &IndexClearReceiver) {
                let _ = receiver.ready.try_send(());
            }
            fn unauthorized_item(permit: PublicationCommitPermit) {
                let construct = IndexClearCommit::new;
                drop(construct(permit));
            }
            fn unauthorized_unseal(token: IndexClearCommit) {
                drop(token.into_publication());
            }
            fn unauthorized_receive(receiver: IndexClearReceiver) {
                drop(receiver.receive_for());
            }
            fn leak(token: IndexClearCommit) {
                std::mem::forget(token);
            }
            fn erase(token: IndexClearCommit) {
                let _ = Box::new(token) as Box<dyn std::any::Any>;
            }
            fn ordinary_channel(permit: PublicationCommitPermit) {
                let (sender, _receiver) = kernal_api::async_engine::oneshot_channel();
                let _ = sender.send(permit);
                let _ = std::fs::read("still protected");
            }
        }
    }
}
fn main() {}
