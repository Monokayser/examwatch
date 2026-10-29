//! `ew-server.toml`. Everything has a sane default so a first run works with
//! only the lab list filled in.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use ipnet::IpNet;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// QUIC endpoint for student agents.
    #[serde(default = "d_agent_listen")]
    pub agent_listen: SocketAddr,
    /// HTTPS dashboard for teachers.
    #[serde(default = "d_http_listen")]
    pub http_listen: SocketAddr,
    /// Serve the dashboard over TLS (turn off only for localhost development).
    #[serde(default = "d_true")]
    pub http_tls: bool,
    #[serde(default = "d_data_dir")]
    pub data_dir: PathBuf,
    /// Host names / IPs teachers use to reach the dashboard (certificate SANs).
    /// Defaults to `localhost` plus this computer's name.
    #[serde(default)]
    pub tls_names: Vec<String>,
    /// Shared secret pushed to student PCs by GPO; agents without it are rejected.
    pub enroll_token: String,
    #[serde(default = "d_retention")]
    pub retention_hours: u64,
    /// Length of one recording segment.
    #[serde(default = "d_segment")]
    pub segment_secs: u64,
    #[serde(default)]
    pub labs: Vec<LabConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub room: String,
    /// Student PCs whose address falls in one of these subnets belong to this lab.
    /// Labs are on separate VLANs, so this needs no per-PC configuration.
    #[serde(default)]
    pub subnets: Vec<IpNet>,
    /// Optional explicit PC-name prefixes (e.g. "B19-") for shared subnets.
    #[serde(default)]
    pub pc_prefixes: Vec<String>,
    #[serde(default)]
    pub expected_pcs: u32,
}

fn d_agent_listen() -> SocketAddr {
    "0.0.0.0:4433".parse().unwrap()
}
fn d_http_listen() -> SocketAddr {
    "0.0.0.0:8443".parse().unwrap()
}
fn d_true() -> bool {
    true
}
fn d_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn d_retention() -> u64 {
    48
}
fn d_segment() -> u64 {
    60
}

pub const UNASSIGNED_LAB: &str = "unassigned";

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.enroll_token.len() >= 16, "enroll_token must be at least 16 characters");
        anyhow::ensure!(self.retention_hours >= 1, "retention_hours must be >= 1");
        anyhow::ensure!((10..=600).contains(&self.segment_secs), "segment_secs must be 10..600");
        let mut seen = std::collections::HashSet::new();
        for l in &self.labs {
            anyhow::ensure!(!l.id.is_empty() && l.id != UNASSIGNED_LAB, "invalid lab id {:?}", l.id);
            anyhow::ensure!(seen.insert(&l.id), "duplicate lab id {}", l.id);
        }
        Ok(())
    }

    /// Picks the lab for a connecting PC: name prefix first (most specific), then subnet.
    pub fn lab_for(&self, ip: IpAddr, pc_name: &str) -> Option<&LabConfig> {
        self.lab_by_prefix(pc_name).or_else(|| self.lab_by_subnet(ip))
    }

    pub fn lab_by_prefix(&self, pc_name: &str) -> Option<&LabConfig> {
        let upper = pc_name.to_uppercase();
        self.labs.iter().find(|l| l.pc_prefixes.iter().any(|p| upper.starts_with(&p.to_uppercase())))
    }

    pub fn lab_by_subnet(&self, ip: IpAddr) -> Option<&LabConfig> {
        self.labs.iter().find(|l| l.subnets.iter().any(|n| n.contains(&ip)))
    }

    pub fn has_lab(&self, id: &str) -> bool {
        self.labs.iter().any(|l| l.id == id)
    }
}

/// The network a PC belongs to, as learned by "Assign to lab": /24 for IPv4, /64 for IPv6.
pub fn subnet_of(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

pub const EXAMPLE: &str = r#"# ExamWatch server configuration
agent_listen = "0.0.0.0:4433"     # QUIC (UDP) from student VLANs
http_listen  = "0.0.0.0:8443"     # teacher dashboard (HTTPS)
http_tls     = true
data_dir     = "data"
enroll_token = "CHANGE-ME-to-a-long-random-string"
retention_hours = 48
segment_secs = 60

# One entry per lab. Subnets map each lab VLAN to its lab.
[[labs]]
id = "anim1"
name = "3D Animation Lab 1"
room = "1018"
subnets = ["10.10.18.0/24"]
expected_pcs = 28

[[labs]]
id = "visarts"
name = "Visual Arts & Communication"
room = "1019"
subnets = ["10.10.19.0/24"]
expected_pcs = 27

[[labs]]
id = "game"
name = "Game & AR/VR Development"
room = "1015"
subnets = ["10.10.15.0/24"]
expected_pcs = 26

[[labs]]
id = "anim2"
name = "3D Animation Lab 2"
room = "1018 (AB-04)"
subnets = ["10.10.28.0/24"]
expected_pcs = 27

[[labs]]
id = "profdev"
name = "Professional Development"
room = "1020 (AB-04)"
subnets = ["10.10.20.0/24"]
expected_pcs = 6
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_maps_labs() {
        let cfg: Config = toml::from_str(EXAMPLE).unwrap();
        // the example token is long enough; real deployments must change it
        cfg.validate().unwrap();
        assert_eq!(cfg.labs.len(), 5);
        assert_eq!(cfg.lab_for("10.10.19.44".parse().unwrap(), "PC-01").unwrap().id, "visarts");
        assert!(cfg.lab_for("192.168.1.5".parse().unwrap(), "PC-01").is_none());
    }

    #[test]
    fn prefix_beats_subnet() {
        let mut cfg: Config = toml::from_str(EXAMPLE).unwrap();
        cfg.labs[4].pc_prefixes = vec!["pd-".into()];
        assert_eq!(cfg.lab_for("10.10.19.44".parse().unwrap(), "PD-03").unwrap().id, "profdev");
    }

    #[test]
    fn subnet_keys() {
        assert_eq!(subnet_of("10.10.19.44".parse().unwrap()), "10.10.19.0/24");
        assert_eq!(subnet_of("fd00:1:2:3::9".parse().unwrap()), "fd00:1:2:3::/64");
    }

    #[test]
    fn rejects_duplicate_lab() {
        let mut cfg: Config = toml::from_str(EXAMPLE).unwrap();
        cfg.labs[1].id = "anim1".into();
        assert!(cfg.validate().is_err());
    }
}
