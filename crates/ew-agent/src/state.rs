//! State shared by the UI thread, the network thread and the capture thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::Mutex;

use ew_proto::ExamInfo;
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct Shared {
    pub ui: Mutex<UiState>,
    /// Capture runs only while this is true (exam open AND Student ID confirmed).
    pub capturing: AtomicBool,
    pub live: AtomicBool,
    pub force_key: AtomicBool,
    /// server clock − local clock, µs; applied to every frame timestamp.
    pub clock_offset_us: AtomicI64,
    pub throttle: AtomicU8,
    pub quit: AtomicBool,
}

#[derive(Default, Clone)]
pub struct UiState {
    pub connected: bool,
    pub lab_name: String,
    pub exam: Option<ExamInfo>,
    pub student_id: Option<String>,
    pub claim_pending: bool,
    pub message: Option<String>,
}

impl Shared {
    pub fn now_us(&self) -> u64 {
        (ew_proto::now_us() as i64 + self.clock_offset_us.load(Ordering::Relaxed)).max(0) as u64
    }

    pub fn set_capturing(&self, on: bool) {
        if on && !self.capturing.swap(true, Ordering::SeqCst) {
            self.force_key.store(true, Ordering::SeqCst);
        } else if !on {
            self.capturing.store(false, Ordering::SeqCst);
            self.live.store(false, Ordering::SeqCst);
        }
    }
}

/// Remembers which Student ID this PC registered for which exam, so that after
/// a crash, reboot or reconnect the student never has to type it again.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct Saved {
    pub exam_id: i64,
    pub student_id: String,
}

fn saved_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("ExamWatch").join("session.json")
}

pub fn load_saved() -> Option<Saved> {
    serde_json::from_slice(&std::fs::read(saved_path()).ok()?).ok()
}

pub fn store_saved(s: Option<&Saved>) {
    let p = saved_path();
    match s {
        Some(s) => {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&p, serde_json::to_vec(s).unwrap_or_default());
        }
        None => {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Client-side check before asking the server: both fields match and the format is plausible.
pub fn validate_entry(id: &str, confirm: &str) -> Result<String, &'static str> {
    let a = ew_proto::normalize_student_id(id);
    let b = ew_proto::normalize_student_id(confirm);
    if a.is_empty() {
        return Err("Enter your Student ID.");
    }
    if !ew_proto::student_id_format_ok(&a) {
        return Err("That doesn't look like a Student ID. Use letters, digits and dashes only.");
    }
    if a != b {
        return Err("The two IDs don't match. Please type it again carefully.");
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_validation() {
        assert_eq!(validate_entry(" 221-15-4021", "221-15-4021 ").unwrap(), "221-15-4021");
        assert_eq!(validate_entry("ab12c", "AB12C").unwrap(), "AB12C");
        assert!(validate_entry("221-15-4021", "221-15-4012").is_err());
        assert!(validate_entry("", "").is_err());
        assert!(validate_entry("221/15", "221/15").is_err());
    }

    #[test]
    fn capturing_forces_keyframe_once() {
        let s = Shared::default();
        s.set_capturing(true);
        assert!(s.force_key.swap(false, Ordering::SeqCst));
        s.set_capturing(true);
        assert!(!s.force_key.load(Ordering::SeqCst));
        s.live.store(true, Ordering::SeqCst);
        s.set_capturing(false);
        assert!(!s.live.load(Ordering::SeqCst));
    }
}
