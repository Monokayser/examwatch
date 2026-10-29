//! Retention: recordings are deleted `retention_hours` after their exam closes.
//! Files go first, then the wrapped exam key, so even a file that a crash left
//! behind can no longer be decrypted.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

pub fn purge_due(db: &Db, rec_root: &Path, retention_hours: u64) -> usize {
    let cutoff = ew_proto::now_us() as i64 - (retention_hours as i64) * 3_600_000_000;
    let ids = match db.exams_to_purge(cutoff) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("retention query failed: {e:#}");
            return 0;
        }
    };
    let mut n = 0;
    for id in ids {
        let dir = rec_root.join(id.to_string());
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                // Keep the key so we can retry; the data stays encrypted meanwhile.
                tracing::error!(exam = id, "retention delete failed: {e}");
                continue;
            }
        }
        match db.mark_purged(id) {
            Ok(()) => {
                db.audit("system", "retention_purge", &format!("exam {id}"));
                tracing::info!(exam = id, "recordings purged (retention {retention_hours} h)");
                n += 1;
            }
            Err(e) => tracing::error!(exam = id, "retention mark failed: {e:#}"),
        }
    }
    n
}

pub async fn run(db: Arc<Db>, rec_root: std::path::PathBuf, retention_hours: u64) {
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        let db = db.clone();
        let root = rec_root.clone();
        let _ = tokio::task::spawn_blocking(move || purge_due(&db, &root, retention_hours)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purges_only_expired_closed_exams() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let root = dir.path().join("rec");
        let open = db.open_exam("open", &["a".into()], "t", &[], b"k").unwrap();
        let closed = db.open_exam("closed", &["b".into()], "t", &[], b"k").unwrap();
        db.close_exam(closed).unwrap();
        std::fs::create_dir_all(root.join(closed.to_string()).join("S1")).unwrap();
        std::fs::write(root.join(closed.to_string()).join("S1").join("1.ews"), b"x").unwrap();
        std::fs::create_dir_all(root.join(open.to_string())).unwrap();

        // 48 h retention: nothing is old enough yet.
        assert_eq!(purge_due(&db, &root, 48), 0);
        assert!(root.join(closed.to_string()).exists());

        // retention 0 h: the closed exam goes, the open one stays.
        assert_eq!(purge_due(&db, &root, 0), 1);
        assert!(!root.join(closed.to_string()).exists());
        assert!(root.join(open.to_string()).exists());
        assert!(db.exam_key(closed).unwrap().is_none());
        assert!(db.exam_key(open).unwrap().is_some());
    }
}
