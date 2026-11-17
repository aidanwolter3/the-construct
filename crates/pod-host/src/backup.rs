//! Backups of a host's data (docs/server.md "Backups and restore", decision
//! `public-links-share-the-closure`): a consistent copy of `host.db` (pods,
//! credentials, the shelf, public links) and of every pod's `server.db`,
//! made while the server runs, and put back when the server next starts.
//!
//! The copy is `VACUUM INTO` (SQLite's own online copy: one transaction, so
//! each file is whole even while the server writes). Put a backup's folder
//! at `<data dir>/restore` and the next start moves the current data aside
//! (`<data dir>/replaced-<unix time>`) and the backup in its place, before
//! anything opens a database.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder a backup is restored from on start.
pub const RESTORE_DIR: &str = "restore";

/// One database copied: from `from` to `to`.
fn copy_db(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        pod_core::util::create_private_dir(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let conn = rusqlite::Connection::open_with_flags(
        from,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("opening {}: {e}", from.display()))?;
    conn.busy_timeout(Duration::from_secs(30))
        .map_err(|e| e.to_string())?;
    let dest = to
        .to_str()
        .ok_or_else(|| format!("{} is not a UTF-8 path", to.display()))?;
    conn.execute("VACUUM INTO ?1", [dest])
        .map_err(|e| format!("copying {}: {e}", from.display()))?;
    Ok(())
}

/// The pods' folders under `dir` that hold a database: `(id, server.db)`.
fn pod_dbs(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let pods = dir.join("pods");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&pods) else {
        return Ok(out);
    };
    for e in entries {
        let e = e.map_err(|e| e.to_string())?;
        let db = e.path().join("server.db");
        if let Some(id) = e.file_name().to_str()
            && db.is_file()
        {
            out.push((id.to_owned(), db));
        }
    }
    out.sort();
    Ok(out)
}

/// Copies the host's data in `dir` to `dest` (a new or empty folder):
/// `host.db` and `pods/<id>/server.db` for each pod. Answers the files
/// written.
pub fn backup(dir: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    if dest.read_dir().is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{} is not empty", dest.display()));
    }
    let host = dir.join("host.db");
    if !host.is_file() {
        return Err(format!("no host data in {}", dir.display()));
    }
    pod_core::util::create_private_dir(dest)
        .map_err(|e| format!("creating {}: {e}", dest.display()))?;
    let mut written = Vec::new();
    let to = dest.join("host.db");
    copy_db(&host, &to)?;
    written.push(to);
    for (id, db) in pod_dbs(dir)? {
        let to = dest.join("pods").join(&id).join("server.db");
        copy_db(&db, &to)?;
        written.push(to);
    }
    Ok(written)
}

/// Puts a backup waiting at `<dir>/restore` in place of the data in `dir`
/// (which moves to `<dir>/replaced-<unix time>`). Call before anything
/// opens a database. Answers where the replaced data went, or `None` when
/// no backup waits.
pub fn restore_pending(dir: &Path) -> Result<Option<PathBuf>, String> {
    let restore = dir.join(RESTORE_DIR);
    if !restore.join("host.db").is_file() {
        return Ok(None);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let aside = dir.join(format!("replaced-{now}"));
    pod_core::util::create_private_dir(&aside)
        .map_err(|e| format!("creating {}: {e}", aside.display()))?;
    let mv = |from: &Path, to: &Path| -> Result<(), String> {
        std::fs::rename(from, to)
            .map_err(|e| format!("moving {} to {}: {e}", from.display(), to.display()))
    };
    // The current data, with its write-ahead logs, goes aside whole.
    for name in ["host.db", "host.db-wal", "host.db-shm", "pods"] {
        let p = dir.join(name);
        if p.exists() {
            mv(&p, &aside.join(name))?;
        }
    }
    mv(&restore.join("host.db"), &dir.join("host.db"))?;
    if restore.join("pods").exists() {
        mv(&restore.join("pods"), &dir.join("pods"))?;
    }
    let _ = std::fs::remove_dir_all(&restore);
    Ok(Some(aside))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(db: &Path, text: &str) {
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let c = rusqlite::Connection::open(db).unwrap();
        c.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE IF NOT EXISTS t (v TEXT);")
            .unwrap();
        c.execute("DELETE FROM t", []).unwrap();
        c.execute("INSERT INTO t VALUES (?1)", [text]).unwrap();
    }

    fn read(db: &Path) -> String {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row("SELECT v FROM t", [], |r| r.get(0))
            .unwrap()
    }

    /// A backup made while the data is in use comes back whole on the next
    /// start, and the data it replaced is kept aside.
    #[test]
    fn a_backup_restores_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let (host, pod) = (
            dir.path().join("host.db"),
            dir.path().join("pods/p1/server.db"),
        );
        note(&host, "links v1");
        note(&pod, "recipes v1");
        // An open writer (the running server) doesn't stop it.
        let _writer = rusqlite::Connection::open(&host).unwrap();
        let dest = dir.path().join("backups/2026-11-17");
        let written = backup(dir.path(), &dest).unwrap();
        assert_eq!(written.len(), 2);
        assert!(backup(dir.path(), &dest).is_err(), "never over a backup");

        note(&host, "links v2");
        note(&pod, "recipes v2");
        assert!(restore_pending(dir.path()).unwrap().is_none());
        std::fs::rename(&dest, dir.path().join(RESTORE_DIR)).unwrap();
        let aside = restore_pending(dir.path()).unwrap().unwrap();
        assert_eq!(read(&host), "links v1");
        assert_eq!(read(&pod), "recipes v1");
        assert_eq!(read(&aside.join("host.db")), "links v2");
        assert!(!dir.path().join(RESTORE_DIR).exists());
    }
}
