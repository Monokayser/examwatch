//! Agent settings. Written by the student installer to
//! `HKLM\SOFTWARE\ExamWatch` (from `examwatch-student.cfg`); command-line
//! flags override them for development.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Settings {
    pub server: String,
    pub pin: String,
    pub token: String,
    pub machine_id: String,
    pub pc_name: String,
}

pub fn load() -> Result<Settings> {
    let mut s = Settings {
        server: reg("Server").unwrap_or_default(),
        pin: reg("ServerPin").unwrap_or_default(),
        token: reg("EnrollToken").unwrap_or_default(),
        machine_id: machine_guid().unwrap_or_else(|| format!("pc-{}", pc_name())),
        pc_name: pc_name(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--server" => s.server = args.next().context("--server needs a value")?,
            "--pin" => s.pin = args.next().context("--pin needs a value")?,
            "--token" => s.token = args.next().context("--token needs a value")?,
            "--machine-id" => s.machine_id = args.next().context("--machine-id needs a value")?,
            "--pc-name" => s.pc_name = args.next().context("--pc-name needs a value")?,
            _ => {}
        }
    }
    if !s.server.contains(':') && !s.server.is_empty() {
        s.server = format!("{}:{}", s.server, ew_proto::DEFAULT_PORT);
    }
    anyhow::ensure!(!s.server.is_empty() && !s.pin.is_empty() && !s.token.is_empty(), "ExamWatch is not configured (reinstall with examwatch-student.cfg next to the installer)");
    Ok(s)
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "UNKNOWN-PC".into())
}

#[cfg(windows)]
fn reg_at(key: &str, name: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    unsafe {
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
        .ok()
        .ok()?;
        let n = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..n]).trim().to_string()).filter(|s| !s.is_empty())
    }
}

#[cfg(windows)]
fn reg(name: &str) -> Option<String> {
    reg_at(r"SOFTWARE\ExamWatch", name)
}

#[cfg(windows)]
fn machine_guid() -> Option<String> {
    reg_at(r"SOFTWARE\Microsoft\Cryptography", "MachineGuid")
}

#[cfg(not(windows))]
fn reg(_: &str) -> Option<String> {
    None
}
#[cfg(not(windows))]
fn machine_guid() -> Option<String> {
    None
}
