//! Per-daemon private compiler staging (`StagingRoot`) and the startup
//! sweep that reclaims crash debris from dead daemons.

use super::*;

const STAGING_LOCK_FILE: &str = ".active.lock";
const CONFIGURED_STAGING_CHILD: &str = "zccache-staging";
/// Private plan roots retired by [`detach_private_root`]. Only the staging
/// root's sweeper thread removes these trees, never the dropping owner (#1964).
const PRIVATE_TRASH: &str = ".trash";
/// How often the sweeper reclaims retired private plan roots.
const PRIVATE_TRASH_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
static NEXT_PRIVATE_TRASH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// How old a staging directory with no `.active.lock` must be before the
/// cleaner may treat it as crash debris (soldr#1250).
///
/// `StagingRoot::new` creates the directory and only then opens its lock, so
/// for a brief moment a perfectly healthy staging root exists with no lock
/// file. A cleaner that removes lockless directories on sight deletes live
/// roots during that window, and the creating daemon's own `open` then fails
/// with `ENOENT`.
///
/// Absence of a lock file therefore cannot mean "abandoned" on its own — it
/// means "cannot judge yet". Age is what separates a directory being born
/// from one whose daemon died before it could write a lock: the create/lock
/// gap is a couple of syscalls, so anything older than this by orders of
/// magnitude is genuinely debris.
const STAGING_ABANDONED_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long every entry under an *unlocked* staging root must have been
/// untouched before the cleaner may remove it (#1796).
///
/// A released `.active.lock` proves only that the owning daemon is gone. A
/// compile child it spawned can outlive it (an abrupt exit that bypassed
/// child cleanup) and keep writing under the root; deleting the root then
/// fails that compile with `couldn't create a temp dir … (os error 2)`. No
/// single compile goes an hour without touching its outputs, and debris
/// only costs disk until the next startup, so err far toward keeping it.
const STAGING_ABANDONED_QUIET_PERIOD: std::time::Duration = std::time::Duration::from_secs(3600);

/// Per-daemon private output staging. The held lock distinguishes an active
/// daemon from crash debris, so startup cleanup cannot remove another live
/// daemon's compiler outputs.
pub(super) struct StagingRoot {
    path: NormalizedPath,
    lock: Option<kernal_api::platform::fs::OwnedFileLock>,
    sweeper: Option<PrivateTrashSweeper>,
}

/// Owns the only recursive removal of this root's retired private trees.
/// A stop message makes the thread remove the whole root; a dropped sender
/// alone stops it without removing anything. Nobody waits for that removal:
/// a root left by process exit is reclaimed by the abandoned-root cleanup.
struct PrivateTrashSweeper {
    messages: std::sync::mpsc::Sender<SweeperMessage>,
}

enum SweeperMessage {
    /// Remove a private root that lives outside this staging root.
    Retire(NormalizedPath),
    /// The staging root is dropped: remove it and exit.
    Stop,
}

/// Hands a private root outside the staging root, such as a directory
/// bundle beside its requested output, to the staging sweeper. Sending never
/// touches the filesystem, so owners may retire roots under exclusion.
#[derive(Clone)]
pub(super) struct PrivateRootRetirer(std::sync::mpsc::Sender<SweeperMessage>);

impl PrivateRootRetirer {
    pub(super) fn retire(&self, root: &Path) {
        #[cfg(test)]
        super::persist::StagedHookGuard::pause_private_plan_cleanup(root);
        if self.0.send(SweeperMessage::Retire(root.into())).is_err() {
            tracing::debug!(root = %root.display(), "staging sweeper stopped; private root retained");
        }
    }
}

impl StagingRoot {
    pub(super) fn new(
        cache_dir: &Path,
        configured_parent: Option<&Path>,
        instance: u64,
    ) -> std::io::Result<Self> {
        use std::io::Write;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let parent = configured_parent
            .map(|root| root.join(CONFIGURED_STAGING_CHILD))
            .unwrap_or_else(|| cache_dir.join("staging"));
        let path = parent.join(format!("{}-{instance}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        let path = NormalizedPath::from(path);
        // Start the sweeper before holding the root lock: spawning is not
        // part of the lock's critical section. Until `Drop` sends the stop
        // message, the sweeper never removes the root itself.
        let sweeper = spawn_private_trash_sweeper(path.clone())?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.join(STAGING_LOCK_FILE))?;
        // Never wait behind a cleaner that observed this just-created
        // directory before we acquired its lock. Failing daemon startup is
        // safer than returning a staging root a concurrent cleaner unlinked.
        let lock = kernal_api::platform::fs::try_lock_exclusive_owned(file)?;
        writeln!(lock.file(), "{}", std::process::id())?;
        Ok(Self {
            path,
            lock: Some(lock),
            sweeper: Some(sweeper),
        })
    }

    pub(super) fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub(super) fn retirer(&self) -> Option<PrivateRootRetirer> {
        self.sweeper
            .as_ref()
            .map(|sweeper| PrivateRootRetirer(sweeper.messages.clone()))
    }

    pub(super) fn cleanup_abandoned(&self) -> std::io::Result<usize> {
        self.cleanup_abandoned_older_than(STAGING_ABANDONED_MIN_AGE)
    }

    /// [`Self::cleanup_abandoned`] with an explicit minimum age for the
    /// lockless case, so tests can exercise both sides of the age gate
    /// without sleeping.
    fn cleanup_abandoned_older_than(&self, min_age: std::time::Duration) -> std::io::Result<usize> {
        let Some(parent) = self.path.parent() else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in std::fs::read_dir(parent)?.flatten() {
            let path = entry.path();
            if !path.is_dir() || path == self.path.as_path() {
                continue;
            }
            // Deliberately NOT `create(true)` (soldr#1250). Creating the lock
            // file here manufactures the very artifact whose absence should
            // have protected the directory: the cleaner would then find its
            // own brand-new file unlocked, conclude the root was abandoned,
            // and delete a staging root that another daemon is mid-way
            // through creating.
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.join(STAGING_LOCK_FILE));
            let lock = match lock {
                Ok(lock) => lock,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    // No lock file: either a root being born, or debris from a
                    // daemon that died before writing one. Only age tells them
                    // apart, and guessing wrong deletes live output.
                    if staging_dir_is_older_than(&path, min_age) {
                        std::fs::remove_dir_all(&path)?;
                        removed += 1;
                    }
                    continue;
                }
                Err(_) => continue,
            };
            // Taking the lock is the probe: if a live daemon holds it, this
            // root is not abandoned. Release it again before deleting.
            let Ok(probe) = kernal_api::platform::fs::try_lock_exclusive(&lock) else {
                continue;
            };
            drop(probe);
            drop(lock);
            // The daemon is gone, but an orphaned compile child may not be.
            if !staging_tree_is_quiet_for(&path, STAGING_ABANDONED_QUIET_PERIOD) {
                continue;
            }
            std::fs::remove_dir_all(&path)?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// Is this staging directory old enough that a missing `.active.lock` can
/// only mean crash debris?
///
/// Errs toward "no": an unreadable mtime, or a clock that makes the directory
/// look like it is from the future, both return false. Skipping real debris
/// costs disk until the next pass; removing a live root costs a failed build.
fn staging_dir_is_older_than(path: &Path, min_age: std::time::Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
        return false;
    };
    age >= min_age
}

/// Has nothing under `root` (root included) been modified within `quiet`?
///
/// Errs toward "no" exactly like [`staging_dir_is_older_than`]: any entry
/// that cannot be read, or looks newer than `quiet`, keeps the tree.
fn staging_tree_is_quiet_for(root: &Path, quiet: std::time::Duration) -> bool {
    if !staging_dir_is_older_than(root, quiet) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let path = entry.path();
        let quiet_entry = match entry.file_type() {
            Ok(kind) if kind.is_dir() => staging_tree_is_quiet_for(&path, quiet),
            Ok(_) => staging_dir_is_older_than(&path, quiet),
            Err(_) => false,
        };
        if !quiet_entry {
            return false;
        }
    }
    true
}

/// Retire a private plan root with one same-filesystem rename into the
/// staging root's `.trash`. Owners call this from `Drop`, possibly while a
/// caller holds publication or store exclusion, so it never walks the tree.
/// A failed rename (an open handle on Windows, for example) leaves the root
/// in place until the whole staging root is removed.
pub(super) fn detach_private_root(root: &Path) {
    #[cfg(test)]
    super::persist::StagedHookGuard::pause_private_plan_cleanup(root);
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return;
    };
    let trash = parent.join(PRIVATE_TRASH);
    match std::fs::create_dir(&trash) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            tracing::debug!(%error, root = %root.display(), "private trash unavailable");
            return;
        }
    }
    let id = NEXT_PRIVATE_TRASH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let destination = trash.join(format!("{}-{id}", name.to_string_lossy()));
    if let Err(error) = crate::platform::fs::replace::rename_without_replace(root, &destination) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(%error, root = %root.display(), "private root detach failed");
        }
    }
}

fn spawn_private_trash_sweeper(root: NormalizedPath) -> std::io::Result<PrivateTrashSweeper> {
    let (messages, received) = std::sync::mpsc::channel::<SweeperMessage>();
    std::thread::Builder::new()
        .name("zccache-staging-sweeper".to_owned())
        .spawn(move || {
            let mut swept = std::time::Instant::now();
            loop {
                match crate::core::wait::recv_for(&received, PRIVATE_TRASH_SWEEP_INTERVAL) {
                    Ok(SweeperMessage::Retire(retired)) => remove_retired_root(retired.as_path()),
                    Ok(SweeperMessage::Stop) => {
                        remove_retired_root(root.as_path());
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
                // Steady retire traffic must not starve the trash sweep.
                if swept.elapsed() >= PRIVATE_TRASH_SWEEP_INTERVAL {
                    sweep_private_trash(root.as_path());
                    swept = std::time::Instant::now();
                }
            }
        })?;
    Ok(PrivateTrashSweeper { messages })
}

/// Reclaim retired private plan roots. Runs only on the sweeper thread.
fn sweep_private_trash(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join(PRIVATE_TRASH)) else {
        return;
    };
    for entry in entries.flatten() {
        if let Err(error) = std::fs::remove_dir_all(entry.path()) {
            tracing::debug!(%error, path = %entry.path().display(), "private trash retained for retry");
        }
    }
}

fn remove_retired_root(root: &Path) {
    if let Err(error) = std::fs::remove_dir_all(root) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(%error, root = %root.display(), "retired private root retained");
        }
    }
}

impl Drop for StagingRoot {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            drop(lock);
        }
        if let Some(sweeper) = self.sweeper.take() {
            if sweeper.messages.send(SweeperMessage::Stop).is_err() {
                tracing::debug!(root = %self.path.display(), "staging sweeper already stopped");
            }
        }
    }
}

#[cfg(test)]
#[path = "staging_root_tests.rs"]
mod tests;
