//! ExamWatch wire protocol shared by the agent, the simulator and the server.
//!
//! One QUIC connection per student PC (ALPN `ew/1`):
//! * a bidirectional **control** stream opened by the agent, carrying
//!   length-prefixed JSON messages ([`AgentMsg`] / [`ServerMsg`]);
//! * one unidirectional **media** stream per [`StreamKind`], each starting
//!   with a single kind byte followed by back-to-back [`FrameHeader`] + payload.
//!
//! Media payloads are H.264 Annex-B access units. Keyframes always carry SPS/PPS
//! in-band so any stream can be joined (or a recording segment decoded) at a keyframe.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ALPN: &[u8] = b"ew/1";
pub const DEFAULT_PORT: u16 = 4433;
/// Hard upper bound for one control message (protects the server from garbage).
pub const MAX_CONTROL_MSG: usize = 64 * 1024;
/// Hard upper bound for one encoded frame (a 1080p IDR is far below this).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

pub mod client;
pub mod tls;

// ---------------------------------------------------------------------------
// Control messages
// ---------------------------------------------------------------------------

/// Which hardware path the agent ended up with; shown in the dashboard pre-flight.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    /// Hardware MFT on the display adapter (NVENC on the lab PCs).
    Hardware,
    /// Windows' built-in software H.264 MFT (GT 730 labs).
    Software,
    /// Synthetic frames from `ew-sim`.
    Simulated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello {
        machine_id: String,
        pc_name: String,
        agent_version: String,
        enroll_token: String,
        encoder: EncoderKind,
        gpu: String,
        screen_w: u32,
        screen_h: u32,
        monitors: u32,
        /// Agent wall clock (unix µs) when this message was sent; used for clock offset.
        sent_us: u64,
    },
    Heartbeat {
        sent_us: u64,
        cpu_pct: f32,
        rss_mb: f32,
        /// Current governor level, 0 = full rate, higher = more throttled.
        throttle: u8,
    },
    /// Student pressed V1 and typed an ID.
    Claim { student_id: String },
    /// Something the teacher should know (monitor count changed, secure desktop, …).
    Event { kind: String, detail: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        lab_id: String,
        lab_name: String,
        exam: Option<ExamInfo>,
        server_us: u64,
    },
    ExamOpened { exam: ExamInfo },
    ExamClosed { exam_id: i64 },
    ClaimResult { ok: bool, student_id: String, reason: Option<String> },
    /// A teacher enlarged this screen: start the LIVE stream.
    LiveOn { max_fps: u32, max_kbps: u32 },
    LiveOff,
    /// Congestion hint from the server (0 = fine … 3 = heavily congested).
    Pressure { level: u8 },
    /// Server rejected the agent (bad token, unknown lab, …). Agent backs off.
    Reject { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExamInfo {
    pub exam_id: i64,
    pub name: String,
    /// When true, the V1 dialog only accepts IDs from the roster.
    pub roster_enforced: bool,
}

pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg)?;
    if body.len() > MAX_CONTROL_MSG {
        bail!("control message too large");
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

/// Returns `Ok(None)` on clean end of stream.
pub async fn read_msg<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncReadExt + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_CONTROL_MSG {
        bail!("control message too large ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body).context("bad control message")?))
}

// ---------------------------------------------------------------------------
// Media framing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum StreamKind {
    /// ≤480×270 @ ≤1 fps, always on during an exam. Feeds the grid.
    Thumb = 1,
    /// ≤720p @ 2 fps, always on during an exam. Recorded.
    Rec = 2,
    /// Up to native @ ≤15 fps, only while a teacher has the screen enlarged.
    Live = 3,
}

impl StreamKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Thumb),
            2 => Some(Self::Rec),
            3 => Some(Self::Live),
            _ => None,
        }
    }
}

pub const FLAG_KEY: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 17;

/// Fixed 17-byte little-endian header in front of every encoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub flags: u8,
    pub width: u16,
    pub height: u16,
    /// Capture time, unix µs, already corrected to server clock by the agent.
    pub pts_us: u64,
    pub len: u32,
}

impl FrameHeader {
    pub fn is_key(&self) -> bool {
        self.flags & FLAG_KEY != 0
    }

    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut b = [0u8; FRAME_HEADER_LEN];
        b[0] = self.flags;
        b[1..3].copy_from_slice(&self.width.to_le_bytes());
        b[3..5].copy_from_slice(&self.height.to_le_bytes());
        b[5..13].copy_from_slice(&self.pts_us.to_le_bytes());
        b[13..17].copy_from_slice(&self.len.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; FRAME_HEADER_LEN]) -> Self {
        Self {
            flags: b[0],
            width: u16::from_le_bytes([b[1], b[2]]),
            height: u16::from_le_bytes([b[3], b[4]]),
            pts_us: u64::from_le_bytes(b[5..13].try_into().unwrap()),
            len: u32::from_le_bytes(b[13..17].try_into().unwrap()),
        }
    }
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, h: &FrameHeader, data: &[u8]) -> Result<()> {
    debug_assert_eq!(h.len as usize, data.len());
    w.write_all(&h.encode()).await?;
    w.write_all(data).await?;
    Ok(())
}

/// Reads one frame into `buf` (reused across calls, so steady state allocates nothing).
pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<Option<FrameHeader>> {
    let mut hb = [0u8; FRAME_HEADER_LEN];
    match r.read_exact(&mut hb).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let h = FrameHeader::decode(&hb);
    if h.len as usize > MAX_FRAME {
        bail!("frame too large ({} bytes)", h.len);
    }
    buf.resize(h.len as usize, 0);
    r.read_exact(buf).await?;
    Ok(Some(h))
}

// ---------------------------------------------------------------------------
// Student ID rules (shared so the agent can pre-validate before asking the server)
// ---------------------------------------------------------------------------

/// Normalises what a student typed: trims, uppercases, removes inner spaces.
pub fn normalize_student_id(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase()
}

/// Format check used when no roster is loaded: 3–32 chars of A–Z, 0–9, '-', '_'.
pub fn student_id_format_ok(id: &str) -> bool {
    (3..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_roundtrip() {
        let h = FrameHeader { flags: FLAG_KEY, width: 1280, height: 720, pts_us: 1_761_700_000_123_456, len: 4242 };
        assert_eq!(FrameHeader::decode(&h.encode()), h);
        assert!(h.is_key());
    }

    #[test]
    fn student_id_rules() {
        assert_eq!(normalize_student_id("  221-15 4021 "), "221-154021");
        assert_eq!(normalize_student_id("ab12"), "AB12");
        assert!(student_id_format_ok("221-15-4021"));
        assert!(!student_id_format_ok("ab"));
        assert!(!student_id_format_ok("221/15"));
    }

    #[tokio::test]
    async fn control_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let m = AgentMsg::Claim { student_id: "S-1".into() };
        write_msg(&mut a, &m).await.unwrap();
        let got: AgentMsg = read_msg(&mut b).await.unwrap().unwrap();
        match got {
            AgentMsg::Claim { student_id } => assert_eq!(student_id, "S-1"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn frame_roundtrip_and_eof() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let data = vec![7u8; 1000];
        let h = FrameHeader { flags: 0, width: 480, height: 270, pts_us: 5, len: 1000 };
        write_frame(&mut a, &h, &data).await.unwrap();
        drop(a);
        let mut buf = Vec::new();
        assert_eq!(read_frame(&mut b, &mut buf).await.unwrap(), Some(h));
        assert_eq!(buf, data);
        assert_eq!(read_frame(&mut b, &mut buf).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_frame_rejected() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let h = FrameHeader { flags: 0, width: 1, height: 1, pts_us: 0, len: (MAX_FRAME + 1) as u32 };
        a.write_all(&h.encode()).await.unwrap();
        let mut buf = Vec::new();
        assert!(read_frame(&mut b, &mut buf).await.is_err());
    }
}
