// Private identities reconstructed only for compiler policy fixtures. Runtime
// namespace/alias tests exercise the actual daemon destination constructors.
use std::path::{Path, PathBuf};
pub struct OutputScope {
    cache_roots: Vec<PathBuf>,
}
pub struct OutputDestination {
    path: PathBuf,
}
pub struct PrivateOutputNamespace {
    root: PathBuf,
}
impl OutputScope {
    pub fn for_state() -> Self {
        Self {
            cache_roots: vec![],
        }
    }
    pub fn output(&self, path: &Path) -> OutputDestination {
        let _ = &self.cache_roots;
        OutputDestination {
            path: path.to_path_buf(),
        }
    }
}
impl PrivateOutputNamespace {
    pub fn create(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
    pub fn output(&self) -> OutputDestination {
        OutputDestination {
            path: self.root.join("output"),
        }
    }
}
impl AsRef<Path> for OutputDestination {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl OutputDestination {
    pub fn fresh_temporary(parent: &Path) -> std::io::Result<(std::fs::File, Self)> {
        let path = parent.join(".owned-temporary");
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok((file, Self { path }))
    }
}
