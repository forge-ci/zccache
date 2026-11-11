//! Bounded, one-level name enumeration for live-name retirement.
//!
//! This owner neither opens child directories nor reads payloads. Its two
//! callers detach names under exclusion and later sweep typed trash outside it.
use std::fs::{self, DirEntry, ReadDir};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) struct ImmediateChildren {
    entries: ReadDir,
    started: Instant,
    bound: Duration,
}

impl ImmediateChildren {
    /// Only the staged and legacy Clear detach owners may open this capability.
    /// The budget includes iteration and the caller's work between entries.
    pub(super) fn open_for(parent: &Path, bound: Duration) -> io::Result<Self> {
        let started = Instant::now();
        expired(started, bound)?;
        let entries = fs::read_dir(parent)?;
        expired(started, bound)?;
        Ok(Self {
            entries,
            started,
            bound,
        })
    }

    pub(super) fn next_entry(&mut self) -> io::Result<Option<DirEntry>> {
        expired(self.started, self.bound)?;
        let entry = self.entries.next().transpose()?;
        expired(self.started, self.bound)?;
        Ok(entry)
    }
}

fn expired(started: Instant, bound: Duration) -> io::Result<()> {
    if started.elapsed() >= bound {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "immediate detach enumeration expired",
        ));
    }
    Ok(())
}
