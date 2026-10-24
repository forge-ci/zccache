#![allow(dead_code)]
// Trait-object drops use the concrete values this crate unsized into them.
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
trait Hook {}
impl Hook for Cleanup {}
trait Quiet {}
struct Plain;
impl Quiet for Plain {}
fn sources(cleanup: Cleanup) -> (Box<dyn Hook>, Box<dyn Quiet>) {
    (Box::new(cleanup), Box::new(Plain))
}
fn sweeping_object(permit: PublicationReadPermit, hook: Box<dyn Hook>) {
    drop(hook);
    drop(permit);
}
fn quiet_object(permit: PublicationReadPermit, quiet: Box<dyn Quiet>) {
    drop(quiet);
    drop(permit);
}
fn closures(cleanup: Cleanup) -> (Box<dyn Fn()>, Box<dyn Fn(u8)>) {
    (
        Box::new(move || {
            let _ = &cleanup;
        }),
        Box::new(|_| {}),
    )
}
fn sweeping_callback(permit: PublicationReadPermit, callback: Box<dyn Fn()>) {
    drop(callback);
    drop(permit);
}
// A different signature has its own sources.
fn quiet_callback(permit: PublicationReadPermit, callback: Box<dyn Fn(u8)>) {
    drop(callback);
    drop(permit);
}
fn boxed<F: Fn(u32) + 'static>(callback: F) -> Box<dyn Fn(u32)> {
    Box::new(callback)
}
fn instantiates(cleanup: Cleanup) -> Box<dyn Fn(u32)> {
    boxed(move |_| {
        let _ = &cleanup;
    })
}
// The generic helper's source is instantiated through its local caller.
fn generic_callback(permit: PublicationReadPermit, callback: Box<dyn Fn(u32)>) {
    drop(callback);
    drop(permit);
}
fn main() {}
