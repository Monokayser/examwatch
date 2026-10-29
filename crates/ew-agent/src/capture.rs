//! Capture thread: Desktop Duplication → GPU scale → H.264 → network queues.
//!
//! Runs only while `Shared::capturing` is true. Every stream is rate-limited,
//! encodes only when the screen changed (plus a 10 s keep-alive frame), and a
//! governor slows everything down when the PC is busy (design §2.8).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ew_media::capture::{self, Duplicator, Grab, Gpu, Scaler};
use ew_media::encoder::{self, EncoderParams, H264Encoder, Input};
use ew_proto::client::ClientHandle;
use ew_proto::{AgentMsg, EncoderKind, FrameHeader, StreamKind, FLAG_KEY};

use crate::state::Shared;

/// Probes the hardware once at start-up so the Hello message can report it.
pub struct Probe {
    pub encoder: EncoderKind,
    pub gpu: String,
    pub screen: (u32, u32),
    pub monitors: u32,
}

pub fn probe() -> Probe {
    let _ = encoder::startup();
    match capture::open_primary() {
        Ok(gpu) => {
            let hw = gpu
                .device_manager()
                .and_then(|m| H264Encoder::hardware(params(480, 270, 1, 50, 120), gpu.luid, &m))
                .is_ok();
            Probe {
                encoder: if hw { EncoderKind::Hardware } else { EncoderKind::Software },
                gpu: gpu.adapter_name.clone(),
                screen: gpu.screen,
                monitors: gpu.monitors,
            }
        }
        Err(e) => {
            tracing::warn!("GPU probe failed: {e:#}");
            Probe { encoder: EncoderKind::Software, gpu: "unknown".into(), screen: (0, 0), monitors: 1 }
        }
    }
}

fn params(w: u32, h: u32, fps: u32, avg: u32, max: u32) -> EncoderParams {
    EncoderParams { width: w, height: h, fps, avg_kbps: avg, max_kbps: max, max_qp: 30, gop_secs: 10 }
}

/// Frame intervals per governor level: (thumb, rec, live).
const LEVELS: [(u64, u64, u64); 4] = [(1000, 500, 66), (2000, 500, 125), (5000, 1000, 250), (10_000, 1000, 500)];
/// Even an unchanged screen is re-sent this often, so the timeline shows it was alive.
const KEEPALIVE: Duration = Duration::from_secs(10);

struct Stream {
    kind: StreamKind,
    scaler: Scaler,
    enc: H264Encoder,
    hardware: bool,
    next: Instant,
    last_sent: Instant,
    dirty: bool,
    nv12: Vec<u8>,
}

impl Stream {
    fn new(gpu: &Gpu, dup: &Duplicator, kind: StreamKind, size: (u32, u32), p: EncoderParams, prefer_hw: bool) -> Result<Self> {
        let scaler = Scaler::new(gpu, &dup.copy, dup.size, size)?;
        let p = EncoderParams { width: size.0, height: size.1, ..p };
        let (enc, hardware) = match prefer_hw.then(|| gpu.device_manager().and_then(|m| H264Encoder::hardware(p, gpu.luid, &m))) {
            Some(Ok(e)) => (e, true),
            other => {
                if let Some(Err(e)) = other {
                    tracing::info!(?kind, "hardware encoder unavailable, using software: {e:#}");
                }
                (H264Encoder::software(p)?, false)
            }
        };
        Ok(Self {
            kind,
            scaler,
            enc,
            hardware,
            next: Instant::now(),
            last_sent: Instant::now() - KEEPALIVE,
            dirty: true,
            nv12: Vec::new(),
        })
    }

    fn due(&self, now: Instant) -> bool {
        now >= self.next && (self.dirty || self.last_sent.elapsed() >= KEEPALIVE)
    }

    fn encode(&mut self, gpu: &Gpu, shared: &Shared, net: &ClientHandle, interval: Duration) -> Result<()> {
        let now = Instant::now();
        self.scaler.run()?;
        let ts = shared.now_us();
        let out = if self.hardware {
            self.enc.encode(Input::Texture(&self.scaler.out), ts)?
        } else {
            self.scaler.read_nv12(gpu, &mut self.nv12)?;
            self.enc.encode(Input::Nv12(&self.nv12), ts)?
        };
        let (w, h) = self.scaler.size;
        for f in out {
            let hdr = FrameHeader { flags: if f.key { FLAG_KEY } else { 0 }, width: w as u16, height: h as u16, pts_us: ts, len: f.data.len() as u32 };
            net.push_frame(self.kind, hdr, f.data);
        }
        self.dirty = false;
        self.last_sent = now;
        self.next = now + interval;
        Ok(())
    }
}

/// Busy-ness of the whole PC, from GetSystemTimes deltas.
struct CpuMeter {
    last: (u64, u64),
}

impl CpuMeter {
    fn new() -> Self {
        Self { last: sys_times() }
    }
    /// Percent of total CPU used by everything since the previous call.
    fn sample(&mut self) -> f32 {
        let now = sys_times();
        let busy = now.0.saturating_sub(self.last.0);
        let total = now.1.saturating_sub(self.last.1);
        self.last = now;
        if total == 0 { 0.0 } else { busy as f32 * 100.0 / total as f32 }
    }
}

fn sys_times() -> (u64, u64) {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user));
    }
    let v = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    let total = v(kernel) + v(user); // kernel time includes idle
    (total.saturating_sub(v(idle)), total)
}

pub fn run(shared: Arc<Shared>, net: ClientHandle) {
    let _ = encoder::startup();
    let mut prefer_hw = true;
    while !shared.quit.load(Ordering::Relaxed) {
        if !shared.capturing.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200)); // nothing at all happens before V1
            continue;
        }
        match session(&shared, &net, prefer_hw) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("capture session ended: {e:#}");
                // A hardware encoder that keeps failing (driver, session limit) → software.
                if format!("{e:#}").contains("hardware") {
                    prefer_hw = false;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn session(shared: &Shared, net: &ClientHandle, prefer_hw: bool) -> Result<()> {
    let gpu = capture::open_primary().context("open GPU")?;
    let monitors = gpu.monitors;
    if monitors > 1 {
        net.send(AgentMsg::Event { kind: "alert_monitors".into(), detail: format!("{monitors} monitors connected") });
    }
    let mut dup = Duplicator::new(&gpu)?;
    capture::ensure_size(dup.size)?;
    let thumb_size = ew_media::fit(dup.size, 480, 270);
    let rec_size = ew_media::fit(dup.size, 4096, 720); // §2.9: 720 px tall, keep aspect, never upscale
    let mut thumb = Stream::new(&gpu, &dup, StreamKind::Thumb, thumb_size, params(0, 0, 1, 50, 120), prefer_hw)?;
    let mut rec = Stream::new(&gpu, &dup, StreamKind::Rec, rec_size, params(0, 0, 2, 250, 600), prefer_hw)?;
    let mut live: Option<Stream> = None;
    let software = !thumb.hardware || !rec.hardware;

    let mut cpu = CpuMeter::new();
    let mut level = 0usize;
    let mut calm_since = Instant::now();
    let mut last_gov = Instant::now();
    let mut lost_count = 0u32;

    while shared.capturing.load(Ordering::Relaxed) && !shared.quit.load(Ordering::Relaxed) {
        // Governor, once per second: the student's work always wins.
        if last_gov.elapsed() >= Duration::from_secs(1) {
            last_gov = Instant::now();
            let busy = cpu.sample();
            if busy > 70.0 && level < LEVELS.len() - 1 {
                level += 1;
                calm_since = Instant::now();
            } else if busy > 50.0 {
                calm_since = Instant::now();
            } else if level > 0 && calm_since.elapsed() > Duration::from_secs(10) {
                level -= 1;
                calm_since = Instant::now();
            }
            shared.throttle.store(level as u8, Ordering::Relaxed);
        }
        let (t_ms, r_ms, l_ms) = LEVELS[level];

        if shared.force_key.swap(false, Ordering::SeqCst) {
            thumb.enc.request_keyframe();
            rec.enc.request_keyframe();
            if let Some(l) = live.as_mut() {
                l.enc.request_keyframe();
            }
        }

        // LIVE exists only while a teacher has this screen enlarged.
        if shared.live.load(Ordering::Relaxed) {
            if live.is_none() {
                // GT 730 PCs (software) are capped at 720p for LIVE.
                let size = if software { rec_size } else { ew_media::fit(dup.size, 1920, 1080) };
                let mut s = Stream::new(&gpu, &dup, StreamKind::Live, size, params(0, 0, 15, 1200, 2000), prefer_hw && !software)?;
                s.enc.request_keyframe();
                live = Some(s);
            }
        } else {
            live = None;
        }

        match dup.grab(&gpu, 100)? {
            Grab::Changed => {
                lost_count = 0;
                thumb.dirty = true;
                rec.dirty = true;
                if let Some(l) = live.as_mut() {
                    l.dirty = true;
                }
            }
            Grab::Unchanged => {}
            Grab::Lost => {
                // UAC prompt, lock screen, resolution change: re-open after a moment.
                lost_count += 1;
                if lost_count == 1 {
                    net.send(AgentMsg::Event { kind: "info_secure_desktop".into(), detail: "Secure desktop or display change; capture paused".into() });
                }
                std::thread::sleep(Duration::from_millis(250));
                let g = capture::open_primary()?;
                if g.monitors != monitors {
                    net.send(AgentMsg::Event { kind: "alert_monitors".into(), detail: format!("Monitor count changed: {monitors} → {}", g.monitors) });
                }
                drop(g);
                return Ok(()); // rebuild everything on the next session() call
            }
        }

        let now = Instant::now();
        if rec.due(now) {
            rec.encode(&gpu, shared, net, Duration::from_millis(r_ms))?;
        }
        if thumb.due(now) {
            thumb.encode(&gpu, shared, net, Duration::from_millis(t_ms))?;
        }
        if let Some(l) = live.as_mut() {
            if l.due(now) {
                l.encode(&gpu, shared, net, Duration::from_millis(l_ms))?;
            }
        }
    }
    Ok(())
}
