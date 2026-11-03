//! Small helpers shared by the core and adapters.

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Create `dir` (and its parents) if missing, and make it private to the
/// server's user (0700 on Unix; a failure to change the mode only warns): it
/// holds databases with sessions and upstream credentials.
pub fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            // Not ours to change (an operator's shared directory): say so,
            // but keep serving.
            tracing::warn!("could not make {} private (mode 0700): {e}", dir.display());
        }
    }
    Ok(())
}

/// Make the SQLite database `path` private (0600 on Unix) before SQLite
/// opens it: a missing file is created empty with that mode, an existing one
/// (and its `-wal`/`-shm` files) is changed to it (a failure to change the
/// mode only warns). SQLite gives the `-wal`,
/// `-shm` and `-journal` files it creates the database file's mode.
pub fn private_db_file(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        for suffix in ["", "-wal", "-shm"] {
            let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            match std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!("could not make {} private (mode 0600): {e}", file.display())
                }
                Ok(()) => {}
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dirs_and_db_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        super::create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        let db = dir.join("x.db");
        super::private_db_file(&db).unwrap();
        assert_eq!(mode(&db), 0o600);
        // An existing world-readable file (and its WAL) is tightened.
        let wal = dir.join("x.db-wal");
        std::fs::write(&wal, b"").unwrap();
        for p in [&db, &wal] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        super::private_db_file(&db).unwrap();
        assert_eq!((mode(&db), mode(&wal)), (0o600, 0o600));
    }
}
