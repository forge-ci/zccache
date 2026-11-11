// Compiler fixtures deliberately reconstruct private identities. Runtime
// cancellation and publication ordering use the actual daemon in Rust tests.
use std::time::Duration;
use zccache_core::sync::PublicationCommitPermit;

pub struct IndexClearCommit {
    publication: PublicationCommitPermit,
}
impl IndexClearCommit {
    fn new(publication: PublicationCommitPermit) -> Self {
        Self { publication }
    }
    fn into_publication(self) -> PublicationCommitPermit {
        self.publication
    }
}
pub struct IndexClearReceiver {
    ready: std::sync::mpsc::SyncSender<()>,
    commit: kernal_api::async_engine::OneshotReceiver<IndexClearCommit>,
}
pub struct ReservedIndexClear {
    commit: kernal_api::async_engine::OneshotSender<IndexClearCommit>,
}
impl ReservedIndexClear {
    pub fn commit_for(self, publication: PublicationCommitPermit) {
        let _ = self.commit.send(IndexClearCommit::new(publication));
    }
}
pub fn reserve_index_clear_blocking() -> (ReservedIndexClear, IndexClearReceiver) {
    let (ready, _) = std::sync::mpsc::sync_channel(1);
    let (commit, receive) = kernal_api::async_engine::oneshot_channel();
    (
        ReservedIndexClear { commit },
        IndexClearReceiver {
            ready,
            commit: receive,
        },
    )
}
pub async fn process_index_writer_command(receiver: IndexClearReceiver) {
    let publication = receiver.receive_for().await;
    drop(publication);
}
