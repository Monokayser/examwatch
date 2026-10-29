//! Recording writer. One dedicated OS thread owns every open segment file, so
//! disk latency can never stall the network tasks (they only `try_send`).
//!
//! Segment file layout (`.ews`):
//! ```text
//! "EWS1" | nonce_prefix[4] | { u32 len | AES-GCM( FrameHeader[17] | H.264 AU ) }*
//! ```
//! A segment always starts at a keyframe, so each file decodes on its own.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use ew_proto::{FrameHeader, FRAME_HEADER_LEN};

use crate::crypto::{self, FrameOpener, FrameSealer, KeyBytes};
use crate::db::{Db, Segment};

const MAGIC: &[u8; 4] = b"EWS1";
const AAD: &[u8] = b"examwatch-segment-v1";

pub enum RecMsg {
    Frame { exam_id: i64, student_id: String, machine_id: String, lab_id: String, hdr: FrameHeader, data: Bytes },
    /// Student disconnected, was released, or the exam ended for them.
    Close { exam_id: i64, student_id: String },
    CloseExam { exam_id: i64 },
    /// Test hook: reply once everything queued before this is on disk.
    #[allow(dead_code)]
    Flush(SyncSender<()>),
}

#[derive(Clone)]
pub struct Recorder {
    tx: SyncSender<RecMsg>,
}

impl Recorder {
    pub fn start(db: Arc<Db>, master: KeyBytes, root: PathBuf, segment_secs: u64) -> Self {
        // ~10 s of REC frames for 200 PCs; beyond that we drop (and log) instead of growing.
        let (tx, rx) = sync_channel(4096);
        std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || RecorderThread::new(db, master, root, Duration::from_secs(segment_secs)).run(rx))
            .expect("spawn recorder thread");
        Self { tx }
    }

    /// Never blocks. Returns false if the recorder is saturated (frame dropped).
    pub fn send(&self, msg: RecMsg) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("recorder queue full, dropping a frame");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    #[cfg(test)]
    pub fn flush(&self) {
        let (tx, rx) = sync_channel(1);
        self.tx.send(RecMsg::Flush(tx)).unwrap();
        rx.recv().unwrap();
    }
}

struct OpenSegment {
    row_id: i64,
    file: BufWriter<File>,
    sealer: FrameSealer,
    opened: Instant,
    start_us: i64,
    last_us: i64,
    frames: i64,
    bytes: i64,
    last_db_update: Instant,
}

struct RecorderThread {
    db: Arc<Db>,
    master: KeyBytes,
    root: PathBuf,
    segment_len: Duration,
    keys: HashMap<i64, KeyBytes>,
    open: HashMap<(i64, String), OpenSegment>,
}

impl RecorderThread {
    fn new(db: Arc<Db>, master: KeyBytes, root: PathBuf, segment_len: Duration) -> Self {
        Self { db, master, root, segment_len, keys: HashMap::new(), open: HashMap::new() }
    }

    fn run(mut self, rx: Receiver<RecMsg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(RecMsg::Frame { exam_id, student_id, machine_id, lab_id, hdr, data }) => {
                    if let Err(e) = self.write(exam_id, &student_id, &machine_id, &lab_id, &hdr, &data) {
                        tracing::error!(exam_id, %student_id, "recording write failed: {e:#}");
                        self.open.remove(&(exam_id, student_id));
                    }
                }
                Ok(RecMsg::Close { exam_id, student_id }) => self.close(&(exam_id, student_id)),
                Ok(RecMsg::CloseExam { exam_id }) => {
                    let keys: Vec<_> = self.open.keys().filter(|k| k.0 == exam_id).cloned().collect();
                    for k in keys {
                        self.close(&k);
                    }
                    self.keys.remove(&exam_id);
                }
                Ok(RecMsg::Flush(done)) => {
                    for seg in self.open.values_mut() {
                        let _ = seg.file.flush();
                    }
                    let _ = done.send(());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let keys: Vec<_> = self.open.keys().cloned().collect();
        for k in keys {
            self.close(&k);
        }
    }

    fn key_for(&mut self, exam_id: i64) -> Result<KeyBytes> {
        if let Some(k) = self.keys.get(&exam_id) {
            return Ok(*k);
        }
        let wrapped = self.db.exam_key(exam_id)?.context("exam has no key (purged?)")?;
        let k = crypto::unwrap_key(&self.master, &wrapped)?;
        self.keys.insert(exam_id, k);
        Ok(k)
    }

    fn write(&mut self, exam_id: i64, student_id: &str, machine_id: &str, lab_id: &str, hdr: &FrameHeader, data: &[u8]) -> Result<()> {
        let map_key = (exam_id, student_id.to_string());

        // Rotate on the first keyframe after the segment length is reached.
        let rotate = self.open.get(&map_key).map_or(false, |s| hdr.is_key() && s.opened.elapsed() >= self.segment_len);
        if rotate {
            self.close(&map_key);
        }

        if !self.open.contains_key(&map_key) {
            if !hdr.is_key() {
                return Ok(()); // wait for a keyframe so the segment is decodable on its own
            }
            let key = self.key_for(exam_id)?;
            let seg = self.create(exam_id, student_id, machine_id, lab_id, hdr.pts_us as i64, &key)?;
            self.open.insert(map_key.clone(), seg);
        }

        let seg = self.open.get_mut(&map_key).unwrap();
        let mut plain = Vec::with_capacity(FRAME_HEADER_LEN + data.len());
        plain.extend_from_slice(&hdr.encode());
        plain.extend_from_slice(data);
        let sealed = seg.sealer.seal(&plain, AAD);
        seg.file.write_all(&(sealed.len() as u32).to_le_bytes())?;
        seg.file.write_all(&sealed)?;
        // Push to the OS every frame (2 fps): a server crash loses at most one frame.
        seg.file.flush()?;
        seg.frames += 1;
        seg.bytes += 4 + sealed.len() as i64;
        seg.last_us = seg.last_us.max(hdr.pts_us as i64);
        if seg.last_db_update.elapsed() >= Duration::from_secs(10) {
            self.db.update_segment(seg.row_id, seg.last_us, seg.frames, seg.bytes)?;
            seg.last_db_update = Instant::now();
        }
        Ok(())
    }

    fn create(&self, exam_id: i64, student_id: &str, machine_id: &str, lab_id: &str, start_us: i64, key: &KeyBytes) -> Result<OpenSegment> {
        let dir = self.root.join(exam_id.to_string()).join(safe_name(student_id));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{start_us}.ews"));
        let sealer = FrameSealer::new(key);
        let mut file = BufWriter::with_capacity(256 * 1024, File::create(&path)?);
        file.write_all(MAGIC)?;
        file.write_all(&sealer.prefix())?;
        let row_id = self.db.insert_segment(&Segment {
            id: 0,
            exam_id,
            student_id: student_id.to_string(),
            machine_id: machine_id.to_string(),
            lab_id: lab_id.to_string(),
            path: path.to_string_lossy().into_owned(),
            prefix: sealer.prefix().to_vec(),
            start_us,
            end_us: start_us,
            frames: 0,
            bytes: 8,
        })?;
        Ok(OpenSegment {
            row_id,
            file,
            sealer,
            opened: Instant::now(),
            start_us,
            last_us: start_us,
            frames: 0,
            bytes: 8,
            last_db_update: Instant::now(),
        })
    }

    fn close(&mut self, k: &(i64, String)) {
        if let Some(mut seg) = self.open.remove(k) {
            let _ = seg.file.flush();
            let _ = seg.file.get_ref().sync_all();
            if let Err(e) = self.db.update_segment(seg.row_id, seg.last_us.max(seg.start_us), seg.frames, seg.bytes) {
                tracing::error!("segment index update failed: {e:#}");
            }
        }
    }
}

/// Student IDs become directory names; keep only safe characters.
pub fn safe_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Decrypts a segment into the dashboard playback format: `{ FrameHeader | AU }*`.
/// A truncated tail (crash mid-write) is ignored; tampering is an error.
pub fn read_segment(path: &Path, key: &KeyBytes) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    File::open(path)?.read_to_end(&mut raw)?;
    if raw.len() < 8 || &raw[..4] != MAGIC {
        bail!("not an ExamWatch segment");
    }
    let mut opener = FrameOpener::new(key, raw[4..8].try_into().unwrap());
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 8;
    while pos + 4 <= raw.len() {
        let len = u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + len > raw.len() {
            break; // partial last frame
        }
        out.extend(opener.open(&raw[pos..pos + len], AAD)?);
        pos += len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(key: bool, pts: u64, fill: u8) -> (FrameHeader, Bytes) {
        let data = Bytes::from(vec![fill; 100 + fill as usize]);
        (FrameHeader { flags: key as u8, width: 1280, height: 720, pts_us: pts, len: data.len() as u32 }, data)
    }

    fn setup() -> (tempfile::TempDir, Arc<Db>, KeyBytes, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("t.db")).unwrap());
        let master = crypto::random_key();
        let exam_key = crypto::random_key();
        let e = db.open_exam("T", &["lab".into()], "t", &[], &crypto::wrap_key(&master, &exam_key)).unwrap();
        (dir, db, master, e)
    }

    fn send(rec: &Recorder, e: i64, f: (FrameHeader, Bytes)) {
        assert!(rec.send(RecMsg::Frame {
            exam_id: e, student_id: "S/1".into(), machine_id: "m".into(), lab_id: "lab".into(), hdr: f.0, data: f.1,
        }));
    }

    #[test]
    fn records_from_first_keyframe_and_reads_back() {
        let (dir, db, master, e) = setup();
        let rec = Recorder::start(db.clone(), master, dir.path().join("rec"), 60);
        send(&rec, e, frame(false, 1, 1)); // dropped: no keyframe yet
        send(&rec, e, frame(true, 2, 2));
        send(&rec, e, frame(false, 3, 3));
        rec.send(RecMsg::Close { exam_id: e, student_id: "S/1".into() });
        rec.flush();

        let segs = db.segments(e, "S/1").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].frames, 2);
        assert_eq!(segs[0].start_us, 2);
        assert_eq!(segs[0].end_us, 3);
        assert!(segs[0].path.contains("S_1"), "student id sanitised in path");

        let key = crypto::unwrap_key(&master, &db.exam_key(e).unwrap().unwrap()).unwrap();
        let out = read_segment(Path::new(&segs[0].path), &key).unwrap();
        let h = FrameHeader::decode(out[..FRAME_HEADER_LEN].try_into().unwrap());
        assert!(h.is_key());
        assert_eq!(h.pts_us, 2);
        assert_eq!(out.len(), 2 * FRAME_HEADER_LEN + 102 + 103);
    }

    #[test]
    fn rotates_only_on_keyframe() {
        let (dir, db, master, e) = setup();
        let rec = Recorder::start(db.clone(), master, dir.path().join("rec"), 0);
        // segment length 0: every keyframe starts a new segment, P-frames never do
        send(&rec, e, frame(true, 10, 1));
        send(&rec, e, frame(false, 11, 1));
        send(&rec, e, frame(true, 12, 1));
        send(&rec, e, frame(false, 13, 1));
        rec.send(RecMsg::CloseExam { exam_id: e });
        rec.flush();
        let segs = db.segments(e, "S/1").unwrap();
        assert_eq!(segs.iter().map(|s| s.frames).collect::<Vec<_>>(), vec![2, 2]);
    }

    #[test]
    fn truncated_tail_is_tolerated_and_tamper_detected() {
        let (dir, db, master, e) = setup();
        let rec = Recorder::start(db.clone(), master, dir.path().join("rec"), 60);
        send(&rec, e, frame(true, 1, 1));
        send(&rec, e, frame(false, 2, 2));
        rec.send(RecMsg::CloseExam { exam_id: e });
        rec.flush();
        let seg = &db.segments(e, "S/1").unwrap()[0];
        let key = crypto::unwrap_key(&master, &db.exam_key(e).unwrap().unwrap()).unwrap();
        let p = Path::new(&seg.path);
        let mut raw = std::fs::read(p).unwrap();
        raw.truncate(raw.len() - 5);
        std::fs::write(p, &raw).unwrap();
        assert_eq!(read_segment(p, &key).unwrap().len(), FRAME_HEADER_LEN + 101);
        raw[20] ^= 0xff;
        std::fs::write(p, &raw).unwrap();
        assert!(read_segment(p, &key).is_err());
    }
}
