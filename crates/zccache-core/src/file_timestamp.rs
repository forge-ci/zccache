//! Shared conservative filesystem timestamp policy.

use std::time::Duration;

// No filesystem-type probe on the hot path. Windows includes FAT's two-second
// ticks; Linux/macOS use 10 ms conservatively for ext4/APFS. Hash-start time,
// rather than completion time, keeps a long read from aging out a racy entry.
pub const HASH_TIMESTAMP_GRANULARITY: Duration = granularity(crate::host::is_windows());

const fn granularity(windows: bool) -> Duration {
    if windows {
        Duration::from_secs(2)
    } else {
        Duration::from_millis(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_granularity_covers_fat_two_second_ticks() {
        assert!(granularity(true) >= Duration::from_secs(2));
        assert_eq!(granularity(false), Duration::from_millis(10));
        assert_eq!(
            HASH_TIMESTAMP_GRANULARITY,
            granularity(crate::host::is_windows())
        );
    }
}
