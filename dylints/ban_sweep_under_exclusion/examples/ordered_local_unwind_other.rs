#![allow(dead_code)]
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
struct Cleanup(Option<DetachedKvNamespace>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(retired) = self.0.take() {
            let _ = retired.sweep_for(std::time::Duration::from_secs(1));
        }
    }
}
struct Panics;
impl Drop for Panics {
    fn drop(&mut self) {
        panic!("live destructor unwind must remain reachable");
    }
}
fn partially_moved(permit: PublicationReadPermit, cleanup: Cleanup) {
    let (original, cached) = (permit, cleanup);
    {
        let owner = (String::new(), Panics);
        let _moved = owner.0;
        // owner.1 is still live and its destructor can unwind into cached.
    }
    let publication = original;
    drop(publication);
    drop(cached);
}
fn main() {}
