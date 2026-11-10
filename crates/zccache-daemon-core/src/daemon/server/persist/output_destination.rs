//! Destination authority for single-file output mutation under reader leases.
//! Cache namespace deletion continues to require detached trash ownership.
use super::*;
use std::io;

pub(in crate::daemon::server) struct OutputScope {
    cache_roots: Arc<[NormalizedPath]>,
}

impl OutputScope {
    pub(in crate::daemon::server) fn for_state(state: &SharedState) -> io::Result<Self> {
        Ok(Self {
            cache_roots: Arc::from([
                NormalizedPath::from(std::fs::canonicalize(&state.cache_dir)?),
                NormalizedPath::from(std::fs::canonicalize(state.staging.path())?),
            ]),
        })
    }

    #[cfg(test)]
    pub(in crate::daemon::server) fn unrestricted_for_test() -> Self {
        Self {
            cache_roots: Arc::from([]),
        }
    }

    pub(in crate::daemon::server) fn output(&self, path: &Path) -> io::Result<OutputDestination> {
        // Validate exactly the normalized name that later mutations will use.
        // Resolving a raw symlink/.. path first can validate a different file.
        let path = NormalizedPath::new(path);
        let resolved = resolve_destination(path.as_path())?;
        if self
            .cache_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "output destination is inside the live cache namespace: {}",
                    path.display()
                ),
            ));
        }
        Ok(OutputDestination { path })
    }

    pub(in crate::daemon::server) fn outputs<P: AsRef<Path>>(
        &self,
        paths: &[P],
    ) -> io::Result<Vec<OutputDestination>> {
        paths
            .iter()
            .map(|path| self.output(path.as_ref()))
            .collect()
    }
}

/// A fresh generation directory, before its namespace can become live.
pub(super) struct PrivateOutputNamespace {
    root: NormalizedPath,
}

impl PrivateOutputNamespace {
    pub(super) fn create(path: &Path) -> io::Result<Self> {
        if !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".tmp-"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private generation must use an unpublished temporary name",
            ));
        }
        std::fs::create_dir(path)?;
        Ok(Self {
            root: NormalizedPath::new(path),
        })
    }

    pub(super) fn output(&self, path: &Path) -> io::Result<OutputDestination> {
        if path.parent() != Some(self.root.as_path()) || path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output is not a private generation child",
            ));
        }
        Ok(OutputDestination {
            path: NormalizedPath::new(path),
        })
    }
}

/// Sealed proof that this name is an external output or a private new output.
/// It never authorizes removal of a live cache entry.
pub(in crate::daemon::server) struct OutputDestination {
    path: NormalizedPath,
}

impl AsRef<Path> for OutputDestination {
    fn as_ref(&self) -> &Path {
        self.path.as_path()
    }
}

impl OutputDestination {
    pub(super) fn unlink(&self) -> io::Result<()> {
        std::fs::remove_file(&self.path)
    }

    pub(super) fn sibling_temporary(&self) -> Self {
        Self {
            path: super::staged_store::temporary_path(self.path.as_path(), "materialize"),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon::server) fn for_test(path: &Path) -> Self {
        Self {
            path: NormalizedPath::new(path),
        }
    }
}

/// Resolve existing parents too: an output parent may alias the cache root.
/// Missing suffixes may contain only normal components, never lexical `..`.
fn resolve_destination(path: &Path) -> io::Result<NormalizedPath> {
    let mut existing = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in suffix.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved.into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or(error)?;
                suffix.push(name);
                existing = existing.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "output has no existing ancestor",
                    )
                })?;
                if existing.as_os_str().is_empty() {
                    existing = Path::new(".");
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub(in crate::daemon::server) fn checked_output_destinations<P: AsRef<Path>>(
    state: &SharedState,
    paths: &[P],
) -> MaterializationResult<Vec<OutputDestination>> {
    OutputScope::for_state(state)
        .and_then(|scope| scope.outputs(paths))
        .map_err(|error| {
            super::write_cached::destination_write_failure(
                paths
                    .first()
                    .map_or(Path::new("<output>"), |path| path.as_ref()),
                error,
            )
        })
}

pub(in crate::daemon::server) fn write_payloads_par_observed_for_state<P: AsRef<Path>>(
    state: &SharedState,
    targets: &[P],
    payloads: &[CachedPayload],
    mode: MaterializationMode,
) -> MaterializationResult<StagedMaterializationStats> {
    checked_output_destinations(state, targets)
        .and_then(|targets| write_payloads_par_observed_scoped(&targets, payloads, mode))
}
pub(in crate::daemon::server) fn write_payloads_par_with_delivery_for_state<P: AsRef<Path>>(
    state: &SharedState,
    targets: &[P],
    payloads: &[CachedPayload],
    mode: MaterializationMode,
    delivery: impl Fn(&Path) -> crate::compiler::DeliveryPolicy + Sync,
) -> MaterializationResult<StagedMaterializationStats> {
    checked_output_destinations(state, targets).and_then(|targets| {
        write_payloads_par_with_delivery_scoped(&targets, payloads, mode, delivery)
    })
}
pub(in crate::daemon::server) fn write_payloads_par_with_modes_for_state<P: AsRef<Path>>(
    state: &SharedState,
    targets: &[P],
    payloads: &[CachedPayload],
    mode: MaterializationMode,
    modes: &[u32],
) -> MaterializationResult<StagedMaterializationStats> {
    checked_output_destinations(state, targets)
        .and_then(|targets| write_payloads_par_with_modes_scoped(&targets, payloads, mode, modes))
}
pub(in crate::daemon::server) fn write_payloads_par_with_mtime_floor_and_policies_observed_for_state<
    P: AsRef<Path>,
    R: AsRef<Path>,
>(
    state: &SharedState,
    targets: &[P],
    payloads: &[CachedPayload],
    floors: &[R],
    policies: &[crate::compiler::DeliveryPolicy],
    mode: MaterializationMode,
) -> MaterializationResult<StagedMaterializationStats> {
    checked_output_destinations(state, targets).and_then(|targets| {
        write_payloads_par_with_mtime_floor_and_policies_observed_scoped(
            &targets, payloads, floors, policies, mode,
        )
    })
}
pub(in crate::daemon::server) fn write_provisional_payloads_par_with_mtime_floor_observed_for_state<
    P: AsRef<Path>,
    R: AsRef<Path>,
>(
    state: &SharedState,
    targets: &[P],
    payloads: &[CachedPayload],
    floors: &[R],
    policies: &[crate::compiler::DeliveryPolicy],
    mode: MaterializationMode,
) -> MaterializationResult<StagedMaterializationStats> {
    checked_output_destinations(state, targets).and_then(|targets| {
        write_provisional_payloads_par_with_mtime_floor_observed_scoped(
            &targets, payloads, floors, policies, mode,
        )
    })
}

#[cfg(test)]
#[path = "output_destination_tests.rs"]
mod tests;
