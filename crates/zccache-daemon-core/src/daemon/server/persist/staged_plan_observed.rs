//! Reconcile compiler-declared staged outputs with physical filenames.

use super::staged_plan::StagedCompilePlan;
use crate::core::path::NormalizedPath;
use kernal_api::platform::fs::{ContextPathKind, DirectoryCursor};
use std::io;

impl StagedCompilePlan {
    /// Reconcile a compiler-declared output with the filename the compiler
    /// actually created in this private staging directory. Some compilers
    /// treat `-o <stem>` as a logical output declaration and append their own
    /// suffix (rather than writing precisely `<stem>`). Keep that observed
    /// mapping in the plan so both miss materialization and the persisted
    /// artifact metadata use the physical filename.
    ///
    /// This is intentionally filename- and compiler-agnostic: an observed
    /// file is accepted only when it is the sole direct child whose name is
    /// `<declared-name>.<suffix>`. Ambiguous or unrelated side outputs remain
    /// unsupported rather than guessing a target-specific convention.
    pub(in crate::daemon::server) fn observe_compiler_output_names(&mut self) -> io::Result<()> {
        let declared_staged_paths: Vec<_> = self
            .outputs
            .iter()
            .map(|output| output.staged.clone())
            .collect();
        for output in &mut self.outputs {
            if output.staged.is_file() {
                continue;
            }
            let Some(declared_name) = output.staged.file_name() else {
                continue;
            };
            let mut prefix = declared_name.to_os_string();
            prefix.push(".");
            let mut cursor = DirectoryCursor::open(&self.root)?;
            let mut candidates: Vec<NormalizedPath> = Vec::new();
            while let Some(entry) = cursor.next_entry()? {
                if entry.kind() != ContextPathKind::RegularFile {
                    continue;
                }
                let path = entry.path().to_path_buf();
                if declared_staged_paths
                    .iter()
                    .any(|declared| declared.as_path() == path.as_path())
                {
                    continue;
                }
                let matches_prefix = path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(prefix.to_string_lossy().as_ref())
                });
                if matches_prefix {
                    candidates.push(path.into());
                }
            }
            if candidates.len() != 1 {
                continue;
            }
            let observed = &candidates[0];
            let Some(observed_name) = observed.as_path().file_name() else {
                continue;
            };
            output.staged = observed.clone();
            output.requested = output.requested.parent().map_or_else(
                || output.requested.clone(),
                |parent| parent.join(observed_name).into(),
            );
        }
        Ok(())
    }

    /// Return the caller-visible destinations after output-name observation.
    pub(in crate::daemon::server) fn requested_output_paths(&self) -> Vec<NormalizedPath> {
        self.outputs
            .iter()
            .map(|output| output.requested.clone())
            .collect()
    }
}
