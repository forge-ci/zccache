// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
mod daemon {
    pub mod server {
        pub mod persist {
            pub mod output_destination {
                include!("auxiliary/server/output_destination.rs");
                impl OutputDestination {
                    fn unlink(&self) -> std::io::Result<()> {
                        std::fs::remove_file(&self.path)
                    }
                }
                fn restore(guard: zccache_core::sync::PublicationReadPermit) {
                    let output = OutputScope::for_state().output(Path::new("requested-output"));
                    let _ = output.unlink();
                    drop(guard);
                }
            }
            mod staged_store {
                mod publish {
                    fn persist_staged_generation() {
                        let private =
                            super::super::output_destination::PrivateOutputNamespace::create(
                                std::path::Path::new(".tmp-new"),
                            );
                        let _ = private.output();
                    }
                }
            }
        }
    }
}
fn main() {}
