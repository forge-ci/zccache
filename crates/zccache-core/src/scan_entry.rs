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
