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
fn release_locals(permit: PublicationReadPermit, cleanup: Cleanup, fail: bool) {
    let (original, cached) = (permit, cleanup);
    let publication = original;
    assert!(!fail);
    drop(publication);
    drop(cached);
}
fn branch_locals(permit: PublicationReadPermit, cleanup: Cleanup, fail: bool) {
    let (original, cached) = (permit, cleanup);
    let publication = original;
    if fail {
        drop(publication);
        drop(cached);
        return;
    }
    assert!(!fail);
    drop(publication);
    drop(cached);
}
fn try_locals(
    input: Result<(PublicationReadPermit, Cleanup), String>,
    fail: bool,
) -> Result<(), String> {
    let (original, cached) = input?;
    let publication = original;
    assert!(!fail);
    drop(publication);
    drop(cached);
    Ok(())
}
fn main() {}
