// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod persist {
            pub mod output_destination {
                include!("auxiliary/server/output_destination.rs");
                struct DropSweep;
                impl Drop for DropSweep {
                    fn drop(&mut self) {
                        let _ = std::fs::remove_file("live-cache");
                    }
                }
                impl OutputDestination {
                    fn unlink(&self) -> std::io::Result<()> {
                        let _cleanup = DropSweep;
                        std::fs::remove_file(&self.path)
                    }
                    fn wrong_self(path: PathBuf) -> Self {
                        Self { path }
                    }
                }
                type Alias = OutputDestination;
                fn forge(path: PathBuf) {
                    let _ = Alias { path };
                }
                fn escape(output: OutputDestination) {
                    let Alias { path } = output;
                    drop(path);
                }
                fn field(output: OutputDestination) {
                    drop(output.path);
                }
                fn forge_scope() {
                    let _ = OutputScope {
                        cache_roots: vec![],
                    };
                }
                fn forge_private(root: PathBuf) {
                    let _ = PrivateOutputNamespace { root };
                }
                fn capture_factory() {
                    let _ = PrivateOutputNamespace::create;
                }
                fn forge_memory(path: PathBuf) -> OutputDestination {
                    unsafe { std::mem::transmute(path) }
                }
                fn erase(output: OutputDestination) {
                    let _ = Box::new(output) as Box<dyn std::any::Any>;
                }
                fn forget(output: OutputDestination) {
                    std::mem::forget(output);
                }
                fn critical(guard: zccache_core::sync::PublicationCommitPermit) {
                    let output = OutputScope::for_state().output(Path::new("requested-output"));
                    let _ = output.unlink();
                    drop(guard);
                }
                fn raw_unlink(guard: zccache_core::sync::PublicationReadPermit) {
                    let _ = std::fs::remove_file("live-cache");
                    drop(guard);
                }
            }
        }
    }
}
fn main() {}
