use std::fs as disk;
mod alias { pub use std::fs::remove_dir_all as erase; }
fn main() {
    let direct = std::fs::remove_dir_all::<String>;
    let _ = direct(String::new());
    let erased: fn(String) -> std::io::Result<()> = std::fs::remove_dir_all::<String>;
    let _ = erased(String::new());
    let _ = disk::remove_dir("owned-cache");
    let _ = alias::erase("owned-cache");
}
