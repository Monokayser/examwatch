//! SQLite metadata store (WAL mode). All queries are tiny and indexed, so they
//! run inline under a mutex; media bytes never go through the database.

use std::path::Path;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Exam {
    pub id: i64,
    pub name: String,
    pub labs: Vec<String>,
    pub opened_us: i64,
    pub closed_us: Option<i64>,
    pub opened_by: String,
    pub roster_enforced: bool,
    pub roster_size: i64,
    pub purged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Claim {
    pub student_id: String,
    pub machine_id: String,
    pub pc_name: String,
    pub lab_id: String,
    pub claimed_us: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub id: i64,
    pub exam_id: i64,
    pub student_id: String,
    pub machine_id: String,
    pub lab_id: String,
    #[serde(skip)]
    pub path: String,
    #[serde(skip)]
    pub prefix: Vec<u8>,
    pub start_us: i64,
    pub end_us: i64,
    pub frames: i64,
    pub bytes: i64,
}

#[derive(Debug, PartialEq)]
pub enum ClaimOutcome {
    Ok,
    NotOnRoster,
    TakenBy { pc_name: String },
    MachineHasOther { student_id: String },
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS teachers (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    pw_hash TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    created_us INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS exams (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    labs TEXT NOT NULL,
    opened_us INTEGER NOT NULL,
    closed_us INTEGER,
    opened_by TEXT NOT NULL,
    roster_enforced INTEGER NOT NULL DEFAULT 0,
    key_wrapped BLOB,
    purged INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS roster (
    exam_id INTEGER NOT NULL REFERENCES exams(id) ON DELETE CASCADE,
    student_id TEXT NOT NULL,
    PRIMARY KEY (exam_id, student_id)
);
CREATE TABLE IF NOT EXISTS claims (
    id INTEGER PRIMARY KEY,
    exam_id INTEGER NOT NULL REFERENCES exams(id) ON DELETE CASCADE,
    student_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    pc_name TEXT NOT NULL,
    lab_id TEXT NOT NULL,
    claimed_us INTEGER NOT NULL,
    released INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS claims_student ON claims(exam_id, student_id) WHERE released = 0;
CREATE UNIQUE INDEX IF NOT EXISTS claims_machine ON claims(exam_id, machine_id) WHERE released = 0;
CREATE TABLE IF NOT EXISTS segments (
    id INTEGER PRIMARY KEY,
    exam_id INTEGER NOT NULL REFERENCES exams(id) ON DELETE CASCADE,
    student_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    lab_id TEXT NOT NULL,
    path TEXT NOT NULL,
    prefix BLOB NOT NULL,
    start_us INTEGER NOT NULL,
    end_us INTEGER NOT NULL,
    frames INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS segments_by_student ON segments(exam_id, student_id, start_us);
CREATE TABLE IF NOT EXISTS machines (
    machine_id TEXT PRIMARY KEY,
    pc_name TEXT NOT NULL,
    lab_id TEXT NOT NULL,
    last_ip TEXT NOT NULL,
    encoder TEXT NOT NULL,
    gpu TEXT NOT NULL,
    first_seen_us INTEGER NOT NULL,
    last_seen_us INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS subnet_labs (
    subnet TEXT PRIMARY KEY,
    lab_id TEXT NOT NULL,
    assigned_by TEXT NOT NULL,
    assigned_us INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY,
    ts_us INTEGER NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    detail TEXT NOT NULL
);
"#;

fn now() -> i64 {
    ew_proto::now_us() as i64
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Self { conn: Mutex::new(conn) }
    }

    // ---- teachers ----------------------------------------------------------

    pub fn upsert_teacher(&self, username: &str, pw_hash: &str, is_admin: bool) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO teachers (username, pw_hash, is_admin, created_us) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(username) DO UPDATE SET pw_hash = excluded.pw_hash, is_admin = excluded.is_admin",
            params![username, pw_hash, is_admin as i64, now()],
        )?;
        Ok(())
    }

    pub fn teacher_hash(&self, username: &str) -> Result<Option<(String, String, bool)>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT username, pw_hash, is_admin FROM teachers WHERE username = ?1",
                params![username],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
            )
            .optional()?)
    }

    pub fn teacher_count(&self) -> Result<i64> {
        Ok(self.conn.lock().query_row("SELECT COUNT(*) FROM teachers", [], |r| r.get(0))?)
    }

    // ---- exams -------------------------------------------------------------

    /// Opens an exam on the given labs. Fails if any of those labs already has one open.
    pub fn open_exam(
        &self,
        name: &str,
        labs: &[String],
        opened_by: &str,
        roster: &[String],
        key_wrapped: &[u8],
    ) -> Result<i64> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("SELECT labs FROM exams WHERE closed_us IS NULL")?;
            let open: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for l in open {
                let busy: Vec<String> = serde_json::from_str(&l)?;
                if let Some(clash) = busy.iter().find(|b| labs.contains(b)) {
                    bail!("lab {clash} already has an exam open");
                }
            }
        }
        tx.execute(
            "INSERT INTO exams (name, labs, opened_us, opened_by, roster_enforced, key_wrapped)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![name, serde_json::to_string(labs)?, now(), opened_by, !roster.is_empty() as i64, key_wrapped],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare("INSERT OR IGNORE INTO roster (exam_id, student_id) VALUES (?1, ?2)")?;
            for s in roster {
                ins.execute(params![id, s])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn close_exam(&self, exam_id: i64) -> Result<bool> {
        let n = self.conn.lock().execute(
            "UPDATE exams SET closed_us = ?2 WHERE id = ?1 AND closed_us IS NULL",
            params![exam_id, now()],
        )?;
        Ok(n == 1)
    }

    fn exam_from_row(r: &rusqlite::Row) -> rusqlite::Result<Exam> {
        let labs: String = r.get(2)?;
        Ok(Exam {
            id: r.get(0)?,
            name: r.get(1)?,
            labs: serde_json::from_str(&labs).unwrap_or_default(),
            opened_us: r.get(3)?,
            closed_us: r.get(4)?,
            opened_by: r.get(5)?,
            roster_enforced: r.get::<_, i64>(6)? != 0,
            roster_size: r.get(7)?,
            purged: r.get::<_, i64>(8)? != 0,
        })
    }

    const EXAM_COLS: &'static str = "id, name, labs, opened_us, closed_us, opened_by, roster_enforced,
         (SELECT COUNT(*) FROM roster WHERE roster.exam_id = exams.id), purged";

    pub fn open_exams(&self) -> Result<Vec<Exam>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("SELECT {} FROM exams WHERE closed_us IS NULL ORDER BY id", Self::EXAM_COLS))?;
        let v = stmt.query_map([], Self::exam_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn recent_exams(&self, limit: i64) -> Result<Vec<Exam>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("SELECT {} FROM exams ORDER BY id DESC LIMIT ?1", Self::EXAM_COLS))?;
        let v = stmt.query_map(params![limit], Self::exam_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn exam(&self, id: i64) -> Result<Option<Exam>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(&format!("SELECT {} FROM exams WHERE id = ?1", Self::EXAM_COLS), params![id], Self::exam_from_row)
            .optional()?)
    }

    pub fn exam_key(&self, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .lock()
            .query_row("SELECT key_wrapped FROM exams WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn on_roster(&self, exam_id: i64, student_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT 1 FROM roster WHERE exam_id = ?1 AND student_id = ?2",
                params![exam_id, student_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    // ---- claims (Student ID ↔ PC) ------------------------------------------

    pub fn claim(
        &self,
        exam_id: i64,
        roster_enforced: bool,
        student_id: &str,
        machine_id: &str,
        pc_name: &str,
        lab_id: &str,
    ) -> Result<ClaimOutcome> {
        if roster_enforced && !self.on_roster(exam_id, student_id)? {
            return Ok(ClaimOutcome::NotOnRoster);
        }
        let conn = self.conn.lock();
        let by_student: Option<(String, String)> = conn
            .query_row(
                "SELECT machine_id, pc_name FROM claims WHERE exam_id = ?1 AND student_id = ?2 AND released = 0",
                params![exam_id, student_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((m, pc)) = by_student {
            // Same PC re-claiming after a reconnect or agent restart is fine.
            return Ok(if m == machine_id { ClaimOutcome::Ok } else { ClaimOutcome::TakenBy { pc_name: pc } });
        }
        let by_machine: Option<String> = conn
            .query_row(
                "SELECT student_id FROM claims WHERE exam_id = ?1 AND machine_id = ?2 AND released = 0",
                params![exam_id, machine_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = by_machine {
            return Ok(ClaimOutcome::MachineHasOther { student_id: other });
        }
        conn.execute(
            "INSERT INTO claims (exam_id, student_id, machine_id, pc_name, lab_id, claimed_us) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![exam_id, student_id, machine_id, pc_name, lab_id, now()],
        )?;
        Ok(ClaimOutcome::Ok)
    }

    pub fn release_claim(&self, exam_id: i64, machine_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let sid: Option<String> = conn
            .query_row(
                "SELECT student_id FROM claims WHERE exam_id = ?1 AND machine_id = ?2 AND released = 0",
                params![exam_id, machine_id],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "UPDATE claims SET released = 1 WHERE exam_id = ?1 AND machine_id = ?2 AND released = 0",
            params![exam_id, machine_id],
        )?;
        Ok(sid)
    }

    pub fn active_claim_for_machine(&self, exam_id: i64, machine_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT student_id FROM claims WHERE exam_id = ?1 AND machine_id = ?2 AND released = 0",
                params![exam_id, machine_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn claims(&self, exam_id: i64) -> Result<Vec<Claim>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT student_id, machine_id, pc_name, lab_id, MIN(claimed_us) FROM claims
             WHERE exam_id = ?1 GROUP BY student_id ORDER BY student_id",
        )?;
        let v = stmt
            .query_map(params![exam_id], |r| {
                Ok(Claim {
                    student_id: r.get(0)?,
                    machine_id: r.get(1)?,
                    pc_name: r.get(2)?,
                    lab_id: r.get(3)?,
                    claimed_us: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    // ---- machines ----------------------------------------------------------

    pub fn touch_machine(&self, machine_id: &str, pc_name: &str, lab_id: &str, ip: &str, encoder: &str, gpu: &str) -> Result<()> {
        let t = now();
        self.conn.lock().execute(
            "INSERT INTO machines (machine_id, pc_name, lab_id, last_ip, encoder, gpu, first_seen_us, last_seen_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(machine_id) DO UPDATE SET pc_name = excluded.pc_name, lab_id = excluded.lab_id,
               last_ip = excluded.last_ip, encoder = excluded.encoder, gpu = excluded.gpu, last_seen_us = excluded.last_seen_us",
            params![machine_id, pc_name, lab_id, ip, encoder, gpu, t],
        )?;
        Ok(())
    }

    /// Known PCs (id, name, lab, last IP), so the dashboard can show a PC that never came online.
    pub fn known_machines(&self) -> Result<Vec<(String, String, String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT machine_id, pc_name, lab_id, last_ip FROM machines ORDER BY lab_id, pc_name")?;
        let v = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn set_machine_lab(&self, machine_id: &str, lab_id: &str) -> Result<()> {
        self.conn.lock().execute("UPDATE machines SET lab_id = ?2 WHERE machine_id = ?1", params![machine_id, lab_id])?;
        Ok(())
    }

    // ---- learned subnet → lab ("Assign to lab" in the dashboard) -------------

    pub fn subnet_labs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT subnet, lab_id FROM subnet_labs")?;
        let v = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn set_subnet_lab(&self, subnet: &str, lab_id: &str, by: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO subnet_labs (subnet, lab_id, assigned_by, assigned_us) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(subnet) DO UPDATE SET lab_id = excluded.lab_id, assigned_by = excluded.assigned_by, assigned_us = excluded.assigned_us",
            params![subnet, lab_id, by, now()],
        )?;
        Ok(())
    }

    // ---- segments ----------------------------------------------------------

    pub fn insert_segment(&self, s: &Segment) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO segments (exam_id, student_id, machine_id, lab_id, path, prefix, start_us, end_us, frames, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![s.exam_id, s.student_id, s.machine_id, s.lab_id, s.path, s.prefix, s.start_us, s.end_us, s.frames, s.bytes],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_segment(&self, id: i64, end_us: i64, frames: i64, bytes: i64) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE segments SET end_us = ?2, frames = ?3, bytes = ?4 WHERE id = ?1",
            params![id, end_us, frames, bytes],
        )?;
        Ok(())
    }

    fn seg_from_row(r: &rusqlite::Row) -> rusqlite::Result<Segment> {
        Ok(Segment {
            id: r.get(0)?,
            exam_id: r.get(1)?,
            student_id: r.get(2)?,
            machine_id: r.get(3)?,
            lab_id: r.get(4)?,
            path: r.get(5)?,
            prefix: r.get(6)?,
            start_us: r.get(7)?,
            end_us: r.get(8)?,
            frames: r.get(9)?,
            bytes: r.get(10)?,
        })
    }

    pub fn segments(&self, exam_id: i64, student_id: &str) -> Result<Vec<Segment>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, exam_id, student_id, machine_id, lab_id, path, prefix, start_us, end_us, frames, bytes
             FROM segments WHERE exam_id = ?1 AND student_id = ?2 ORDER BY start_us",
        )?;
        let v = stmt.query_map(params![exam_id, student_id], Self::seg_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn segment(&self, id: i64) -> Result<Option<Segment>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT id, exam_id, student_id, machine_id, lab_id, path, prefix, start_us, end_us, frames, bytes
                 FROM segments WHERE id = ?1",
                params![id],
                Self::seg_from_row,
            )
            .optional()?)
    }

    /// Per-student recording summary for one exam.
    pub fn recording_summary(&self, exam_id: i64) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT student_id, lab_id, MIN(start_us), MAX(end_us), COUNT(*), SUM(bytes), MAX(machine_id)
             FROM segments WHERE exam_id = ?1 GROUP BY student_id ORDER BY lab_id, student_id",
        )?;
        let v = stmt
            .query_map(params![exam_id], |r| {
                Ok(serde_json::json!({
                    "student_id": r.get::<_, String>(0)?,
                    "lab_id": r.get::<_, String>(1)?,
                    "start_us": r.get::<_, i64>(2)?,
                    "end_us": r.get::<_, i64>(3)?,
                    "segments": r.get::<_, i64>(4)?,
                    "bytes": r.get::<_, i64>(5)?,
                    "machine_id": r.get::<_, String>(6)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    // ---- retention ---------------------------------------------------------

    /// Exams closed before `cutoff_us` that still hold data.
    pub fn exams_to_purge(&self, cutoff_us: i64) -> Result<Vec<i64>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id FROM exams WHERE closed_us IS NOT NULL AND closed_us < ?1 AND purged = 0")?;
        let v = stmt.query_map(params![cutoff_us], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// Drops the exam key and every segment row. Files are deleted by the caller first.
    pub fn mark_purged(&self, exam_id: i64) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE exam_id = ?1", params![exam_id])?;
        tx.execute("UPDATE exams SET key_wrapped = NULL, purged = 1 WHERE id = ?1", params![exam_id])?;
        tx.commit()?;
        Ok(())
    }

    // ---- audit -------------------------------------------------------------

    pub fn audit(&self, actor: &str, action: &str, detail: &str) {
        let r = self.conn.lock().execute(
            "INSERT INTO audit (ts_us, actor, action, detail) VALUES (?1, ?2, ?3, ?4)",
            params![now(), actor, action, detail],
        );
        if let Err(e) = r {
            tracing::error!("audit write failed: {e}");
        }
    }

    pub fn audit_recent(&self, limit: i64) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT ts_us, actor, action, detail FROM audit ORDER BY id DESC LIMIT ?1")?;
        let v = stmt
            .query_map(params![limit], |r| {
                Ok(serde_json::json!({
                    "ts_us": r.get::<_, i64>(0)?, "actor": r.get::<_, String>(1)?,
                    "action": r.get::<_, String>(2)?, "detail": r.get::<_, String>(3)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lab_cannot_have_two_open_exams() {
        let db = Db::memory();
        let a = db.open_exam("Midterm", &labs(&["anim1", "game"]), "t", &[], b"k").unwrap();
        assert!(db.open_exam("Other", &labs(&["game"]), "t", &[], b"k").is_err());
        db.open_exam("Other", &labs(&["visarts"]), "t", &[], b"k").unwrap();
        assert!(db.close_exam(a).unwrap());
        assert!(!db.close_exam(a).unwrap(), "closing twice is a no-op");
        db.open_exam("Again", &labs(&["game"]), "t", &[], b"k").unwrap();
    }

    #[test]
    fn claim_rules() {
        let db = Db::memory();
        let roster = labs(&["S1", "S2"]);
        let e = db.open_exam("Final", &labs(&["anim1"]), "t", &roster, b"k").unwrap();
        assert_eq!(db.claim(e, true, "S9", "m1", "PC1", "anim1").unwrap(), ClaimOutcome::NotOnRoster);
        assert_eq!(db.claim(e, true, "S1", "m1", "PC1", "anim1").unwrap(), ClaimOutcome::Ok);
        // reconnect on same PC
        assert_eq!(db.claim(e, true, "S1", "m1", "PC1", "anim1").unwrap(), ClaimOutcome::Ok);
        // duplicate on another PC
        assert_eq!(
            db.claim(e, true, "S1", "m2", "PC2", "anim1").unwrap(),
            ClaimOutcome::TakenBy { pc_name: "PC1".into() }
        );
        // same PC trying a second ID
        assert_eq!(
            db.claim(e, true, "S2", "m1", "PC1", "anim1").unwrap(),
            ClaimOutcome::MachineHasOther { student_id: "S1".into() }
        );
        // teacher releases PC1, student moves to PC2
        assert_eq!(db.release_claim(e, "m1").unwrap(), Some("S1".into()));
        assert_eq!(db.claim(e, true, "S1", "m2", "PC2", "anim1").unwrap(), ClaimOutcome::Ok);
    }

    #[test]
    fn purge_removes_key_and_segments() {
        let db = Db::memory();
        let e = db.open_exam("X", &labs(&["anim1"]), "t", &[], b"key").unwrap();
        db.insert_segment(&Segment {
            id: 0, exam_id: e, student_id: "S1".into(), machine_id: "m".into(), lab_id: "anim1".into(),
            path: "p".into(), prefix: vec![0; 4], start_us: 1, end_us: 2, frames: 1, bytes: 1,
        })
        .unwrap();
        db.close_exam(e).unwrap();
        assert_eq!(db.exams_to_purge(i64::MAX).unwrap(), vec![e]);
        db.mark_purged(e).unwrap();
        assert!(db.exam_key(e).unwrap().is_none());
        assert!(db.segments(e, "S1").unwrap().is_empty());
        assert!(db.exams_to_purge(i64::MAX).unwrap().is_empty());
    }
}
