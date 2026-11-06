//! Classifying one entry found by a filesystem scan, and walks that report
//! directories they could not read.
//!
//! A scanner that treats every failure as "not there" produces a partial tree
//! that looks complete, so only `NotFound` means an entry vanished.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Classifies one operation on a single listed scan entry: `NotFound` means a
/// concurrent publisher renamed or removed that entry after it was listed, so
/// it yields `None`. Every other error still fails the scan. Apply this only
/// to individual entries, never to the root being scanned; a stat or open of an
/// entry goes through [`listed_entry`], which also rides out Windows denials.
pub fn entry_if_present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Runs one stat or open on a listed scan entry, classifying a vanished entry
/// as `None`.
///
/// On Windows such a call can return `ERROR_ACCESS_DENIED` or
/// `ERROR_SHARING_VIOLATION` instead of `NotFound` while a concurrent publisher
/// or scanner holds the entry mid-delete. That says nothing about whether the
/// entry is gone, so the call is repeated on the #1757 ladder (50, 100, 250 and
/// 500 ms): it only counts as vanished once it really reports `NotFound`, and
/// an entry that stays denied fails the scan naming its path. The classifier is
/// always `false` on Linux and macOS, so there a denial fails on first attempt.
pub fn listed_entry<T>(
    path: &Path,
    op: impl FnMut(&Path) -> io::Result<T>,
) -> io::Result<Option<T>> {
    listed_entry_with(
        path,
        op,
        kernal_api::platform::fs::replacement::is_transient_share_error,
    )
}

/// [`listed_entry`] with the classifier injected, so the retry contract is
/// testable off Windows.
pub fn listed_entry_with<T>(
    path: &Path,
    mut op: impl FnMut(&Path) -> io::Result<T>,
    is_transient: impl Fn(&io::Error) -> bool,
) -> io::Result<Option<T>> {
    const DELAYS: [Duration; 4] = [
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(250),
        Duration::from_millis(500),
    ];
    let mut delays = DELAYS.iter();
    loop {
        match op(path) {
            Err(error) if is_transient(&error) => match delays.next() {
                Some(delay) => std::thread::sleep(*delay),
                None => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "listed entry stayed inaccessible: {}: {error}",
                            path.display()
                        ),
                    ));
                }
            },
            result => return entry_if_present(result),
        }
    }
}

/// Runs a walk that also reports directories it could not read.
///
/// The kernal-api walk (through at least 0.1.40) drops jwalk's
/// `read_children_error`, so a directory it cannot open arrives as `Ok` with no
/// children, indistinguishable from an empty one (#2092). Traversal is
/// depth-first, so a directory with no children is one whose next entry is not
/// deeper, and only those are reopened: a tree of non-empty directories pays no
/// extra syscalls. A pruned directory is never yielded, so it is never probed.
///
/// A failure is an extra `Err` item naming the directory, after its entry.
/// Remove this once the facade reports the error itself.
pub fn walk_reporting_unreadable(
    walk: kernal_api::platform::fs::DirectoryWalk,
) -> impl Iterator<Item = io::Result<kernal_api::platform::fs::DirectoryEntry>> {
    let mut entries = walk.walk().peekable();
    let mut unreadable: Option<io::Error> = None;
    std::iter::from_fn(move || {
        if let Some(error) = unreadable.take() {
            return Some(Err(error));
        }
        let item = entries.next()?;
        if let Ok(entry) = &item {
            let childless = match entries.peek() {
                Some(Ok(next)) => next.depth() <= entry.depth(),
                Some(Err(_)) | None => true,
            };
            if entry.is_directory() && childless {
                unreadable = probe_directory(entry.path()).err();
            }
        }
        Some(item)
    })
}

fn probe_directory(path: &Path) -> io::Result<()> {
    listed_entry(path, |directory| std::fs::read_dir(directory))
        .map(drop)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("unreadable directory: {}: {error}", path.display()),
            )
        })
}

#[cfg(test)]
mod tests;
