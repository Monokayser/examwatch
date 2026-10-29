//! QUIC endpoint for student agents.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use ew_proto::{read_frame, read_msg, write_msg, AgentMsg, ServerMsg, StreamKind};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::hub::Hub;

pub fn endpoint(hub: &Hub, cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Result<quinn::Endpoint> {
    let mut tls = rustls::ServerConfig::builder_with_provider(ew_proto::tls::provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    tls.alpn_protocols = vec![ew_proto::ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(Arc::new(ew_proto::tls::transport()));
    Ok(quinn::Endpoint::server(cfg, hub.cfg.agent_listen)?)
}

pub async fn run(hub: Arc<Hub>, ep: quinn::Endpoint) {
    tracing::info!("agent endpoint listening on {}", hub.cfg.agent_listen);
    while let Some(incoming) = ep.accept().await {
        let hub = hub.clone();
        tokio::spawn(async move {
            let remote = incoming.remote_address();
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle(hub, conn).await {
                        tracing::debug!(%remote, "agent session ended: {e:#}");
                    }
                }
                Err(e) => tracing::debug!(%remote, "handshake failed: {e}"),
            }
        });
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle(hub: Arc<Hub>, conn: quinn::Connection) -> Result<()> {
    let ip = conn.remote_address().ip();
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi())
        .await
        .context("no control stream")??;

    let hello: AgentMsg = tokio::time::timeout(Duration::from_secs(5), read_msg(&mut recv))
        .await
        .context("hello timeout")??
        .context("closed before hello")?;
    let AgentMsg::Hello { machine_id, pc_name, agent_version, enroll_token, encoder, gpu, screen_w, screen_h, monitors, .. } = hello else {
        bail!("first message was not hello");
    };
    if !ct_eq(enroll_token.as_bytes(), hub.cfg.enroll_token.as_bytes()) {
        let _ = write_msg(&mut send, &ServerMsg::Reject { reason: "enrollment token mismatch".into() }).await;
        let _ = send.finish();
        hub.db.audit(&pc_name, "agent_rejected", &format!("bad token from {ip}"));
        bail!("bad enroll token from {ip}");
    }
    if machine_id.is_empty() || machine_id.len() > 64 || pc_name.is_empty() || pc_name.len() > 64 {
        bail!("bad hello fields");
    }
    let lab_id = hub.resolve_lab(ip, &pc_name);
    let enc_name = serde_json::to_value(encoder)?.as_str().unwrap_or("").to_string();
    hub.db.touch_machine(&machine_id, &pc_name, &lab_id, &ip.to_string(), &enc_name, &gpu)?;
    tracing::info!(%machine_id, %pc_name, %lab_id, %agent_version, ?encoder, "agent connected from {ip}");

    // Server → agent messages go through a small queue drained by one writer task.
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ServerMsg>(32);
    let (gen, welcome) = hub.agent_connected(
        &machine_id, &pc_name, &lab_id, ip.to_string(), encoder, gpu, (screen_w, screen_h), monitors, ctl_tx,
    );
    write_msg(&mut send, &welcome).await?;
    let writer = tokio::spawn(async move {
        while let Some(m) = ctl_rx.recv().await {
            if write_msg(&mut send, &m).await.is_err() {
                break;
            }
        }
    });

    // Media streams.
    let media = {
        let hub = hub.clone();
        let conn = conn.clone();
        let machine_id = machine_id.clone();
        tokio::spawn(async move {
            while let Ok(mut uni) = conn.accept_uni().await {
                let hub = hub.clone();
                let machine_id = machine_id.clone();
                tokio::spawn(async move {
                    let Ok(k) = uni.read_u8().await else { return };
                    let Some(kind) = StreamKind::from_u8(k) else { return };
                    let mut buf = Vec::with_capacity(64 * 1024);
                    loop {
                        match read_frame(&mut uni, &mut buf).await {
                            Ok(Some(h)) => hub.media(&machine_id, kind, h, Bytes::copy_from_slice(&buf)),
                            Ok(None) => break,
                            Err(e) => {
                                tracing::debug!(%machine_id, ?kind, "media stream error: {e:#}");
                                break;
                            }
                        }
                    }
                });
            }
        })
    };

    // Control loop.
    let result: Result<()> = async {
        while let Some(msg) = read_msg::<_, AgentMsg>(&mut recv).await? {
            match msg {
                AgentMsg::Heartbeat { cpu_pct, rss_mb, throttle, .. } => hub.heartbeat(&machine_id, cpu_pct, rss_mb, throttle),
                AgentMsg::Claim { student_id } => {
                    let reply = hub.claim(&machine_id, &student_id);
                    hub_send(&hub, &machine_id, reply);
                }
                AgentMsg::Event { kind, detail } => {
                    hub.agent_event(&machine_id, &truncate(&kind, 40), &truncate(&detail, 200))
                }
                AgentMsg::Hello { .. } => bail!("duplicate hello"),
            }
        }
        Ok(())
    }
    .await;

    hub.agent_disconnected(&machine_id, gen);
    writer.abort();
    media.abort();
    conn.close(0u32.into(), b"bye");
    result
}

fn hub_send(hub: &Hub, machine_id: &str, msg: ServerMsg) {
    hub.send_to(machine_id, msg);
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
