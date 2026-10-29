//! TLS helpers. The server uses a self-signed certificate (or one from the
//! school's CA); agents **pin its SHA-256 fingerprint**, which is pushed to
//! every PC by Group Policy. No public CA is trusted, so a rogue server on the
//! LAN cannot impersonate ExamWatch.

use std::sync::Arc;

use anyhow::Result;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct PinnedVerifier {
    pin: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity.as_ref()).eq_ignore_ascii_case(&self.pin) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("server certificate fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// QUIC client config for agents: TLS 1.3 only, pinned server certificate,
/// short idle timeout so a dead link is noticed in ~3 s.
pub fn client_config(pin_sha256: &str) -> Result<quinn::ClientConfig> {
    let provider = provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { pin: pin_sha256.to_string(), provider }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![crate::ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(Arc::new(transport()));
    Ok(cfg)
}

/// Transport tuning shared by both ends.
pub fn transport() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(std::time::Duration::from_secs(3).try_into().unwrap()));
    t.keep_alive_interval(Some(std::time::Duration::from_millis(1000)));
    // Small windows: one student never needs more than a few hundred kB in flight.
    t.stream_receive_window(quinn::VarInt::from_u32(1024 * 1024));
    t.receive_window(quinn::VarInt::from_u32(4 * 1024 * 1024));
    t.send_window(4 * 1024 * 1024);
    t.max_concurrent_uni_streams(quinn::VarInt::from_u32(8));
    t.max_concurrent_bidi_streams(quinn::VarInt::from_u32(2));
    t
}
