// compile-flags: --edition=2021
use std::sync::Mutex as Renamed;
fn main() {
    let lock = Renamed::new(1);
    let _guard = lock.lock();
    let _function_item = Renamed::<u8>::lock;
    let (_, rx) = std::sync::mpsc::channel::<()>();
    let _ = rx.recv();
    let _ = std::process::Command::new("probe").output();
}
