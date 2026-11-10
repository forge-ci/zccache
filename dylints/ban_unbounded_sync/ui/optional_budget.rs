// compile-flags: --edition=2021
#![allow(dead_code)]
use std::time::Duration as Budget;
type MaybeBudget = Option<Budget>;
fn receive(timeout: MaybeBudget) { let _ = timeout; }
fn download_wait(timeout_ms: Option<u64>) { let _ = timeout_ms; }
trait Waiting { fn receive(&self, budget: MaybeBudget); }
fn main() {}
