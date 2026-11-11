#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
struct Cleanup(Option<DetachedKvNamespace>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(retired) = self.0.take() {
            let _ = retired.sweep_for(Duration::from_secs(1));
        }
    }
}
fn retain(retired: &mut Vec<Cleanup>, cleanup: Cleanup) {
    retired.push(cleanup);
}
fn transfer(permit: PublicationReadPermit, retired: &mut Vec<Cleanup>, cleanup: Cleanup) {
    retain(retired, cleanup);
    drop(permit);
}
fn option_transfer(
    permit: PublicationReadPermit,
    retired: &mut Vec<Cleanup>,
    value: Option<(String, Cleanup)>,
) {
    if let Some((_, cleanup)) = value {
        retired.push(cleanup);
    }
    drop(permit);
}
fn main() {}
