//! Native receiver channel: TCP/TLS, device-auth challenge and offline trust.
//! `probe_authentication` checks and closes. `connect_verified` (crate-only)
//! hands a verified channel to `session`; nothing else can obtain one.
use crate::{auth, trust, wire};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::{net::SocketAddr, sync::Arc, time::Instant};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::{Duration, timeout},
};
use tokio_rustls::TlsConnector;
use x509_parser::prelude::{FromDer, X509Certificate};

const MAX_ENDPOINTS: usize = 8;
#[doc(hidden)]
pub const MAX_CERT_BYTES: usize = 16_384;
const MAX_REMAINING_CERT_SECONDS: i64 = 4 * 24 * 60 * 60;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const AUTH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum Error {
    #[error("no usable receiver address")]
    Address,
    #[error("receiver connection timed out")]
    Timeout,
    #[error("network access denied")]
    NetworkDenied,
    #[error("receiver is unreachable")]
    Connect,
    #[error("receiver TLS certificate or handshake rejected")]
    Tls,
    #[error("receiver authentication response rejected")]
    Authentication(#[from] auth::Error),
    #[error("receiver channel framing rejected")]
    Wire(#[from] wire::Error),
    #[error("too many unrelated receiver messages")]
    MessageLimit,
    #[error("receiver device identity rejected: {0}")]
    Untrusted(#[from] trust::Error),
}

impl Error {
    /// Transient network/framing failures may be retried. Address, permission,
    /// TLS, authentication and trust failures are final for this receiver.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Connect | Self::Wire(_) | Self::MessageLimit
        )
    }
}

pub(crate) type Channel = tokio_rustls::client::TlsStream<TcpStream>;

/// Authenticates the receiver's device identity for this TLS channel, then
/// closes it. Returns trust/policy facts only, never a session/transport handle.
pub async fn probe_authentication(
    endpoints: &[SocketAddr],
) -> Result<trust::VerifiedDevice, Error> {
    let (channel, credentials) = open_channel(endpoints).await?;
    drop(channel);
    Ok(trust::verify(&credentials, std::time::SystemTime::now())?)
}

/// A channel whose peer proved a trusted, non-revoked Cast device identity
/// bound to this exact TLS connection.
pub(crate) async fn connect_verified(
    endpoints: &[SocketAddr],
) -> Result<(Channel, trust::VerifiedDevice), Error> {
    let (channel, credentials) = open_channel(endpoints).await?;
    let device = trust::verify(&credentials, std::time::SystemTime::now())?;
    Ok((channel, device))
}

async fn open_channel(
    endpoints: &[SocketAddr],
) -> Result<(Channel, auth::UnverifiedCredentials), Error> {
    if endpoints.is_empty() || endpoints.len() > MAX_ENDPOINTS || endpoints.iter().any(|address| {
        address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast()
            || address.ip().is_loopback()
            || matches!(address, SocketAddr::V4(v4) if v4.ip().is_broadcast())
            || matches!(address, SocketAddr::V6(v6) if v6.ip().is_unicast_link_local() && v6.scope_id() == 0)
    }) {
        return Err(Error::Address);
    }
    timeout(PROBE_TIMEOUT, async {
        for address in endpoints {
            match timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => {
                    stream.set_nodelay(true).map_err(|_| Error::Connect)?;
                    return authenticate(stream, address.ip().into()).await;
                }
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Err(Error::NetworkDenied);
                }
                _ => continue,
            }
        }
        Err(Error::Connect)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// Authentication exchange over any stream (tests use in-memory duplex). The
/// channel is closed on return; credentials stay unverified.
#[doc(hidden)]
pub async fn probe_stream<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    name: ServerName<'static>,
) -> Result<auth::UnverifiedCredentials, Error> {
    authenticate(stream, name)
        .await
        .map(|(_closed, credentials)| credentials)
}

async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    name: ServerName<'static>,
) -> Result<
    (
        tokio_rustls::client::TlsStream<S>,
        auth::UnverifiedCredentials,
    ),
    Error,
> {
    let config = tls_config()?;
    let mut channel = timeout(
        Duration::from_secs(3),
        TlsConnector::from(Arc::new(config)).connect(name, stream),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(|_| Error::Tls)?;
    let certificate = channel
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or(Error::Tls)?;
    let challenge = auth::Challenge::new(certificate.as_ref(), Instant::now())?;
    timeout(AUTH_TIMEOUT, async {
        wire::write(&mut channel, &challenge.message()?).await?;
        // Receivers may send unsolicited messages before their auth reply. None
        // are acted on; bound both the count and total exchange time.
        for _ in 0..8 {
            let message = wire::read(&mut channel).await?;
            if message.namespace == wire::AUTH_NAMESPACE {
                return Ok(challenge.parse_response(&message, Instant::now())?);
            }
        }
        Err(Error::MessageLimit)
    })
    .await
    .map_err(|_| Error::Timeout)?
    .map(|credentials| (channel, credentials))
}

/// Cast uses a temporary TLS key, whose certificate must be signed by the
/// separately trusted device identity in AuthResponse. This verifier enforces
/// bounds/dates and real TLS key possession, NOT device identity or PKI trust.
/// Keep private: it may only back the authentication-only probe above.
#[derive(Debug)]
struct PendingDeviceVerifier(Arc<rustls::crypto::CryptoProvider>);

#[doc(hidden)]
pub fn validate_tls_leaf(cert: &[u8], now: UnixTime) -> Result<(), rustls::Error> {
    let invalid = || rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding);
    if cert.is_empty() || cert.len() > MAX_CERT_BYTES {
        return Err(invalid());
    }
    let (trailing, certificate) = X509Certificate::from_der(cert).map_err(|_| invalid())?;
    if !trailing.is_empty() {
        return Err(invalid());
    }
    let now = i64::try_from(now.as_secs()).map_err(|_| invalid())?;
    let validity = certificate.validity();
    let start = validity.not_before.timestamp();
    let end = validity.not_after.timestamp();
    if start > now
        || end <= now
        || end < start
        || end.saturating_sub(now) > MAX_REMAINING_CERT_SECONDS
    {
        return Err(rustls::Error::General(
            "invalid temporary Cast TLS certificate dates".into(),
        ));
    }
    Ok(())
}

impl ServerCertVerifier for PendingDeviceVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if intermediates.len() > 5 || intermediates.iter().any(|cert| cert.len() > MAX_CERT_BYTES) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::BadEncoding,
            ));
        }
        validate_tls_leaf(cert.as_ref(), now)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config() -> Result<rustls::ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|_| Error::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PendingDeviceVerifier(provider)))
        .with_no_client_auth();
    config.enable_sni = false;
    config.enable_early_data = false;
    config.resumption = rustls::client::Resumption::disabled();
    Ok(config)
}
