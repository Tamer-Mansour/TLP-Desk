//! rustls configuration: TLS 1.3 only, self-signed per-install certs, key pinning instead of a
//! PKI, mutual authentication (the host learns the viewer's key / device ID).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use tlp_desk_crypto::{fingerprint_of_cert, DeviceId, Fingerprint, Identity};

use crate::NetError;

/// ALPN protocol id. Deliberately version-less so a peer on another protocol version still
/// reaches the `Hello` check and gets a clear `Rejected(VersionMismatch)`.
pub const ALPN: &[u8] = b"tlp-desk";

/// How the viewer authenticates the host's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Dialed by IP/hostname without a known key: accept any key, report it (TOFU).
    /// The caller must check it against [`tlp_desk_crypto::KnownHosts`] and warn on change.
    Tofu,
    /// Dialed by 9-digit device ID: the key must hash to this ID.
    DeviceId(DeviceId),
    /// Full key pin (e.g. from the known-hosts store). Strongest option.
    Fingerprint(Fingerprint),
}

impl Pin {
    pub fn matches(&self, fp: &Fingerprint) -> bool {
        match self {
            Pin::Tofu => true,
            Pin::DeviceId(id) => fp.device_id() == *id,
            Pin::Fingerprint(want) => want == fp,
        }
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            Pin::Tofu => "any key".into(),
            Pin::DeviceId(id) => format!("device ID {id}"),
            Pin::Fingerprint(fp) => format!("key {fp}"),
        }
    }
}

impl From<Option<DeviceId>> for Pin {
    fn from(id: Option<DeviceId>) -> Self {
        id.map_or(Pin::Tofu, Pin::DeviceId)
    }
}

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn bad_cert(e: impl std::fmt::Display) -> rustls::Error {
    tracing::debug!(%e, "unparseable peer certificate");
    rustls::Error::InvalidCertificate(CertificateError::BadEncoding)
}

/// Server-cert verifier used by the viewer: pins the host key, records what it saw.
#[derive(Debug)]
pub(crate) struct PinnedServerVerifier {
    pin: Pin,
    pub(crate) observed: ObservedKey,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint_of_cert(end_entity).map_err(bad_cert)?;
        *self.observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(fp);
        if self.pin.matches(&fp) {
            Ok(ServerCertVerified::assertion())
        } else {
            tracing::warn!(
                expected = %self.pin.describe(),
                actual_id = %fp.device_id(),
                "host key does not match pin; refusing connection"
            );
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Client-cert verifier used by the host: any well-formed key is accepted at the TLS layer
/// (who may connect is decided by Accept prompt / code / password); the handshake signature
/// proves the viewer owns the key, so its device ID is authentic.
#[derive(Debug)]
pub(crate) struct AnyClientKeyVerifier {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyClientKeyVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        fingerprint_of_cert(end_entity).map_err(bad_cert)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn cert_and_key(identity: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(identity.cert_der().to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            identity.private_key_pkcs8_der().to_vec(),
        )),
    )
}

/// QUIC transport tuning shared by both sides.
pub(crate) fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(4)));
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(15)).expect("15s fits"),
    ));
    // Per-frame video streams: allow a few in flight, each up to one large keyframe.
    t.max_concurrent_uni_streams(32u32.into());
    t.max_concurrent_bidi_streams(4u32.into());
    t.stream_receive_window((8u32 * 1024 * 1024).into());
    t.receive_window((32u32 * 1024 * 1024).into());
    // Bound how much unacknowledged data we buffer so latency cannot grow unboundedly.
    t.send_window(8 * 1024 * 1024);
    Arc::new(t)
}

pub(crate) fn server_config(identity: &Identity) -> Result<quinn::ServerConfig, NetError> {
    let provider = provider();
    let (certs, key) = cert_and_key(identity);
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AnyClientKeyVerifier { provider }))
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.max_early_data_size = 0;
    let quic = QuicServerConfig::try_from(tls)
        .map_err(|e| NetError::Tls(rustls::Error::General(e.to_string())))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(transport_config());
    Ok(cfg)
}

/// Slot where the verifier records the host key it saw (even when it rejects it).
pub(crate) type ObservedKey = Arc<Mutex<Option<Fingerprint>>>;

/// Builds a client config for one dial; returns the slot where the observed host key lands.
pub(crate) fn client_config(
    identity: &Identity,
    pin: Pin,
) -> Result<(quinn::ClientConfig, ObservedKey), NetError> {
    let provider = provider();
    let observed = Arc::new(Mutex::new(None));
    let verifier = Arc::new(PinnedServerVerifier {
        pin,
        observed: observed.clone(),
        provider: provider.clone(),
    });
    let (certs, key) = cert_and_key(identity);
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let quic = QuicClientConfig::try_from(tls)
        .map_err(|e| NetError::Tls(rustls::Error::General(e.to_string())))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(transport_config());
    Ok((cfg, observed))
}

/// Fingerprint of the peer's certificate on an established connection.
pub(crate) fn peer_fingerprint(conn: &quinn::Connection) -> Result<Fingerprint, NetError> {
    let certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or_else(|| NetError::Unexpected("peer presented no certificate".into()))?;
    let leaf = certs
        .first()
        .ok_or_else(|| NetError::Unexpected("empty certificate chain".into()))?;
    Ok(fingerprint_of_cert(leaf)?)
}

/// 32 bytes of TLS exporter keying material used as SPAKE2 channel binding.
pub(crate) fn channel_binding(conn: &quinn::Connection) -> Result<Vec<u8>, NetError> {
    let mut out = vec![0u8; 32];
    conn.export_keying_material(&mut out, b"EXPORTER-tlp-desk-pake-v2", b"")
        .map_err(|_| NetError::Unexpected("TLS exporter unavailable".into()))?;
    Ok(out)
}
