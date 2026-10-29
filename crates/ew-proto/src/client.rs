//! Agent-side connection manager, shared by `ew-agent` and `ew-sim`.
//!
//! * Reconnects forever with jittered backoff (0.5 s → 5 s), plus a random
//!   0–3 s delay on first start so a whole lab booting together doesn't
//!   stampede the server.
//! * THUMB and LIVE frames are dropped when the link is busy or down (stale
//!   pictures are worthless).
//! * REC frames are never dropped while there is room: they queue in a
//!   bounded in-memory backlog (default 24 MB ≈ 20–60 min of 720p @ 2 fps)
//!   that survives disconnects and is uploaded, paced, after reconnecting.

use std::collections::VecDeque;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use parking_lot_shim::Mutex;
use rand::Rng;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Notify};

use crate::{read_msg, write_frame, write_msg, AgentMsg, EncoderKind, FrameHeader, ServerMsg, StreamKind};

/// Tiny std-mutex wrapper so this crate doesn't need parking_lot.
mod parking_lot_shim {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub fn new(v: T) -> Self {
            Self(std::sync::Mutex::new(v))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap_or_else(|e| e.into_inner())
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// `host:port` of ew-server.
    pub server: String,
    pub pin_sha256: String,
    pub enroll_token: String,
    pub machine_id: String,
    pub pc_name: String,
    pub agent_version: String,
    pub encoder: EncoderKind,
    pub gpu: String,
    pub screen: (u32, u32),
    pub monitors: u32,
    pub rec_backlog_bytes: usize,
    /// Upload pace for the REC backlog after a reconnect.
    pub backlog_kbps: u32,
    pub start_jitter: Duration,
}

#[derive(Debug, Clone)]
pub enum ClientEvent {
    Connected,
    Server(ServerMsg),
    /// Link lost. The app should force a REC keyframe so the backlog stays decodable.
    Disconnected,
}

struct Shared {
    rec: Mutex<RecBacklog>,
    rec_notify: Notify,
    thumb: Mutex<Option<(FrameHeader, Vec<u8>)>>,
    live: Mutex<Option<(FrameHeader, Vec<u8>)>>,
    media_notify: Notify,
}

#[derive(Default)]
struct RecBacklog {
    q: VecDeque<(FrameHeader, Vec<u8>)>,
    bytes: usize,
    cap: usize,
    dropped: u64,
}

impl RecBacklog {
    fn push(&mut self, h: FrameHeader, d: Vec<u8>) {
        self.bytes += d.len();
        self.q.push_back((h, d));
        // Over budget: drop whole GOPs from the front so what remains still starts on a keyframe.
        while self.bytes > self.cap && self.q.len() > 1 {
            let (_, d) = self.q.pop_front().unwrap();
            self.bytes -= d.len();
            self.dropped += 1;
            while self.q.front().map_or(false, |(h, _)| !h.is_key()) {
                let (_, d) = self.q.pop_front().unwrap();
                self.bytes -= d.len();
                self.dropped += 1;
            }
        }
    }
}

#[derive(Clone)]
pub struct ClientHandle {
    shared: Arc<Shared>,
    cmd: mpsc::Sender<AgentMsg>,
}

impl ClientHandle {
    /// Queue an encoded frame. Never blocks the caller (the capture thread).
    pub fn push_frame(&self, kind: StreamKind, hdr: FrameHeader, data: Vec<u8>) {
        match kind {
            StreamKind::Rec => {
                self.shared.rec.lock().push(hdr, data);
                self.shared.rec_notify.notify_one();
            }
            StreamKind::Thumb => {
                *self.shared.thumb.lock() = Some((hdr, data));
                self.shared.media_notify.notify_one();
            }
            StreamKind::Live => {
                *self.shared.live.lock() = Some((hdr, data));
                self.shared.media_notify.notify_one();
            }
        }
    }

    /// Control message (claim, heartbeat, event). Dropped if the queue is full.
    pub fn send(&self, msg: AgentMsg) {
        let _ = self.cmd.try_send(msg);
    }

    pub fn rec_backlog(&self) -> (usize, usize, u64) {
        let r = self.shared.rec.lock();
        (r.q.len(), r.bytes, r.dropped)
    }
}

/// Starts the connection manager on the current Tokio runtime.
pub fn spawn(cfg: ClientConfig) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    let shared = Arc::new(Shared {
        rec: Mutex::new(RecBacklog { cap: cfg.rec_backlog_bytes, ..Default::default() }),
        rec_notify: Notify::new(),
        thumb: Mutex::new(None),
        live: Mutex::new(None),
        media_notify: Notify::new(),
    });
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (ev_tx, ev_rx) = mpsc::channel(64);
    tokio::spawn(run(cfg, shared.clone(), cmd_rx, ev_tx));
    (ClientHandle { shared, cmd: cmd_tx }, ev_rx)
}

async fn run(cfg: ClientConfig, shared: Arc<Shared>, mut cmd_rx: mpsc::Receiver<AgentMsg>, ev_tx: mpsc::Sender<ClientEvent>) {
    if !cfg.start_jitter.is_zero() {
        let ms = rand::thread_rng().gen_range(0..=cfg.start_jitter.as_millis() as u64);
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    let mut backoff = Duration::from_millis(500);
    let mut endpoint: Option<quinn::Endpoint> = None;
    loop {
        let res = session(&cfg, &shared, &mut cmd_rx, &ev_tx, &mut endpoint, &mut backoff).await;
        if let Err(e) = &res {
            tracing::debug!("session ended: {e:#}");
        }
        let _ = ev_tx.send(ClientEvent::Disconnected).await;
        // Live pictures are stale after a drop; recordings stay queued.
        *shared.thumb.lock() = None;
        *shared.live.lock() = None;
        let jitter = rand::thread_rng().gen_range(0..=backoff.as_millis() as u64 / 2);
        tokio::time::sleep(backoff + Duration::from_millis(jitter)).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn resolve(server: &str) -> Result<SocketAddr> {
    server
        .to_socket_addrs()
        .with_context(|| format!("resolving {server}"))?
        .find(|a| a.is_ipv4())
        .or_else(|| server.to_socket_addrs().ok()?.next())
        .context("server address did not resolve")
}

async fn session(
    cfg: &ClientConfig,
    shared: &Arc<Shared>,
    cmd_rx: &mut mpsc::Receiver<AgentMsg>,
    ev_tx: &mpsc::Sender<ClientEvent>,
    endpoint: &mut Option<quinn::Endpoint>,
    backoff: &mut Duration,
) -> Result<()> {
    let addr = resolve(&cfg.server)?;
    if endpoint.is_none() {
        let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())?;
        ep.set_default_client_config(crate::tls::client_config(&cfg.pin_sha256)?);
        *endpoint = Some(ep);
    }
    let conn = tokio::time::timeout(Duration::from_secs(5), endpoint.as_ref().unwrap().connect(addr, "examwatch")?)
        .await
        .context("connect timeout")??;
    let (mut send, mut recv) = conn.open_bi().await?;
    write_msg(&mut send, &AgentMsg::Hello {
        machine_id: cfg.machine_id.clone(),
        pc_name: cfg.pc_name.clone(),
        agent_version: cfg.agent_version.clone(),
        enroll_token: cfg.enroll_token.clone(),
        encoder: cfg.encoder,
        gpu: cfg.gpu.clone(),
        screen_w: cfg.screen.0,
        screen_h: cfg.screen.1,
        monitors: cfg.monitors,
        sent_us: crate::now_us(),
    })
    .await?;
    let first: ServerMsg = tokio::time::timeout(Duration::from_secs(5), read_msg(&mut recv))
        .await
        .context("welcome timeout")??
        .context("closed before welcome")?;
    if let ServerMsg::Reject { reason } = &first {
        bail!("rejected by server: {reason}");
    }
    *backoff = Duration::from_millis(500);
    let _ = ev_tx.send(ClientEvent::Connected).await;
    let _ = ev_tx.send(ClientEvent::Server(first)).await;

    // Server → app.
    let reader = {
        let ev_tx = ev_tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(m)) = read_msg::<_, ServerMsg>(&mut recv).await {
                if ev_tx.send(ClientEvent::Server(m)).await.is_err() {
                    break;
                }
            }
        })
    };

    // REC uploader (backlog first, paced, then live).
    let rec_task = {
        let conn = conn.clone();
        let shared = shared.clone();
        let kbps = cfg.backlog_kbps.max(256);
        tokio::spawn(async move {
            let res: Result<()> = async {
                let mut s = conn.open_uni().await?;
                s.set_priority(-1)?; // below thumbnails/live: it can wait, it is never dropped
                s.write_u8(StreamKind::Rec as u8).await?;
                loop {
                    let next = shared.rec.lock().q.front().cloned();
                    let Some((h, d)) = next else {
                        shared.rec_notify.notified().await;
                        continue;
                    };
                    write_frame(&mut s, &h, &d).await?;
                    {
                        let mut r = shared.rec.lock();
                        // Only pop what we sent (a new frame may have been pushed meanwhile).
                        if r.q.front().map_or(false, |(fh, _)| fh.pts_us == h.pts_us) {
                            let (_, d) = r.q.pop_front().unwrap();
                            r.bytes -= d.len();
                        }
                    }
                    // Pace only while catching up on a backlog.
                    if shared.rec.lock().q.len() > 2 {
                        let ms = (d.len() as u64 * 8) / kbps as u64;
                        tokio::time::sleep(Duration::from_millis(ms.max(1))).await;
                    }
                }
            }
            .await;
            res
        })
    };

    // THUMB + LIVE: latest-wins slots.
    let media_task = {
        let conn = conn.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            let res: Result<()> = async {
                let mut thumb: Option<quinn::SendStream> = None;
                let mut live: Option<quinn::SendStream> = None;
                loop {
                    let t = shared.thumb.lock().take();
                    let l = shared.live.lock().take();
                    if t.is_none() && l.is_none() {
                        shared.media_notify.notified().await;
                        continue;
                    }
                    if let Some((h, d)) = l {
                        if live.is_none() {
                            let mut s = conn.open_uni().await?;
                            s.set_priority(2)?;
                            s.write_u8(StreamKind::Live as u8).await?;
                            live = Some(s);
                        }
                        write_frame(live.as_mut().unwrap(), &h, &d).await?;
                    }
                    if let Some((h, d)) = t {
                        if thumb.is_none() {
                            let mut s = conn.open_uni().await?;
                            s.set_priority(1)?;
                            s.write_u8(StreamKind::Thumb as u8).await?;
                            thumb = Some(s);
                        }
                        write_frame(thumb.as_mut().unwrap(), &h, &d).await?;
                    }
                }
            }
            .await;
            res
        })
    };

    // App → server control, until something breaks.
    let result: Result<()> = async {
        loop {
            tokio::select! {
                m = cmd_rx.recv() => match m {
                    Some(m) => write_msg(&mut send, &m).await?,
                    None => bail!("app closed"),
                },
                e = conn.closed() => bail!("connection closed: {e}"),
            }
        }
    }
    .await;
    reader.abort();
    rec_task.abort();
    media_task.abort();
    conn.close(0u32.into(), b"bye");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(key: bool, pts: u64) -> FrameHeader {
        FrameHeader { flags: key as u8, width: 1, height: 1, pts_us: pts, len: 10 }
    }

    #[test]
    fn backlog_drops_whole_gops_from_front() {
        let mut b = RecBacklog { cap: 35, ..Default::default() };
        for (i, key) in [true, false, false, true, false].iter().enumerate() {
            b.push(h(*key, i as u64), vec![0; 10]);
        }
        // 50 bytes > 35: first GOP (3 frames) removed, remaining starts with keyframe 3.
        assert!(b.q.front().unwrap().0.is_key());
        assert_eq!(b.q.front().unwrap().0.pts_us, 3);
        assert_eq!(b.bytes, 20);
        assert_eq!(b.dropped, 3);
    }
}
