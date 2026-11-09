//! Shared conservative filesystem timestamp policy.

use std::time::Duration;

// No filesystem-type probe on the hot path. Windows includes FAT's two-second
// ticks; Linux/macOS use 10 ms conservatively for ext4/APFS. Hash-start time,
// rather than completion time, keeps a long read from aging out a racy entry.
pub const HASH_TIMESTAMP_GRANULARITY: Duration = if crate::host::is_windows() {
    Duration::from_secs(2)
} else {
    Duration::from_millis(10)
};

