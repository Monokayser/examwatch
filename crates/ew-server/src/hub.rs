//! In-memory hub: who is connected, which exam each lab is in, and fan-out of
//! media to dashboards. Locks here are synchronous and never held across an
//! `.await`; every send to a dashboard or agent is `try_send`, so one slow
//! browser can never slow down a student's upload.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use axum::extract::ws::Message;
use bytes::Bytes;
use ew_proto::{EncoderKind, ExamInfo, FrameHeader, ServerMsg, StreamKind};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::config::{Config, UNASSIGNED_LAB};
use crate::crypto::{self, KeyBytes};
use crate::db::{ClaimOutcome, Db, Exam};
use crate::recorder::{RecMsg, Recorder};

/// Thumbnail GOP cache depth (1 fps, keyframe every 10 s → ≤ 10 frames normally).
const GOP_CACHE: usize = 30;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PcStatus {
    /// Not connected.
    Offline,
    /// Connected, no exam open for its lab.
    Idle,
    /// Exam open, student has not pressed V1 yet.
    Waiting,
    /// Student ID confirmed, screen being recorded.
    Live,
}

#[derive(Debug, Clone, Serialize)]
pub struct PcInfo {
    pub machine_id: String,
    pub pc_name: String,
    pub lab_id: String,
    pub ip: String,
    pub status: PcStatus,
    pub student_id: Option<String>,
    pub exam_id: Option<i64>,
    pub encoder: Option<EncoderKind>,
    pub gpu: String,
    pub screen: (u32, u32),
    pub monitors: u32,
    pub cpu_pct: f32,
    pub rss_mb: f32,
    pub throttle: u8,
    pub since_us: u64,
    pub viewers: u32,
}

struct PcEntry {
    info: PcInfo,
    conn_gen: u64,
    ctl: Option<mpsc::Sender<ServerMsg>>,
    thumb_gop: Vec<(FrameHeader, Bytes)>,
}

pub struct DashClient {
    pub id: u64,
    pub user: String,
    pub tx: mpsc::Sender<Message>,
    /// `None` = no thumbnails, `Some("*")` = all labs, `Some(lab)` = one lab.
    pub thumb_lab: RwLock<Option<String>>,
    pub live: Mutex<HashSet<String>>,
    need_key: Mutex<HashSet<(String, u8)>>,
}

pub struct Hub {
    pub cfg: Config,
    pub db: Arc<Db>,
    pub recorder: Recorder,
    master: KeyBytes,
    pcs: RwLock<HashMap<String, PcEntry>>,
    dashes: RwLock<HashMap<u64, Arc<DashClient>>>,
    open_exams: RwLock<Vec<Exam>>,
    next_id: AtomicU64,
}

#[derive(Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum DashEvent<'a> {
    Snapshot { me: &'a str, labs: serde_json::Value, pcs: Vec<PcInfo>, exams: Vec<Exam> },
    Pc { pc: PcInfo },
    Exams { exams: Vec<Exam> },
    Toast { level: &'a str, text: String, lab_id: String },
}

impl Hub {
    pub fn new(cfg: Config, db: Arc<Db>, recorder: Recorder, master: KeyBytes) -> Result<Arc<Self>> {
        let open = db.open_exams()?;
        let hub = Arc::new(Self {
            cfg,
            db,
            recorder,
            master,
            pcs: RwLock::new(HashMap::new()),
            dashes: RwLock::new(HashMap::new()),
            open_exams: RwLock::new(open),
            next_id: AtomicU64::new(1),
        });
        // Known PCs appear as Offline tiles from the start, so a PC that never
        // boots is still visible to the teacher.
        for (machine_id, pc_name, lab_id) in hub.db.known_machines()? {
            hub.pcs.write().insert(machine_id.clone(), PcEntry {
                info: PcInfo {
                    machine_id, pc_name, lab_id, ip: String::new(), status: PcStatus::Offline,
                    student_id: None, exam_id: None, encoder: None, gpu: String::new(), screen: (0, 0),
                    monitors: 0, cpu_pct: 0.0, rss_mb: 0.0, throttle: 0, since_us: ew_proto::now_us(), viewers: 0,
                },
                conn_gen: 0,
                ctl: None,
                thumb_gop: Vec::new(),
            });
        }
        Ok(hub)
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn lab_name(&self, lab_id: &str) -> String {
        self.cfg.labs.iter().find(|l| l.id == lab_id).map(|l| l.name.clone()).unwrap_or_else(|| "Unassigned".into())
    }

    pub fn exam_for_lab(&self, lab_id: &str) -> Option<Exam> {
        self.open_exams.read().iter().find(|e| e.labs.iter().any(|l| l == lab_id)).cloned()
    }

    fn exam_info(e: &Exam) -> ExamInfo {
        ExamInfo { exam_id: e.id, name: e.name.clone(), roster_enforced: e.roster_enforced }
    }

    pub fn labs_json(&self) -> serde_json::Value {
        let mut v: Vec<serde_json::Value> = self
            .cfg
            .labs
            .iter()
            .map(|l| serde_json::json!({"id": l.id, "name": l.name, "room": l.room, "expected_pcs": l.expected_pcs}))
            .collect();
        v.push(serde_json::json!({"id": UNASSIGNED_LAB, "name": "Unassigned", "room": "", "expected_pcs": 0}));
        serde_json::Value::Array(v)
    }

    // ---------------------------------------------------------------- agents

    /// Registers a freshly authenticated agent connection. Returns its generation
    /// (used so a stale connection's cleanup never clobbers a newer one) and the welcome.
    pub fn agent_connected(
        &self,
        machine_id: &str,
        pc_name: &str,
        lab_id: &str,
        ip: String,
        encoder: EncoderKind,
        gpu: String,
        screen: (u32, u32),
        monitors: u32,
        ctl: mpsc::Sender<ServerMsg>,
    ) -> (u64, ServerMsg) {
        let gen = self.next_id();
        let exam = self.exam_for_lab(lab_id);
        let student = exam.as_ref().and_then(|e| self.db.active_claim_for_machine(e.id, machine_id).ok().flatten());
        let status = match (&exam, &student) {
            (None, _) => PcStatus::Idle,
            (Some(_), None) => PcStatus::Waiting,
            (Some(_), Some(_)) => PcStatus::Waiting, // becomes Live when the agent re-claims
        };
        let info = PcInfo {
            machine_id: machine_id.to_string(),
            pc_name: pc_name.to_string(),
            lab_id: lab_id.to_string(),
            ip,
            status,
            student_id: None,
            exam_id: exam.as_ref().map(|e| e.id),
            encoder: Some(encoder),
            gpu,
            screen,
            monitors,
            cpu_pct: 0.0,
            rss_mb: 0.0,
            throttle: 0,
            since_us: ew_proto::now_us(),
            viewers: 0,
        };
        let was_offline_in_exam;
        {
            let mut pcs = self.pcs.write();
            let prev = pcs.get(machine_id);
            was_offline_in_exam = prev.map_or(false, |p| p.info.status == PcStatus::Offline && p.info.student_id.is_some());
            let viewers = prev.map_or(0, |p| p.info.viewers);
            let mut info = info;
            info.viewers = viewers;
            pcs.insert(machine_id.to_string(), PcEntry { info, conn_gen: gen, ctl: Some(ctl.clone()), thumb_gop: Vec::new() });
        }
        if was_offline_in_exam {
            self.toast("info", lab_id, format!("{pc_name} is back online"));
        }
        // A teacher may still have this screen enlarged from before the drop.
        if self.pcs.read().get(machine_id).map_or(0, |p| p.info.viewers) > 0 {
            let _ = ctl.try_send(ServerMsg::LiveOn { max_fps: 15, max_kbps: 2000 });
        }
        self.publish_pc(machine_id);
        let welcome = ServerMsg::Welcome {
            lab_id: lab_id.to_string(),
            lab_name: self.lab_name(lab_id),
            exam: exam.as_ref().map(Self::exam_info),
            server_us: ew_proto::now_us(),
        };
        (gen, welcome)
    }

    pub fn agent_disconnected(&self, machine_id: &str, gen: u64) {
        let (lab, name, student, exam) = {
            let mut pcs = self.pcs.write();
            let Some(p) = pcs.get_mut(machine_id) else { return };
            if p.conn_gen != gen {
                return; // a newer connection already replaced this one
            }
            p.ctl = None;
            p.thumb_gop.clear();
            p.info.status = PcStatus::Offline;
            p.info.since_us = ew_proto::now_us();
            (p.info.lab_id.clone(), p.info.pc_name.clone(), p.info.student_id.clone(), p.info.exam_id)
        };
        if let (Some(sid), Some(e)) = (&student, exam) {
            self.recorder.send(RecMsg::Close { exam_id: e, student_id: sid.clone() });
            self.toast("alert", &lab, format!("{name} ({sid}) went OFFLINE"));
        }
        self.publish_pc(machine_id);
    }

    pub fn heartbeat(&self, machine_id: &str, cpu: f32, rss: f32, throttle: u8) {
        let publish = {
            let mut pcs = self.pcs.write();
            let Some(p) = pcs.get_mut(machine_id) else { return };
            let changed = p.info.throttle != throttle;
            p.info.cpu_pct = cpu;
            p.info.rss_mb = rss;
            p.info.throttle = throttle;
            changed
        };
        if publish {
            self.publish_pc(machine_id);
        }
    }

    pub fn agent_event(&self, machine_id: &str, kind: &str, detail: &str) {
        let (lab, name) = match self.pcs.read().get(machine_id) {
            Some(p) => (p.info.lab_id.clone(), p.info.pc_name.clone()),
            None => return,
        };
        self.db.audit(&name, &format!("agent:{kind}"), detail);
        let level = if kind.starts_with("alert") { "alert" } else { "info" };
        self.toast(level, &lab, format!("{name}: {detail}"));
    }

    /// Student pressed V1 and confirmed an ID.
    pub fn claim(&self, machine_id: &str, raw_id: &str) -> ServerMsg {
        let sid = ew_proto::normalize_student_id(raw_id);
        let (lab, pc_name) = match self.pcs.read().get(machine_id) {
            Some(p) => (p.info.lab_id.clone(), p.info.pc_name.clone()),
            None => return ServerMsg::ClaimResult { ok: false, student_id: sid, reason: Some("Not connected".into()) },
        };
        let Some(exam) = self.exam_for_lab(&lab) else {
            return ServerMsg::ClaimResult {
                ok: false,
                student_id: sid,
                reason: Some("No exam is open. Please ask your invigilator.".into()),
            };
        };
        if !exam.roster_enforced && !ew_proto::student_id_format_ok(&sid) {
            return ServerMsg::ClaimResult { ok: false, student_id: sid, reason: Some("That ID doesn't look right. Check it and try again.".into()) };
        }
        let outcome = match self.db.claim(exam.id, exam.roster_enforced, &sid, machine_id, &pc_name, &lab) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("claim failed: {e:#}");
                return ServerMsg::ClaimResult { ok: false, student_id: sid, reason: Some("Server error. Please try again.".into()) };
            }
        };
        let reason = match outcome {
            ClaimOutcome::Ok => None,
            ClaimOutcome::NotOnRoster => Some("This ID is not on the list for this exam. Check for typos.".to_string()),
            ClaimOutcome::TakenBy { pc_name } => {
                self.toast("alert", &lab, format!("Duplicate ID {sid}: already in use on {pc_name}"));
                Some(format!("This ID is already in use on {pc_name}. Please ask your invigilator."))
            }
            ClaimOutcome::MachineHasOther { student_id } => {
                Some(format!("This PC is already registered to {student_id}. Ask your invigilator to reset it."))
            }
        };
        if reason.is_some() {
            self.db.audit(&pc_name, "claim_rejected", &format!("{sid}: {}", reason.as_deref().unwrap_or("")));
            return ServerMsg::ClaimResult { ok: false, student_id: sid, reason };
        }
        let newly = {
            let mut pcs = self.pcs.write();
            let p = pcs.get_mut(machine_id).unwrap();
            let newly = p.info.student_id.as_deref() != Some(sid.as_str());
            p.info.student_id = Some(sid.clone());
            p.info.exam_id = Some(exam.id);
            p.info.status = PcStatus::Live;
            p.info.since_us = ew_proto::now_us();
            newly
        };
        if newly {
            self.db.audit(&pc_name, "claim", &format!("exam {} student {sid}", exam.id));
            self.toast("join", &lab, format!("{sid} · {} · {pc_name} started", self.lab_name(&lab)));
        }
        self.publish_pc(machine_id);
        ServerMsg::ClaimResult { ok: true, student_id: sid, reason: None }
    }

    /// Media from an agent. Called for every frame, so it only does cheap work.
    pub fn media(&self, machine_id: &str, kind: StreamKind, hdr: FrameHeader, data: Bytes) {
        let (lab, student, exam) = {
            let mut pcs = self.pcs.write();
            let Some(p) = pcs.get_mut(machine_id) else { return };
            if kind == StreamKind::Thumb {
                if hdr.is_key() {
                    p.thumb_gop.clear();
                }
                if p.thumb_gop.len() < GOP_CACHE {
                    p.thumb_gop.push((hdr, data.clone()));
                }
            }
            (p.info.lab_id.clone(), p.info.student_id.clone(), p.info.exam_id)
        };
        match kind {
            StreamKind::Rec => {
                if let (Some(sid), Some(e)) = (student, exam) {
                    self.recorder.send(RecMsg::Frame {
                        exam_id: e, student_id: sid, machine_id: machine_id.to_string(), lab_id: lab, hdr, data,
                    });
                }
            }
            StreamKind::Thumb => {
                let msg = encode_media(kind, machine_id, &hdr, &data);
                for d in self.dashes.read().values() {
                    let wants = d.thumb_lab.read().as_deref().map_or(false, |l| l == "*" || l == lab);
                    if wants {
                        d.send_media(machine_id, kind, &hdr, &msg);
                    }
                }
            }
            StreamKind::Live => {
                let msg = encode_media(kind, machine_id, &hdr, &data);
                for d in self.dashes.read().values() {
                    if d.live.lock().contains(machine_id) {
                        d.send_media(machine_id, kind, &hdr, &msg);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ dashboards

    pub fn add_dash(&self, user: &str, tx: mpsc::Sender<Message>) -> Arc<DashClient> {
        let d = Arc::new(DashClient {
            id: self.next_id(),
            user: user.to_string(),
            tx,
            thumb_lab: RwLock::new(None),
            live: Mutex::new(HashSet::new()),
            need_key: Mutex::new(HashSet::new()),
        });
        self.dashes.write().insert(d.id, d.clone());
        let snap = DashEvent::Snapshot { me: user, labs: self.labs_json(), pcs: self.pc_list(), exams: self.open_exams.read().clone() };
        let _ = d.tx.try_send(Message::Text(serde_json::to_string(&snap).unwrap()));
        d
    }

    pub fn remove_dash(&self, d: &DashClient) {
        self.dashes.write().remove(&d.id);
        let lives: Vec<String> = d.live.lock().drain().collect();
        for m in lives {
            self.release_live(&m);
        }
    }

    pub fn subscribe_thumbs(&self, d: &DashClient, lab: Option<String>) {
        *d.thumb_lab.write() = lab.clone();
        d.need_key.lock().retain(|(_, k)| *k != StreamKind::Thumb as u8);
        let Some(lab) = lab else { return };
        // Replay the cached GOP so tiles fill instantly instead of waiting ≤10 s for a keyframe.
        let pcs = self.pcs.read();
        for (id, p) in pcs.iter() {
            if lab == "*" || p.info.lab_id == lab {
                for (h, data) in &p.thumb_gop {
                    d.send_media(id, StreamKind::Thumb, h, &encode_media(StreamKind::Thumb, id, h, data));
                }
            }
        }
    }

    pub fn set_live(&self, d: &DashClient, machine_id: &str, on: bool) {
        let changed = if on { d.live.lock().insert(machine_id.to_string()) } else { d.live.lock().remove(machine_id) };
        if !changed {
            return;
        }
        if on {
            d.need_key.lock().insert((machine_id.to_string(), StreamKind::Live as u8));
            if let Some(p) = self.pcs.write().get_mut(machine_id) {
                p.info.viewers += 1;
            }
            // The agent restarts LIVE with a keyframe on every LiveOn, which also
            // serves a second viewer joining mid-stream.
            self.send_to(machine_id, ServerMsg::LiveOn { max_fps: 15, max_kbps: 2000 });
            self.publish_pc(machine_id);
        } else {
            self.release_live(machine_id);
        }
    }

    fn release_live(&self, machine_id: &str) {
        let last = {
            let mut pcs = self.pcs.write();
            match pcs.get_mut(machine_id) {
                Some(p) if p.info.viewers > 0 => {
                    p.info.viewers -= 1;
                    p.info.viewers == 0
                }
                _ => false,
            }
        };
        if last {
            self.send_to(machine_id, ServerMsg::LiveOff);
        }
        self.publish_pc(machine_id);
    }

    pub fn send_to(&self, machine_id: &str, msg: ServerMsg) {
        if let Some(tx) = self.pcs.read().get(machine_id).and_then(|p| p.ctl.clone()) {
            let _ = tx.try_send(msg);
        }
    }

    pub fn pc_list(&self) -> Vec<PcInfo> {
        let mut v: Vec<PcInfo> = self.pcs.read().values().map(|p| p.info.clone()).collect();
        v.sort_by(|a, b| a.lab_id.cmp(&b.lab_id).then_with(|| natural_cmp(&a.pc_name, &b.pc_name)));
        v
    }

    fn publish_pc(&self, machine_id: &str) {
        let Some(info) = self.pcs.read().get(machine_id).map(|p| p.info.clone()) else { return };
        self.broadcast(&DashEvent::Pc { pc: info });
    }

    pub fn toast(&self, level: &str, lab_id: &str, text: String) {
        self.broadcast(&DashEvent::Toast { level, text, lab_id: lab_id.to_string() });
    }

    fn broadcast(&self, ev: &DashEvent) {
        let text = serde_json::to_string(ev).unwrap();
        for d in self.dashes.read().values() {
            // Control events are tiny; if even these back up the browser is gone.
            let _ = d.tx.try_send(Message::Text(text.clone()));
        }
    }

    // ----------------------------------------------------------------- exams

    pub fn open_exam(&self, actor: &str, name: &str, labs: Vec<String>, roster: Vec<String>) -> Result<Exam> {
        let name = name.trim();
        if name.is_empty() || name.len() > 120 {
            bail!("Exam name must be 1–120 characters");
        }
        if labs.is_empty() {
            bail!("Choose at least one lab");
        }
        for l in &labs {
            if !self.cfg.labs.iter().any(|c| &c.id == l) {
                bail!("Unknown lab {l}");
            }
        }
        let roster: Vec<String> = roster
            .iter()
            .map(|s| ew_proto::normalize_student_id(s))
            .filter(|s| !s.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let key = crypto::random_key();
        let id = self.db.open_exam(name, &labs, actor, &roster, &crypto::wrap_key(&self.master, &key))?;
        let exam = self.db.exam(id)?.expect("just inserted");
        *self.open_exams.write() = self.db.open_exams()?;
        self.db.audit(actor, "exam_open", &format!("{id} {name} labs={labs:?} roster={}", roster.len()));

        let info = Self::exam_info(&exam);
        let ids: Vec<String> = {
            let mut pcs = self.pcs.write();
            pcs.values_mut()
                .filter(|p| labs.contains(&p.info.lab_id))
                .map(|p| {
                    p.info.exam_id = Some(id);
                    p.info.student_id = None;
                    if p.info.status != PcStatus::Offline {
                        p.info.status = PcStatus::Waiting;
                    }
                    if let Some(tx) = &p.ctl {
                        let _ = tx.try_send(ServerMsg::ExamOpened { exam: info.clone() });
                    }
                    p.info.machine_id.clone()
                })
                .collect()
        };
        for m in ids {
            self.publish_pc(&m);
        }
        self.broadcast(&DashEvent::Exams { exams: self.open_exams.read().clone() });
        Ok(exam)
    }

    pub fn close_exam(&self, actor: &str, exam_id: i64) -> Result<()> {
        if !self.db.close_exam(exam_id)? {
            bail!("Exam is not open");
        }
        self.recorder.send(RecMsg::CloseExam { exam_id });
        *self.open_exams.write() = self.db.open_exams()?;
        self.db.audit(actor, "exam_close", &exam_id.to_string());
        let ids: Vec<String> = {
            let mut pcs = self.pcs.write();
            pcs.values_mut()
                .filter(|p| p.info.exam_id == Some(exam_id))
                .map(|p| {
                    p.info.exam_id = None;
                    p.info.student_id = None;
                    if p.info.status != PcStatus::Offline {
                        p.info.status = PcStatus::Idle;
                    }
                    p.thumb_gop.clear();
                    if let Some(tx) = &p.ctl {
                        let _ = tx.try_send(ServerMsg::ExamClosed { exam_id });
                    }
                    p.info.machine_id.clone()
                })
                .collect()
        };
        for m in ids {
            self.publish_pc(&m);
        }
        self.broadcast(&DashEvent::Exams { exams: self.open_exams.read().clone() });
        Ok(())
    }

    /// Teacher frees a PC's Student ID (wrong ID typed, or the student moved PCs).
    pub fn release_pc(&self, actor: &str, machine_id: &str) -> Result<()> {
        let exam = match self.pcs.read().get(machine_id) {
            Some(p) => p.info.exam_id,
            None => bail!("Unknown PC"),
        };
        let Some(exam) = exam else { bail!("No exam open on this PC") };
        let sid = self.db.release_claim(exam, machine_id)?;
        if let Some(sid) = &sid {
            self.recorder.send(RecMsg::Close { exam_id: exam, student_id: sid.clone() });
        }
        {
            let mut pcs = self.pcs.write();
            if let Some(p) = pcs.get_mut(machine_id) {
                p.info.student_id = None;
                if p.info.status == PcStatus::Live {
                    p.info.status = PcStatus::Waiting;
                }
            }
        }
        self.send_to(machine_id, ServerMsg::ClaimResult {
            ok: false,
            student_id: sid.clone().unwrap_or_default(),
            reason: Some("Your invigilator reset this PC. Press V1 to start again.".into()),
        });
        self.db.audit(actor, "release", &format!("{machine_id} {}", sid.unwrap_or_default()));
        self.publish_pc(machine_id);
        Ok(())
    }

    pub fn exams_open(&self) -> Vec<Exam> {
        self.open_exams.read().clone()
    }

    pub fn master_key(&self) -> &KeyBytes {
        &self.master
    }
}

impl DashClient {
    fn send_media(&self, machine_id: &str, kind: StreamKind, hdr: &FrameHeader, msg: &[u8]) {
        let k = (machine_id.to_string(), kind as u8);
        {
            let mut need = self.need_key.lock();
            if need.contains(&k) {
                if !hdr.is_key() {
                    return; // decoder would fail on a P-frame after a gap
                }
                need.remove(&k);
            }
        }
        if self.tx.try_send(Message::Binary(msg.to_vec())).is_err() {
            // Browser can't keep up: skip until the next keyframe rather than queueing.
            self.need_key.lock().insert(k);
        }
    }
}

/// Binary WebSocket frame: `kind u8 | id_len u8 | machine_id | FrameHeader | AU`.
pub fn encode_media(kind: StreamKind, machine_id: &str, hdr: &FrameHeader, data: &[u8]) -> Vec<u8> {
    let id = machine_id.as_bytes();
    let id = &id[..id.len().min(255)];
    let mut v = Vec::with_capacity(2 + id.len() + ew_proto::FRAME_HEADER_LEN + data.len());
    v.push(kind as u8);
    v.push(id.len() as u8);
    v.extend_from_slice(id);
    v.extend_from_slice(&hdr.encode());
    v.extend_from_slice(data);
    v
}

/// "PC-2" < "PC-10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| {
        let mut parts: Vec<(String, u64)> = Vec::new();
        let mut text = String::new();
        let mut num = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else {
                if !num.is_empty() {
                    parts.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                    num.clear();
                }
                text.push(c.to_ascii_lowercase());
            }
        }
        parts.push((text, num.parse().unwrap_or(0)));
        parts
    };
    key(a).cmp(&key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort() {
        let mut v = vec!["PC-10", "PC-2", "PC-1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["PC-1", "PC-2", "PC-10"]);
    }
}
