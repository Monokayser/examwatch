//! Load / failure simulator: many fake student PCs against a real ew-server.
//!
//! Each simulated PC behaves like `ew-agent`: connects, waits for an exam,
//! "presses V1" (claims a Student ID), then streams real H.264 (THUMB 1 fps,
//! REC 2 fps, LIVE 5 fps while enlarged) encoded by the Windows software MFT
//! from synthetic desktops that show the PC number and a running clock.
//!
//! ```text
//! ew-sim --server 127.0.0.1:4433 --pin <sha256> --token <enroll token>
//!        [--labs A1-:28,VA-:27,GM-:26,A2-:27,PD-:6] [--no-v1] [--rec 1280x720]
//!        [--chaos <secs>]   randomly drop one PC's connection every <secs>
//!        [--dup-every <n>]  every n-th PC types an ID that is already taken
//!        [--seconds <n>]    stop after n seconds and print a summary
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ew_media::encoder::{self, EncoderParams, H264Encoder, Input};
use ew_media::synth::Synth;
use ew_proto::client::{self, ClientConfig, ClientEvent, ClientHandle};
use ew_proto::{AgentMsg, EncoderKind, FrameHeader, ServerMsg, StreamKind, FLAG_KEY};
use rand::Rng;

struct Opts {
    server: String,
    pin: String,
    token: String,
    labs: Vec<(String, u32)>,
    auto_v1: bool,
    rec: (u32, u32),
    chaos: Option<u64>,
    dup_every: Option<u32>,
    seconds: Option<u64>,
}

fn parse() -> Result<Opts> {
    let mut a = std::env::args().skip(1);
    let mut o = Opts {
        server: "127.0.0.1:4433".into(),
        pin: String::new(),
        token: String::new(),
        labs: vec![("A1-".into(), 28), ("VA-".into(), 27), ("GM-".into(), 26), ("A2-".into(), 27), ("PD-".into(), 6)],
        auto_v1: true,
        rec: (1280, 720),
        chaos: None,
        dup_every: None,
        seconds: None,
    };
    while let Some(k) = a.next() {
        let mut v = || a.next().with_context(|| format!("{k} needs a value"));
        match k.as_str() {
            "--server" => o.server = v()?,
            "--pin" => o.pin = v()?,
            "--token" => o.token = v()?,
            "--labs" => {
                o.labs = v()?
                    .split(',')
                    .map(|p| {
                        let (pre, n) = p.split_once(':').context("lab spec is PREFIX:COUNT")?;
                        Ok((pre.to_string(), n.parse()?))
                    })
                    .collect::<Result<_>>()?
            }
            "--no-v1" => o.auto_v1 = false,
            "--rec" => {
                let s = v()?;
                let (w, h) = s.split_once('x').context("--rec WxH")?;
                o.rec = (w.parse()?, h.parse()?);
            }
            "--chaos" => o.chaos = Some(v()?.parse()?),
            "--dup-every" => o.dup_every = Some(v()?.parse()?),
            "--seconds" => o.seconds = Some(v()?.parse()?),
            other => bail!("unknown option {other}"),
        }
    }
    if o.pin.is_empty() || o.token.is_empty() {
        bail!("--pin and --token are required (see data/server.fingerprint.txt and ew-server.toml)");
    }
    Ok(o)
}

#[derive(Default)]
struct Stats {
    connected: AtomicU64,
    claimed: AtomicU64,
    rejected: AtomicU64,
    frames: AtomicU64,
    bytes: AtomicU64,
    encode_us: AtomicU64,
    reconnects: AtomicU64,
}

struct PcFlags {
    claimed: AtomicBool,
    live: AtomicBool,
    force_key: AtomicBool,
    stop: AtomicBool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into())).init();
    let o = parse()?;
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    rt.block_on(run(o))
}

async fn run(o: Opts) -> Result<()> {
    let stats = Arc::new(Stats::default());
    let mut pcs = Vec::new();
    let mut idx = 0u32;
    for (prefix, n) in &o.labs {
        for i in 1..=*n {
            idx += 1;
            let name = format!("{prefix}{i:02}");
            let student = match o.dup_every {
                Some(k) if idx % k == 0 && idx > 1 => format!("221-15-{:04}", 4000 + idx - 1), // already taken
                _ => format!("221-15-{:04}", 4000 + idx),
            };
            pcs.push(spawn_pc(&o, idx, name, student, stats.clone()));
        }
    }
    println!("simulating {} PCs against {}", pcs.len(), o.server);

    if let Some(secs) = o.chaos {
        let pcs2: Vec<_> = pcs.iter().map(|(h, _)| h.clone()).collect();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                let i = rand::thread_rng().gen_range(0..pcs2.len());
                // A bogus control message makes the server drop the link: simulates a cable pull.
                pcs2[i].send(AgentMsg::Hello {
                    machine_id: String::new(), pc_name: String::new(), agent_version: String::new(),
                    enroll_token: String::new(), encoder: EncoderKind::Simulated, gpu: String::new(),
                    screen_w: 0, screen_h: 0, monitors: 0, sent_us: 0,
                });
                println!("chaos: dropped PC #{i}");
            }
        });
    }

    let start = Instant::now();
    let mut last = (0u64, 0u64);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let f = stats.frames.load(Ordering::Relaxed);
        let b = stats.bytes.load(Ordering::Relaxed);
        let enc = stats.encode_us.load(Ordering::Relaxed);
        println!(
            "[{:>5}s] connected {:>3}  started {:>3}  rejected {:>2}  reconnects {:>3}  frames/s {:>5.0}  Mbit/s {:>6.2}  avg encode {:>5.1} ms",
            start.elapsed().as_secs(),
            stats.connected.load(Ordering::Relaxed),
            stats.claimed.load(Ordering::Relaxed),
            stats.rejected.load(Ordering::Relaxed),
            stats.reconnects.load(Ordering::Relaxed),
            (f - last.0) as f64 / 5.0,
            (b - last.1) as f64 * 8.0 / 5.0 / 1e6,
            if f > 0 { enc as f64 / f as f64 / 1000.0 } else { 0.0 },
        );
        last = (f, b);
        if o.seconds.map_or(false, |s| start.elapsed().as_secs() >= s) {
            for (_, flags) in &pcs {
                flags.stop.store(true, Ordering::Relaxed);
            }
            println!("done");
            return Ok(());
        }
    }
}

fn spawn_pc(o: &Opts, idx: u32, name: String, student: String, stats: Arc<Stats>) -> (ClientHandle, Arc<PcFlags>) {
    let (h, mut ev) = client::spawn(ClientConfig {
        server: o.server.clone(),
        pin_sha256: o.pin.clone(),
        enroll_token: o.token.clone(),
        machine_id: format!("sim-{name}"),
        pc_name: name.clone(),
        agent_version: concat!("sim-", env!("CARGO_PKG_VERSION")).into(),
        encoder: EncoderKind::Simulated,
        gpu: "synthetic".into(),
        screen: (1920, 1080),
        monitors: 1,
        rec_backlog_bytes: 8 * 1024 * 1024,
        backlog_kbps: 2000,
        start_jitter: Duration::from_secs(3),
    });
    let flags = Arc::new(PcFlags {
        claimed: AtomicBool::new(false),
        live: AtomicBool::new(false),
        force_key: AtomicBool::new(true),
        stop: AtomicBool::new(false),
    });

    // Control: react like a student at the PC.
    {
        let h = h.clone();
        let flags = flags.clone();
        let stats = stats.clone();
        let auto = o.auto_v1;
        tokio::spawn(async move {
            let mut was_connected = false;
            let mut hb = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    e = ev.recv() => {
                        let Some(e) = e else { return };
                        match e {
                            ClientEvent::Connected => {
                                stats.connected.fetch_add(1, Ordering::Relaxed);
                                if was_connected { stats.reconnects.fetch_add(1, Ordering::Relaxed); }
                                was_connected = true;
                            }
                            ClientEvent::Disconnected => {
                                if was_connected { stats.connected.fetch_sub(1, Ordering::Relaxed); }
                                flags.force_key.store(true, Ordering::Relaxed);
                                flags.live.store(false, Ordering::Relaxed);
                                // keep `claimed`: the PC keeps recording into its backlog
                            }
                            ClientEvent::Server(m) => match m {
                                ServerMsg::Welcome { exam: Some(_), .. } | ServerMsg::ExamOpened { .. } => {
                                    if flags.claimed.load(Ordering::Relaxed) {
                                        h.send(AgentMsg::Claim { student_id: student.clone() }); // re-claim after reconnect
                                    } else if auto {
                                        let h = h.clone();
                                        let s = student.clone();
                                        tokio::spawn(async move {
                                            let ms = rand::thread_rng().gen_range(1500..15000);
                                            tokio::time::sleep(Duration::from_millis(ms)).await;
                                            h.send(AgentMsg::Claim { student_id: s });
                                        });
                                    }
                                }
                                ServerMsg::Welcome { exam: None, .. } | ServerMsg::ExamClosed { .. } => {
                                    if flags.claimed.swap(false, Ordering::Relaxed) { stats.claimed.fetch_sub(1, Ordering::Relaxed); }
                                }
                                ServerMsg::ClaimResult { ok, reason, .. } => {
                                    if ok {
                                        if !flags.claimed.swap(true, Ordering::Relaxed) {
                                            stats.claimed.fetch_add(1, Ordering::Relaxed);
                                            flags.force_key.store(true, Ordering::Relaxed);
                                        }
                                    } else {
                                        if flags.claimed.swap(false, Ordering::Relaxed) { stats.claimed.fetch_sub(1, Ordering::Relaxed); }
                                        stats.rejected.fetch_add(1, Ordering::Relaxed);
                                        tracing::info!("claim rejected: {reason:?}");
                                    }
                                }
                                ServerMsg::LiveOn { .. } => { flags.live.store(true, Ordering::Relaxed); flags.force_key.store(true, Ordering::Relaxed); }
                                ServerMsg::LiveOff => flags.live.store(false, Ordering::Relaxed),
                                _ => {}
                            },
                        }
                    }
                    _ = hb.tick() => {
                        if was_connected {
                            h.send(AgentMsg::Heartbeat { sent_us: ew_proto::now_us(), cpu_pct: 0.4, rss_mb: 38.0, throttle: 0 });
                        }
                    }
                }
            }
        });
    }

    // Encoder thread: same cadence as the real agent.
    {
        let h = h.clone();
        let flags = flags.clone();
        let rec = o.rec;
        std::thread::Builder::new()
            .name(format!("enc-{name}"))
            .spawn(move || {
                if let Err(e) = encode_loop(idx, rec, h, flags, stats) {
                    eprintln!("PC {idx}: encoder stopped: {e:#}");
                }
            })
            .expect("spawn encoder thread");
    }
    (h, flags)
}

fn encode_loop(idx: u32, rec: (u32, u32), h: ClientHandle, flags: Arc<PcFlags>, stats: Arc<Stats>) -> Result<()> {
    encoder::startup()?;
    let mk = |w, h, fps, avg, max| EncoderParams { width: w, height: h, fps, avg_kbps: avg, max_kbps: max, max_qp: 30, gop_secs: 10 };
    let mut thumb_enc = H264Encoder::software(mk(480, 270, 1, 50, 120))?;
    let mut rec_enc = H264Encoder::software(mk(rec.0, rec.1, 2, 250, 600))?;
    let mut live_enc: Option<H264Encoder> = None;
    let mut thumb_s = Synth::new(480, 270, idx);
    let mut rec_s = Synth::new(rec.0, rec.1, idx);
    let mut live_s = Synth::new(1280, 720, idx);
    let (mut next_thumb, mut next_rec, mut next_live) = (Instant::now(), Instant::now(), Instant::now());

    while !flags.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if !flags.claimed.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200)); // no capture before V1
            continue;
        }
        if flags.force_key.swap(false, Ordering::Relaxed) {
            thumb_enc.request_keyframe();
            rec_enc.request_keyframe();
            if let Some(l) = live_enc.as_mut() {
                l.request_keyframe();
            }
        }
        let ts = ew_proto::now_us();
        let emit = |kind: StreamKind, enc: &mut H264Encoder, nv12: &[u8], w: u32, hh: u32| -> Result<()> {
            let t0 = Instant::now();
            for f in enc.encode(Input::Nv12(nv12), ts)? {
                stats.frames.fetch_add(1, Ordering::Relaxed);
                stats.bytes.fetch_add(f.data.len() as u64, Ordering::Relaxed);
                stats.encode_us.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
                let hdr = FrameHeader { flags: if f.key { FLAG_KEY } else { 0 }, width: w as u16, height: hh as u16, pts_us: ts, len: f.data.len() as u32 };
                h.push_frame(kind, hdr, f.data);
            }
            Ok(())
        };
        if now >= next_rec {
            let fr = rec_s.frame(ts / 1000, idx).to_vec();
            emit(StreamKind::Rec, &mut rec_enc, &fr, rec.0, rec.1)?;
            next_rec = now + Duration::from_millis(500);
        }
        if now >= next_thumb {
            let fr = thumb_s.frame(ts / 1000, idx).to_vec();
            emit(StreamKind::Thumb, &mut thumb_enc, &fr, 480, 270)?;
            next_thumb = now + Duration::from_millis(1000);
        }
        if flags.live.load(Ordering::Relaxed) {
            if live_enc.is_none() {
                live_enc = Some(H264Encoder::software(mk(1280, 720, 5, 800, 2000))?);
            }
            if now >= next_live {
                let fr = live_s.frame(ts / 1000, idx).to_vec();
                emit(StreamKind::Live, live_enc.as_mut().unwrap(), &fr, 1280, 720)?;
                next_live = now + Duration::from_millis(200);
            }
        } else {
            live_enc = None;
        }
        let wake = [next_rec, next_thumb, if flags.live.load(Ordering::Relaxed) { next_live } else { next_rec }]
            .into_iter()
            .min()
            .unwrap();
        std::thread::sleep(wake.saturating_duration_since(Instant::now()).min(Duration::from_millis(200)));
    }
    Ok(())
}
