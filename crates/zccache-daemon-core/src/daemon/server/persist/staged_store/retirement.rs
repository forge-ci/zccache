//! Corruption and failed digest publication are retired by the maintenance owner.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::path::Component;
use std::sync::OnceLock;

#[derive(Debug)]
pub(in crate::daemon::server) struct CorruptBlobError;
impl std::fmt::Display for CorruptBlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cache blob failed durable verification; retirement deferred")
    }
}
impl std::error::Error for CorruptBlobError {}

pub(in crate::daemon::server) fn verified_corruption(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<CorruptBlobError>())
}

enum Candidate {
    Corrupt(NormalizedPath),
    DigestRollback {
        blob: NormalizedPath,
        published: fs::File,
        previous: Option<Vec<u8>>,
    },
}
type Pending = BTreeMap<NormalizedPath, VecDeque<Candidate>>;
static PENDING: OnceLock<crate::core::sync::State<Pending>> = OnceLock::new();
static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();
fn pending() -> &'static crate::core::sync::State<Pending> {
    PENDING.get_or_init(|| crate::core::sync::State::new(BTreeMap::new()))
}
pub(in crate::daemon::server) fn retirement_wake() -> &'static tokio::sync::Notify {
    WAKE.get_or_init(tokio::sync::Notify::new)
}
fn enqueue(root: &Path, candidate: Candidate, wake: bool) {
    pending().with_blocking_for(crate::core::sync::STATE_TIMEOUT, |all| {
        all.entry(root.into()).or_default().push_back(candidate);
    });
    if wake {
        retirement_wake().notify_waiters();
    }
}
pub(in crate::daemon::server) fn queue_corrupt_blob(root: &Path, path: &Path) {
    enqueue(root, Candidate::Corrupt(path.into()), true);
}
pub(in crate::daemon::server) fn queue_digest_rollback(
    blob: &Path,
    published: fs::File,
    previous: Option<Vec<u8>>,
) {
    if let Some(root) = blob.parent() {
        enqueue(
            root,
            Candidate::DigestRollback {
                blob: blob.into(),
                published,
                previous,
            },
            true,
        );
    }
}
pub(in crate::daemon::server) fn has_pending_retirements(root: &Path) -> bool {
    pending().with_blocking_for(crate::core::sync::STATE_TIMEOUT, |all| {
        all.get(&NormalizedPath::new(root))
            .is_some_and(|queue| !queue.is_empty())
    })
}

/// A pinned handle prevents inode reuse from turning a stale scan into authority.
struct PinnedName {
    path: NormalizedPath,
    file: fs::File,
    metadata: fs::Metadata,
}
impl PinnedName {
    fn open(path: &Path) -> io::Result<Self> {
        let file = fs::File::open(path)?;
        let metadata = file.metadata()?;
        Ok(Self {
            path: path.into(),
            file,
            metadata,
        })
    }
    fn unchanged(&self) -> io::Result<bool> {
        let current = fs::File::open(&self.path)?;
        let metadata = current.metadata()?;
        Ok(kernal_api::platform::fs::file_identity(&current)?
            == kernal_api::platform::fs::file_identity(&self.file)?
            && metadata.len() == self.metadata.len()
            && metadata.modified()? == self.metadata.modified()?)
    }
}
fn validate_owned_path(root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(io::Error::other)?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid retirement path",
        ));
    }
    let mut ancestor = Some(path);
    while let Some(parent) = ancestor {
        if is_staged_link_or_reparse(parent) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "linked retirement path",
            ));
        }
        if parent == root {
            break;
        }
        ancestor = parent.parent();
    }
    Ok(())
}

struct StagedSelection {
    directory: NormalizedPath,
    pointer_path: NormalizedPath,
    pointer: Option<PinnedName>,
    selected: bool,
}
impl StagedSelection {
    fn prepare(root: &Path, path: &Path) -> io::Result<Self> {
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("generation missing"))?;
        let key_dir = directory
            .parent()
            .ok_or_else(|| io::Error::other("key missing"))?;
        if key_dir.parent() != Some(staged_root(root).as_path()) {
            return Err(io::Error::other("foreign staged root"));
        }
        let key = key_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid key"))?;
        let current = pointer_path(root, key);
        validate_owned_path(root, &current)?;
        let (pointer, selected) = match PinnedName::open(&current) {
            Ok(pin) => {
                let selected =
                    maintenance::read_generation_pointer_for(&current, crate::core::wait::PROBE)?;
                let matches = directory
                    .file_name()
                    .is_some_and(|name| name == selected.trim());
                (Some(pin), matches)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, false),
            Err(error) => return Err(error),
        };
        Ok(Self {
            directory: directory.into(),
            pointer_path: current,
            pointer,
            selected,
        })
    }
    fn unchanged(&self) -> io::Result<bool> {
        match &self.pointer {
            Some(pin) => pin.unchanged(),
            None => match fs::symlink_metadata(&self.pointer_path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(error) => Err(error),
                Ok(_) => Ok(false),
            },
        }
    }
}
struct CorruptSnapshot {
    blob: PinnedName,
    digest: Option<PinnedName>,
    staged: Option<StagedSelection>,
}
impl CorruptSnapshot {
    fn prepare(root: &Path, path: &Path) -> io::Result<Option<Self>> {
        validate_owned_path(root, path)?;
        let blob = PinnedName::open(path)?;
        let digest_path = zccache_artifact::blob_digest::sidecar_path(path);
        validate_owned_path(root, &digest_path)?;
        let digest = match PinnedName::open(&digest_path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(expected) = super::super::link_registry::read_authoritative_blob_digest(path)? {
            if super::super::link_registry::hash_file(path)? == expected {
                return Ok(None);
            }
        }
        let staged = if is_staged_artifact_path(path) {
            Some(StagedSelection::prepare(root, path)?)
        } else {
            if path.parent() != Some(root) {
                return Err(io::Error::other("unsupported cache payload layout"));
            }
            None
        };
        Ok(Some(Self {
            blob,
            digest,
            staged,
        }))
    }

    fn unchanged(&self) -> io::Result<bool> {
        if !self.blob.unchanged()? {
            return Ok(false);
        }
        match &self.digest {
            Some(pin) if !pin.unchanged()? => return Ok(false),
            None if zccache_artifact::blob_digest::sidecar_path(&self.blob.path).exists() => {
                return Ok(false)
            }
            _ => {}
        }
        if let Some(staged) = &self.staged {
            return staged.unchanged();
        }
        Ok(true)
    }
    fn detach(&self, root: &Path, trash: &mut trash::DetachedStagedTrash) -> io::Result<()> {
        if let Some(staged) = &self.staged {
            if staged.selected {
                trash.detach(&staged.pointer_path)?;
            }
            trash.detach(&staged.directory)
        } else {
            if let Some(digest) = &self.digest {
                trash.detach_cache_child(root, &digest.path)?;
            }
            trash.detach_cache_child(root, &self.blob.path)
        }
    }
}

enum Prepared {
    Corrupt(CorruptSnapshot),
    DigestOnly(PinnedName),
    Restore {
        blob: PinnedName,
        digest: PinnedName,
        temporary: tempfile::NamedTempFile,
    },
}
fn prepare(root: &Path, candidate: &Candidate) -> io::Result<Option<Prepared>> {
    match candidate {
        Candidate::Corrupt(path) => {
            CorruptSnapshot::prepare(root, path).map(|value| value.map(Prepared::Corrupt))
        }
        Candidate::DigestRollback {
            blob,
            published,
            previous,
        } => {
            validate_owned_path(root, blob)?;
            let digest_path = zccache_artifact::blob_digest::sidecar_path(blob);
            validate_owned_path(root, &digest_path)?;
            let digest = PinnedName::open(&digest_path)?;
            if kernal_api::platform::fs::file_identity(&digest.file)?
                != kernal_api::platform::fs::file_identity(published)?
            {
                return Ok(None);
            }
            match PinnedName::open(blob) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Ok(Some(Prepared::DigestOnly(digest)))
                }
                Err(error) => Err(error),
                Ok(pin) => {
                    let actual = super::super::link_registry::hash_file(blob)?;
                    if previous.as_deref() == Some(actual.as_slice()) {
                        use std::io::Write;
                        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
                        temporary.write_all(actual.as_slice())?;
                        Ok(Some(Prepared::Restore {
                            blob: pin,
                            digest,
                            temporary,
                        }))
                    } else {
                        CorruptSnapshot::prepare(root, blob)
                            .map(|value| value.map(Prepared::Corrupt))
                    }
                }
            }
        }
    }
}
fn commit(
    root: &Path,
    prepared: &Prepared,
    trash: &mut trash::DetachedStagedTrash,
) -> io::Result<()> {
    let _store = StagedCommitGuard::acquire_for(root, Duration::from_secs(2))?;
    match prepared {
        Prepared::Corrupt(snapshot) => {
            validate_owned_path(root, &snapshot.blob.path)?;
            if let Some(digest) = &snapshot.digest {
                validate_owned_path(root, &digest.path)?;
            }
            if let Some(staged) = &snapshot.staged {
                validate_owned_path(root, &staged.pointer_path)?;
            }
            if snapshot.unchanged()? {
                let registered = registered_blob_id(&snapshot.blob.path);
                snapshot.detach(root, trash)?;
                if let Some(id) = registered {
                    unregister_blob_id(id);
                }
            }
        }
        Prepared::DigestOnly(digest) => {
            validate_owned_path(root, &digest.path)?;
            if digest.unchanged()? {
                trash.detach_cache_child(root, &digest.path)?;
            }
        }
        Prepared::Restore {
            blob,
            digest,
            temporary,
        } => {
            validate_owned_path(root, &blob.path)?;
            validate_owned_path(root, &digest.path)?;
            if blob.unchanged()? && digest.unchanged()? {
                crate::platform::fs::replace::atomic_replace(temporary.path(), &digest.path)?;
            }
        }
    }
    Ok(())
}
/// Hash/read outside exclusion, revalidate and detach inside it, then sweep.
pub(in crate::daemon::server) fn maintain_pending_retirements(
    state: &SharedState,
) -> io::Result<u64> {
    maintain_pending_retirements_for(state.artifact_dir.as_path(), &state.artifact_publication)
}
fn maintain_pending_retirements_for(
    root: &Path,
    publication_barrier: &crate::core::sync::PublicationBarrier,
) -> io::Result<u64> {
    let owner_root = NormalizedPath::new(root);
    let candidates = pending().with_blocking_for(crate::core::sync::STATE_TIMEOUT, |all| {
        let mut batch = Vec::new();
        if let Some(queue) = all.get_mut(&owner_root) {
            for _ in 0..64 {
                let Some(candidate) = queue.pop_front() else {
                    break;
                };
                batch.push(candidate);
            }
            if queue.is_empty() {
                all.remove(&owner_root);
            }
        }
        batch
    });
    let mut reclaimed = 0u64;
    for candidate in candidates {
        let mut trash = trash::DetachedStagedTrash::empty(root);
        let outcome = (|| {
            let Some(prepared) = prepare(root, &candidate)? else {
                return Ok(());
            };
            let Some(publication) = publication_barrier.try_write() else {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "publication busy",
                ));
            };
            let result = commit(root, &prepared, &mut trash);
            drop(publication);
            drop(prepared);
            result
        })();
        if let Err(error) = outcome {
            if error.kind() != io::ErrorKind::NotFound {
                enqueue(root, candidate, false);
            }
            tracing::debug!(%error, "cache retirement deferred");
        }
        reclaimed =
            reclaimed.saturating_add(trash.sweep_for(crate::core::wait::PROBE)?.reclaimed_bytes);
    }
    Ok(reclaimed)
}

#[cfg(test)]
pub(in crate::daemon::server) fn verify_then_retire_blob_for_test(path: &Path) -> io::Result<()> {
    let result = verify_registered_blob(path);
    if result.as_ref().is_err_and(verified_corruption) {
        let root = path.parent().unwrap();
        queue_corrupt_blob(root, path);
        maintain_pending_retirements_for(root, &crate::core::sync::PublicationBarrier::new())?;
    }
    result
}

#[cfg(test)]
mod retirement_regressions {
    use super::*;

    fn poison(root: &Path) -> NormalizedPath {
        let blob = NormalizedPath::new(root).join("blob");
        fs::write(&blob, b"original").unwrap();
        write_authoritative_blob_digest(&blob).unwrap();
        fs::write(&blob, b"poisoned").unwrap();
        blob
    }

    #[test]
    fn corruption_retirement_defers_for_publication_and_store_readers() {
        let root = tempfile::tempdir().unwrap();
        let blob = poison(root.path());
        let alias = root.path().join("alias");
        fs::hard_link(&blob, &alias).unwrap();
        let publication = crate::core::sync::PublicationBarrier::new();
        let reader = publication.try_read_owned().unwrap();
        let store_reader =
            StagedReadGuard::acquire_for(root.path(), crate::core::wait::PROBE).unwrap();
        queue_corrupt_blob(root.path(), &blob);
        maintain_pending_retirements_for(root.path(), &publication).unwrap();
        assert!(blob.exists());
        drop(reader);
        let waiting = Instant::now();
        maintain_pending_retirements_for(root.path(), &publication).unwrap();
        assert!(
            waiting.elapsed() < Duration::from_secs(3),
            "maintenance exceeded its two-second acquisition allowance"
        );
        assert!(
            blob.exists(),
            "cross-process reader must retain the cache name"
        );
        drop(store_reader);
        maintain_pending_retirements_for(root.path(), &publication).unwrap();
        assert!(!blob.exists());
        assert_eq!(fs::read(alias).unwrap(), b"poisoned");
    }

    #[test]
    fn stale_corruption_snapshot_preserves_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let blob = poison(root.path());
        let snapshot = CorruptSnapshot::prepare(root.path(), &blob)
            .unwrap()
            .unwrap();
        persist_artifact_output(&blob, b"replacement").unwrap();
        let publication = crate::core::sync::PublicationBarrier::new();
        let mut trash = trash::DetachedStagedTrash::empty(root.path());
        let writer = publication.try_write().unwrap();
        commit(root.path(), &Prepared::Corrupt(snapshot), &mut trash).unwrap();
        drop(writer);
        trash.sweep_for(crate::core::wait::PROBE).unwrap();
        assert_eq!(fs::read(&blob).unwrap(), b"replacement");
        verify_registered_blob(&blob).unwrap();
    }

    #[test]
    fn rollback_preserves_a_newer_sidecar_with_identical_digest_bytes() {
        let root = tempfile::tempdir().unwrap();
        let blob = root.path().join("blob");
        fs::write(&blob, b"published").unwrap();
        let published =
            super::super::super::link_registry::write_authoritative_blob_digest_for_pinned(
                &blob, &blob,
            )
            .unwrap();
        queue_digest_rollback(&blob, published, None);
        write_authoritative_blob_digest(&blob).unwrap();
        maintain_pending_retirements_for(
            root.path(),
            &crate::core::sync::PublicationBarrier::new(),
        )
        .unwrap();
        verify_registered_blob(&blob).unwrap();
        assert!(zccache_artifact::blob_digest::sidecar_path(&blob).exists());
    }

    #[test]
    fn failed_publication_restores_the_old_digest_after_exclusion_release() {
        let root = tempfile::tempdir().unwrap();
        let blob = root.path().join("blob");
        fs::write(&blob, b"previous").unwrap();
        write_authoritative_blob_digest(&blob).unwrap();
        let previous = fs::read(zccache_artifact::blob_digest::sidecar_path(&blob)).unwrap();
        let failed = root.path().join("unpublished");
        fs::write(&failed, b"never published").unwrap();
        let published =
            super::super::super::link_registry::write_authoritative_blob_digest_for_pinned(
                &failed, &blob,
            )
            .unwrap();
        queue_digest_rollback(&blob, published, Some(previous));
        maintain_pending_retirements_for(
            root.path(),
            &crate::core::sync::PublicationBarrier::new(),
        )
        .unwrap();
        verify_registered_blob(&blob).unwrap();
        assert_eq!(fs::read(blob).unwrap(), b"previous");
    }

    #[test]
    fn corrupt_staged_generation_is_retired_before_same_generation_republication() {
        let root = tempfile::tempdir().unwrap();
        let key = "a".repeat(64);
        let payload = Arc::new(b"original".to_vec());
        persist_staged_artifact_payloads(root.path(), &key, &[Arc::clone(&payload)]).unwrap();
        let paths = load_staged_artifact_paths(root.path(), &key, &[8])
            .unwrap()
            .unwrap();
        let blob = &paths[0];
        crate::platform::fs::permissions::make_writable(blob).unwrap();
        fs::write(blob, b"poisoned").unwrap();
        queue_corrupt_blob(root.path(), blob);
        maintain_pending_retirements_for(
            root.path(),
            &crate::core::sync::PublicationBarrier::new(),
        )
        .unwrap();
        assert!(!blob.exists());
        assert!(!pointer_path(root.path(), &key).exists());
        persist_staged_artifact_payloads(root.path(), &key, &[payload]).unwrap();
        let restored = load_staged_artifact_paths(root.path(), &key, &[8])
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&restored[0]).unwrap(), b"original");
    }
    #[test]
    fn repointed_orphan_generation_is_preserved_by_a_stale_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let key = "b".repeat(64);
        persist_staged_artifact_payloads(root.path(), &key, &[Arc::new(b"original".to_vec())])
            .unwrap();
        let paths = load_staged_artifact_paths(root.path(), &key, &[8])
            .unwrap()
            .unwrap();
        let blob = &paths[0];
        let generation = blob
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        crate::platform::fs::permissions::make_writable(blob).unwrap();
        fs::write(blob, b"poisoned").unwrap();
        let pointer = pointer_path(root.path(), &key);
        atomic_write(&pointer, "c".repeat(64).as_bytes()).unwrap();
        let snapshot = CorruptSnapshot::prepare(root.path(), blob)
            .unwrap()
            .unwrap();
        atomic_write(&pointer, generation.as_bytes()).unwrap();
        let publication = crate::core::sync::PublicationBarrier::new();
        let writer = publication.try_write().unwrap();
        let mut trash = trash::DetachedStagedTrash::empty(root.path());
        commit(root.path(), &Prepared::Corrupt(snapshot), &mut trash).unwrap();
        drop(writer);
        trash.sweep_for(crate::core::wait::PROBE).unwrap();
        assert!(
            blob.exists(),
            "a newly current generation requires a fresh retirement decision"
        );
        assert_eq!(fs::read_to_string(pointer).unwrap(), generation);
    }

    #[test]
    fn owned_temporary_keeps_the_reflink_store_tier_available() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"copy-on-write payload").unwrap();
        let probe = root.path().join("probe");
        let reflink_supported = kernal_api::platform::fs::reflink_file(&source, &probe).is_ok();
        let cached = root.path().join("cached");
        let stats = persist_artifact_file(&cached, &source, MaterializationMode::Auto).unwrap();
        if reflink_supported {
            assert_eq!(
                stats.reflink_count, 1,
                "the owned temporary must not disable reflinks"
            );
        }
        assert_eq!(fs::read(cached).unwrap(), b"copy-on-write payload");
    }
    #[cfg(unix)]
    #[test]
    fn fresh_temporary_normalizes_before_exclusive_creation() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("other/inner")).unwrap();
        std::os::unix::fs::symlink(root.path().join("other/inner"), root.path().join("alias"))
            .unwrap();
        let parent = root.path().join("alias/..");
        let (_temporary, destination) = OutputDestination::fresh_temporary(&parent).unwrap();
        assert!(
            destination.as_ref().exists(),
            "authority must name the file actually created"
        );
        assert_eq!(destination.as_ref().parent(), Some(root.path()));
    }

    #[tokio::test]
    async fn state_aware_delivery_queues_retirement_without_deleting_under_its_lease() {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(|| {
                let directory = tempfile::tempdir().unwrap();
                let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
                let state = &server.state;
                fs::create_dir_all(&state.artifact_dir).unwrap();
                let blob = poison(&state.artifact_dir);
                let outputs = tempfile::tempdir().unwrap();
                let requested = outputs.path().join("requested");
                let reader = state.artifact_publication.try_read_owned().unwrap();
                let result = write_payloads_par_observed_for_state(
                    state,
                    &[requested.as_path()],
                    &[CachedPayload::File(blob.clone())],
                    MaterializationMode::Copy,
                );
                assert!(matches!(
                    result,
                    Err(MaterializationFailure::CacheBlobMissing(_))
                ));
                assert!(!requested.exists());
                assert!(has_pending_retirements(&state.artifact_dir));
                maintain_pending_retirements(state).unwrap();
                assert!(blob.exists());
                drop(reader);
                maintain_pending_retirements(state).unwrap();
                assert!(!blob.exists());
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }
}
