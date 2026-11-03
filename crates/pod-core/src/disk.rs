//! Free disk space, so large writes stop before the data volume is full
//! (decision `pod-storage-limits`): once it is, every pod's SQLite writes
//! fail, sign-ins and sessions included.

use std::path::{Path, PathBuf};

/// Default free space kept on the data volume (`CONSTRUCT_MIN_FREE_DISK_BYTES`).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 100 * 1024 * 1024;

/// Bytes an unprivileged process can still write on the filesystem holding
/// `path`; `None` when unknown (not Unix, or the call failed).
pub fn free_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let s = rustix::fs::statvfs(path).ok()?;
        Some(s.f_bavail.saturating_mul(s.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Refuses large writes when the filesystem under `dir` has less than
/// `min_free` bytes left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskGuard {
    pub dir: PathBuf,
    pub min_free: u64,
}

impl DiskGuard {
    pub fn new(dir: impl Into<PathBuf>, min_free: u64) -> Self {
        Self {
            dir: dir.into(),
            min_free,
        }
    }

    /// True when a write of `extra` bytes would leave less than `min_free`.
    /// Unknown free space counts as enough (the guard is a backstop, not the
    /// only limit: quotas still apply).
    pub fn too_low_for(&self, extra: u64) -> bool {
        match free_bytes(&self.dir) {
            Some(free) => free < self.min_free.saturating_add(extra),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_the_temp_dir() {
        let dir = std::env::temp_dir();
        if cfg!(unix) {
            assert!(free_bytes(&dir).is_some_and(|b| b > 0));
            assert!(DiskGuard::new(&dir, u64::MAX).too_low_for(0));
        }
        assert!(!DiskGuard::new(&dir, 0).too_low_for(0));
    }
}
