//! Open-receive TLS: require certificate-key possession on both endpoints, but
//! do not assert PKI/device ownership. This permits active LAN impersonation;
//! verified remote enrollment must use a different policy, never this verifier.

use anyhow::Result;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::{
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{sync::Arc, time::Duration};

#[derive(Debug)]
struct OpenReceiverVerifier(Arc<rustls::crypto::CryptoProvider>);

impl OpenReceiverVerifier {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
    fn signature(
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
}

impl ServerCertVerifier for OpenReceiverVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if cert.len() > 4096 || !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "open mode requires one bounded device certificate".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is disabled".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for OpenReceiverVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if cert.len() > 4096 || !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "open mode requires one bounded device certificate".into(),
            ));
        }
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is disabled".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.max_concurrent_bidi_streams(1_u8.into());
    config.max_concurrent_uni_streams(0_u8.into());
    config.stream_receive_window(32_768_u32.into());
    config.receive_window(65_536_u32.into());
    config.send_window(65_536);
    config.datagram_receive_buffer_size(None);
    config.max_idle_timeout(Some(
        Duration::from_secs(5).try_into().expect("fixed timeout"),
    ));
    Arc::new(config)
}

pub fn server(alpn: &[u8]) -> Result<quinn::ServerConfig> {
    let identity = rcgen::generate_simple_self_signed(vec!["janus.local".into()])?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(OpenReceiverVerifier::new())
        .with_single_cert(
            vec![identity.cert.der().clone()],
            PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der()).into(),
        )?;
    config.alpn_protocols = vec![alpn.to_vec()];
    config.max_early_data_size = 0;
    config.send_tls13_tickets = 0;
    let mut server =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(config)?));
    server.transport_config(transport());
    Ok(server)
}

pub fn client(alpn: &[u8]) -> Result<quinn::ClientConfig> {
    let identity = rcgen::generate_simple_self_signed(vec!["janus.local".into()])?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(OpenReceiverVerifier::new())
        .with_client_auth_cert(
            vec![identity.cert.der().clone()],
            PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der()).into(),
        )?;
    config.alpn_protocols = vec![alpn.to_vec()];
    config.enable_early_data = false;
    let mut client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(config)?));
    client.transport_config(transport());
    Ok(client)
}
