// compile-flags: --edition=2021
#![allow(dead_code)]
use std::time::Duration as Budget;
type MaybeBudget = Option<Budget>;
fn receive(timeout: MaybeBudget) { let _ = timeout; }
fn download_wait(timeout_ms: Option<u64>) { let _ = timeout_ms; }
trait Waiting { fn receive(&self, budget: MaybeBudget); }
trait Milliseconds { fn wait(&self, timeout_ms: Option<u64>); }
struct Renamed;
impl Milliseconds for Renamed { fn wait(&self, ms: Option<u64>) { let _ = ms; } }
trait Milliseconds32 { fn wait(&self, timeout_ms: Option<u32>); }
fn main() {}
