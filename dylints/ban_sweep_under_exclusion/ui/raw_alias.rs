use std::fs::remove_dir_all as renamed;
fn main() {
    let _ = renamed("owned-cache");
}
