//! Owned namespace retirement: rename under the namespace gate, sweep later.
use super::{check_namespace, namespace_gate, KvError, KvResult, KvStore};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use zccache_core::path::NormalizedPath;

const RETIRED: &str = ".retired";
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Names already detached from every live namespace. Drop leaves durable
/// recovery work; it never recursively deletes on the dropping thread.
#[must_use = "sweep retired names outside publication exclusion"]
pub struct DetachedKvNamespace {
    root: NormalizedPath,
    paths: Vec<NormalizedPath>,
}

/// Successful unlinks are credited independently from failed/deferred work.
#[derive(Default, Debug)]
pub struct KvSweepReport {
    pub reclaimed_bytes: u64,
    pub failures: usize,
    pub deferred: bool,
}

impl KvStore {
    /// Retire a namespace with a mandatory acquisition budget. New puts use
    /// the recreated live name; this handle owns only its retired predecessor.
    pub fn detach_namespace_for(
        &self,
        namespace: &str,
        bound: Duration,
    ) -> KvResult<DetachedKvNamespace> {
        check_namespace(namespace)?;
        deadline(bound)?;
        let dir = self.namespace_dir(namespace);
        let root = self.kv_root().join(RETIRED);
        let mut detached = DetachedKvNamespace {
            root,
            paths: Vec::new(),
        };
        let _gate = namespace_gate(dir.as_path()).blocking_write_for(bound);
        match fs::symlink_metadata(dir.as_path()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(detached),
            Err(error) => return Err(error.into()),
            Ok(_) => directory(dir.as_path())?,
        }
        directory(self.kv_root().as_path())?;
        match fs::create_dir(&detached.root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        directory(&detached.root)?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = detached.root.join(format!(
            "{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        kernal_api::platform::fs::replacement::rename_generation(dir.as_path(), &path)?;
        detached.paths.push(path);
        Ok(detached)
    }

    /// Retry names retained by interrupted or failed cleanup. The same
    /// deadline covers enumeration and recursive deletion.
    pub fn sweep_retired_namespaces_for(&self, bound: Duration) -> KvResult<KvSweepReport> {
        let until = deadline(bound)?;
        let root = self.kv_root().join(RETIRED);
        match fs::symlink_metadata(&root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(KvSweepReport::default())
            }
            Err(error) => return Err(error.into()),
            Ok(_) => directory(&root)?,
        }
        let mut detached = DetachedKvNamespace {
            root,
            paths: Vec::new(),
        };
        let mut deferred = false;
        for entry in fs::read_dir(&detached.root)? {
            if Instant::now() >= until {
                deferred = true;
                break;
            }
            detached.paths.push(entry?.path().into());
        }
        let mut report = detached.sweep_until(until)?;
        report.deferred |= deferred;
        Ok(report)
    }
}

impl DetachedKvNamespace {
    /// Run on the blocking lane after publication/namespace guards are gone.
    /// Deadlines bound work between syscalls, not a stalled kernel syscall.
    pub fn sweep_for(self, bound: Duration) -> KvResult<KvSweepReport> {
        self.sweep_until(deadline(bound)?)
    }

    fn sweep_until(self, until: Instant) -> KvResult<KvSweepReport> {
        let mut report = KvSweepReport::default();
        if self.paths.is_empty() {
            return Ok(report);
        }
        match directory(&self.root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error.into()),
        }
        for path in self.paths {
            if path.as_path().parent() != Some(self.root.as_path()) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "retired namespace escaped its owner",
                )
                .into());
            }
            sweep_tree(&path, until, &mut report);
            if report.deferred {
                break;
            }
        }
        Ok(report)
    }
}

fn deadline(bound: Duration) -> KvResult<Instant> {
    Instant::now().checked_add(bound).ok_or_else(|| {
        KvError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unrepresentable namespace retirement budget",
        ))
    })
}

fn directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || kernal_api::platform::fs::classify(path)? != kernal_api::platform::fs::LinkKind::Regular
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing namespace retirement through linked/reparse or non-directory path",
        ));
    }
    Ok(())
}

fn fail(path: &Path, error: io::Error, report: &mut KvSweepReport) {
    if report.failures < 8 {
        tracing::warn!(path = %path.display(), %error, "retired namespace cleanup will retry");
    }
    report.failures += 1;
}

fn sweep_tree(path: &Path, until: Instant, report: &mut KvSweepReport) {
    if Instant::now() >= until {
        report.deferred = true;
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            fail(path, error, report);
            return;
        }
    };
    let kind = match kernal_api::platform::fs::classify(path) {
        Ok(kind) => kind,
        Err(error) => {
            fail(path, error, report);
            return;
        }
    };
    if kind != kernal_api::platform::fs::LinkKind::Regular {
        let result = if metadata.is_dir() {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
        if let Err(error) = result {
            fail(path, error, report);
        }
    } else if metadata.is_dir() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                fail(path, error, report);
                return;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => sweep_tree(&entry.path(), until, report),
                Err(error) => fail(path, error, report),
            }
            if report.deferred {
                return;
            }
        }
        if let Err(error) = fs::remove_dir(path) {
            if error.kind() != io::ErrorKind::NotFound {
                fail(path, error, report);
            }
        }
    } else {
        let allocated = if zccache_core::config::file_may_free_space_on_removal(path) {
            kernal_api::platform::fs::allocated_bytes(path, &metadata)
        } else {
            0
        };
        match fs::remove_file(path) {
            Ok(()) => report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(allocated),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => fail(path, error, report),
        }
    }
}
