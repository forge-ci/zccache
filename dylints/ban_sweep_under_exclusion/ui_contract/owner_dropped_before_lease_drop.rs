use zccache_daemon_core::daemon::server::TestMaterializationOwner;
fn main() {
    let owner = TestMaterializationOwner::inline();
    let lease = owner.lease();
    assert_eq!(lease.payload_count(), 0);
    drop(owner);
    // The native lease still has to be destroyed at scope exit, even though
    // the last payload access was before the candidate owner drop.
}
