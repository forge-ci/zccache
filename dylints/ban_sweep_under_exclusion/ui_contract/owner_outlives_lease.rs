use zccache_daemon_core::daemon::server::TestMaterializationOwner;
fn main() {
    let owner = TestMaterializationOwner::inline();
    let lease = owner.lease();
    assert_eq!(lease.payload_count(), 0);
    drop(lease);
    drop(owner);
}
