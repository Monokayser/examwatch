//! ExamWatch central server.
//!
//! ```text
//! ew-server init                         write ew-server.toml if missing
//! ew-server add-teacher <name> [--admin]  password from EW_PASSWORD or stdin
//! ew-server fingerprint                  print the certificate pin for the GPO
//! ew-server [run]                        start serving (console)
//! ew-server service                      start serving under the Windows service manager
//! ```
//! `--config <path>` (default `ew-server.toml`) works with every command.

mod config;
mod crypto;
mod db;
mod http;
mod hub;
mod ingest;
mod recorder;
mod retention;
#[cfg(windows)]
mod winsvc;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use config::Config;

fn filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,quinn=warn".into())
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cfg_path = match args.iter().position(|a| a == "--config") {
        Some(i) if i + 1 < args.len() => {
            let p = PathBuf::from(args.remove(i + 1));
            args.remove(i);
            p
        }
        Some(_) => bail!("--config needs a path"),
        None => PathBuf::from("ew-server.toml"),
    };
    let cmd = args.first().map(String::as_str).unwrap_or("run");
    if cmd != "service" {
        tracing_subscriber::fmt().with_env_filter(filter()).init();
    }

    match cmd {
        // Used by the teacher installer: idempotent, no prompts. Writes a config
        // with a random enrolment token and an absolute data folder, creates the
        // certificate, and leaves lab subnets to be learned from the dashboard.
        "init" => {
            if cfg_path.exists() {
                println!("{} already exists", cfg_path.display());
            } else {
                let data_dir = match args.iter().position(|a| a == "--data-dir") {
                    Some(i) => args.get(i + 1).map(PathBuf::from).context("--data-dir needs a path")?,
                    None => PathBuf::from("data"),
                };
                let token = hex::encode(crypto::random_key());
                let text: String = config::EXAMPLE
                    .replace("CHANGE-ME-to-a-long-random-string", &token)
                    .replace("data_dir     = \"data\"", &format!("data_dir     = {:?}", data_dir.to_string_lossy().replace('\\', "/")))
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("subnets"))
                    .map(|l| format!("{l}\n"))
                    .collect();
                if let Some(dir) = cfg_path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&cfg_path, text)?;
                println!("wrote {}", cfg_path.display());
            }
            let cfg = Config::load(&cfg_path)?;
            let (cert, _) = load_or_create_cert(&cfg)?;
            std::fs::write(cfg.data_dir.join("server.fingerprint.txt"), ew_proto::tls::fingerprint(cert.as_ref()))?;
            Ok(())
        }
        "add-teacher" => {
            let cfg = Config::load(&cfg_path)?;
            let name = args.get(1).context("usage: add-teacher <username> [--admin]")?;
            let admin = args.iter().any(|a| a == "--admin");
            let pw = match std::env::var("EW_PASSWORD") {
                Ok(p) => p,
                Err(_) => {
                    eprint!("Password for {name}: ");
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    s.trim_end().to_string()
                }
            };
            if pw.chars().count() < 10 {
                bail!("password must be at least 10 characters");
            }
            std::fs::create_dir_all(&cfg.data_dir)?;
            let db = db::Db::open(&cfg.data_dir.join("examwatch.db"))?;
            db.upsert_teacher(name, &http::hash_password(&pw)?, admin)?;
            db.audit("cli", "teacher_set", name);
            println!("teacher {name} saved");
            Ok(())
        }
        "fingerprint" => {
            let cfg = Config::load(&cfg_path)?;
            let (cert, _) = load_or_create_cert(&cfg)?;
            println!("{}", ew_proto::tls::fingerprint(cert.as_ref()));
            Ok(())
        }
        #[cfg(windows)]
        "service" => winsvc::start(cfg_path),
        "run" => {
            let cfg = Config::load(&cfg_path)?;
            tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run(cfg))
        }
        other => bail!("unknown command {other:?}"),
    }
}

pub async fn run(cfg: Config) -> Result<()> {
    if cfg.enroll_token.starts_with("CHANGE-ME") {
        tracing::warn!("enroll_token is still the example value; change it before deployment");
    }
    std::fs::create_dir_all(&cfg.data_dir)?;
    let rec_root = cfg.data_dir.join("rec");
    std::fs::create_dir_all(&rec_root)?;

    let db = Arc::new(db::Db::open(&cfg.data_dir.join("examwatch.db"))?);
    if db.teacher_count()? == 0 {
        tracing::warn!("no teacher accounts yet: run `ew-server add-teacher <name> --admin`");
    }
    let master = crypto::load_or_create_master_key(&cfg.data_dir.join("master.key"))?;
    let (cert, key) = load_or_create_cert(&cfg)?;
    let pin = ew_proto::tls::fingerprint(cert.as_ref());
    std::fs::write(cfg.data_dir.join("server.fingerprint.txt"), &pin)?;
    tracing::info!("certificate pin (push to student PCs via GPO): {pin}");

    // Recover from a crash mid-exam: purge first, then serve.
    retention::purge_due(&db, &rec_root, cfg.retention_hours);

    let recorder = recorder::Recorder::start(db.clone(), master, rec_root.clone(), cfg.segment_secs);
    let hub = hub::Hub::new(cfg.clone(), db.clone(), recorder, master)?;

    let ep = ingest::endpoint(&hub, cert.clone(), key.clone_key())?;
    tokio::spawn(ingest::run(hub.clone(), ep));
    tokio::spawn(retention::run(db.clone(), rec_root, cfg.retention_hours));

    let app = http::router(hub.clone(), cfg.http_tls, pin.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tracing::info!("dashboard on {}://{}", if cfg.http_tls { "https" } else { "http" }, cfg.http_listen);
    if cfg.http_tls {
        let mut tls = rustls::ServerConfig::builder_with_provider(ew_proto::tls::provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)?;
        // HTTP/1.1 only: WebSocket upgrades are not supported over HTTP/2 here.
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls));
        axum_server::bind_rustls(cfg.http_listen, rustls_cfg).serve(app).await?;
    } else {
        let listener = tokio::net::TcpListener::bind(cfg.http_listen).await?;
        axum::serve(listener, app).await?;
    }
    Ok(())
}

/// Self-signed certificate created on first run. The private key is sealed
/// with DPAPI. Agents pin the certificate; teacher PCs trust it via GPO.
fn load_or_create_cert(cfg: &Config) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let cert_p = cfg.data_dir.join("server.crt.der");
    let key_p = cfg.data_dir.join("server.key.sealed");
    if cert_p.exists() && key_p.exists() {
        let cert = std::fs::read(&cert_p)?;
        let key = crypto::unseal(&std::fs::read(&key_p)?)?;
        return Ok((CertificateDer::from(cert), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key))));
    }
    let mut names = cfg.tls_names.clone();
    if names.is_empty() {
        names.push("localhost".into());
        if let Ok(h) = std::env::var("COMPUTERNAME") {
            names.push(h);
        }
    }
    let ck = rcgen::generate_simple_self_signed(names)?;
    std::fs::create_dir_all(&cfg.data_dir)?;
    let cert_der = ck.cert.der().to_vec();
    let key_der = ck.key_pair.serialize_der();
    std::fs::write(&cert_p, &cert_der)?;
    std::fs::write(&key_p, crypto::seal(&key_der)?)?;
    // PEM copy for importing into teacher PCs' Trusted Root store via GPO.
    std::fs::write(cfg.data_dir.join("server.crt.pem"), ck.cert.pem())?;
    write_note(&cfg.data_dir)?;
    Ok((CertificateDer::from(cert_der), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der))))
}

fn write_note(dir: &Path) -> Result<()> {
    std::fs::write(
        dir.join("README-certificate.txt"),
        "server.crt.pem: import into Trusted Root Certification Authorities on teacher PCs (GPO).\n\
         server.fingerprint.txt: set as HKLM\\SOFTWARE\\ExamWatch\\ServerPin on student PCs (GPO).\n",
    )?;
    Ok(())
}
