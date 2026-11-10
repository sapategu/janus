//! Fresh challenge and TLS-certificate transcript binding. Parsing credentials
//! is NOT authenticating them; `crate::trust::verify` performs path, revocation
//! and signature checks. No trusted token is minted here.
use crate::wire::{self, AUTH_NAMESPACE, Message, Payload, RECEIVER, SENDER};
use bytes::Bytes;
use prost::Message as _;
use std::time::{Duration, Instant};
use thiserror::Error;

pub const NONCE_BYTES: usize = 16;
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CERTIFICATE_BYTES: usize = 16_384;
pub const MAX_INTERMEDIATES: usize = 5;
const MAX_SIGNATURE_BYTES: usize = 1024;

/// Open Screen DeviceAuthMessage schema (protocol data, not trust state).
#[doc(hidden)]
#[derive(Clone, PartialEq, prost::Message)]
pub struct DeviceAuth {
    #[prost(message, optional, tag = "1")]
    pub challenge: Option<AuthChallenge>,
    #[prost(message, optional, tag = "2")]
    pub response: Option<AuthResponse>,
    #[prost(message, optional, tag = "3")]
    pub error: Option<AuthError>,
}

/// Open Screen DeviceAuthMessage schema (protocol data, not trust state).
#[doc(hidden)]
#[derive(Clone, PartialEq, prost::Message)]
pub struct AuthChallenge {
    #[prost(int32, optional, tag = "1")]
    pub signature_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub sender_nonce: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "3")]
    pub hash_algorithm: Option<i32>,
}

/// Open Screen DeviceAuthMessage schema (protocol data, not trust state).
#[doc(hidden)]
#[derive(Clone, PartialEq, prost::Message)]
pub struct AuthResponse {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub signature: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub certificate: Option<Vec<u8>>,
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub intermediates: Vec<Vec<u8>>,
    #[prost(int32, optional, tag = "4")]
    pub signature_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub sender_nonce: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "6")]
    pub hash_algorithm: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub crl: Option<Vec<u8>>,
}

/// Open Screen DeviceAuthMessage schema (protocol data, not trust state).
#[doc(hidden)]
#[derive(Clone, PartialEq, prost::Message)]
pub struct AuthError {
    #[prost(int32, optional, tag = "1")]
    pub kind: Option<i32>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("device authentication random source unavailable")]
    Random,
    #[error("device authentication deadline expired")]
    Expired,
    #[error("invalid device authentication message")]
    Invalid,
    #[error("device authentication nonce mismatch")]
    Nonce,
    #[error("device authentication algorithm unsupported")]
    Algorithm,
    #[error("receiver rejected device authentication")]
    Rejected,
    #[error("invalid device authentication protobuf")]
    Protobuf(#[from] prost::DecodeError),
    #[error("invalid device authentication framing")]
    Wire(#[from] wire::Error),
}

/// Owned by one pending connection; consumed when parsing its response.
pub struct Challenge {
    nonce: [u8; NONCE_BYTES],
    tls_certificate: Vec<u8>,
    expires: Instant,
}

/// The name is intentional: these are only bounded, transcript-bound inputs to
/// `crate::trust::verify`. There is no authenticated=true
/// flag or API that permits callers to promote this to trusted credentials.
pub struct UnverifiedCredentials {
    signed_input: Vec<u8>,
    signature: Vec<u8>,
    certificate: Vec<u8>,
    intermediates: Vec<Vec<u8>>,
    crl: Option<Vec<u8>>,
}

impl std::fmt::Debug for UnverifiedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnverifiedCredentials")
            .field("credentials", &"[redacted]")
            .finish()
    }
}

impl UnverifiedCredentials {
    pub fn signed_input(&self) -> &[u8] {
        &self.signed_input
    }
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    pub fn intermediates(&self) -> &[Vec<u8>] {
        &self.intermediates
    }
    pub fn crl(&self) -> Option<&[u8]> {
        self.crl.as_deref()
    }

    /// For `trust::testing` vectors only: real device signatures over known
    /// data. Production values come only from `parse_response`.
    pub(crate) fn from_parts(
        signed_input: Vec<u8>,
        signature: Vec<u8>,
        certificate: Vec<u8>,
        intermediates: Vec<Vec<u8>>,
        crl: Option<Vec<u8>>,
    ) -> Self {
        Self {
            signed_input,
            signature,
            certificate,
            intermediates,
            crl,
        }
    }
}

impl Challenge {
    /// `tls_certificate` must be the DER leaf read from this actual TLS channel,
    /// never a discovery field or a certificate supplied by another connection.
    pub fn new(tls_certificate: &[u8], now: Instant) -> Result<Self, Error> {
        if tls_certificate.is_empty() || tls_certificate.len() > MAX_CERTIFICATE_BYTES {
            return Err(Error::Invalid);
        }
        let mut nonce = [0; NONCE_BYTES];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut nonce)
            .map_err(|_| Error::Random)?;
        Ok(Self {
            nonce,
            tls_certificate: tls_certificate.to_vec(),
            expires: now + AUTH_TIMEOUT,
        })
    }

    /// The fresh nonce this challenge sends; public protocol data.
    #[doc(hidden)]
    pub fn nonce(&self) -> [u8; NONCE_BYTES] {
        self.nonce
    }

    pub fn message(&self) -> Result<Message, Error> {
        let auth = DeviceAuth {
            challenge: Some(AuthChallenge {
                signature_algorithm: Some(1),
                sender_nonce: Some(self.nonce.to_vec()),
                hash_algorithm: Some(1),
            }),
            response: None,
            error: None,
        };
        Ok(Message::new(
            SENDER,
            RECEIVER,
            AUTH_NAMESPACE,
            Payload::Binary(Bytes::from(auth.encode_to_vec())),
        )?)
    }

    pub fn parse_response(
        self,
        message: &Message,
        now: Instant,
    ) -> Result<UnverifiedCredentials, Error> {
        if now >= self.expires {
            return Err(Error::Expired);
        }
        if message.source != RECEIVER
            || message.destination != SENDER
            || message.namespace != AUTH_NAMESPACE
        {
            return Err(Error::Invalid);
        }
        let Payload::Binary(bytes) = &message.payload else {
            return Err(Error::Invalid);
        };
        if bytes.is_empty() || bytes.len() > wire::MAX_FRAME_BYTES {
            return Err(Error::Invalid);
        }
        let auth = DeviceAuth::decode(bytes.as_ref())?;
        if auth.error.is_some() {
            return Err(Error::Rejected);
        }
        if auth.challenge.is_some() {
            return Err(Error::Invalid);
        }
        let response = auth.response.ok_or(Error::Invalid)?;
        // SHA-1 is the proto2 default; do not silently downgrade from SHA-256.
        if response.signature_algorithm.unwrap_or(1) != 1 || response.hash_algorithm != Some(1) {
            return Err(Error::Algorithm);
        }
        if response.sender_nonce.as_deref() != Some(&self.nonce) {
            return Err(Error::Nonce);
        }
        let signature = response.signature.ok_or(Error::Invalid)?;
        let certificate = response.certificate.ok_or(Error::Invalid)?;
        let valid_cert = |cert: &[u8]| !cert.is_empty() && cert.len() <= MAX_CERTIFICATE_BYTES;
        if signature.is_empty()
            || signature.len() > MAX_SIGNATURE_BYTES
            || !valid_cert(&certificate)
            || response.intermediates.len() > MAX_INTERMEDIATES
            || response.intermediates.iter().any(|cert| !valid_cert(cert))
        {
            return Err(Error::Invalid);
        }
        let mut signed_input = self.nonce.to_vec();
        signed_input.extend_from_slice(&self.tls_certificate);
        Ok(UnverifiedCredentials {
            signed_input,
            signature,
            certificate,
            intermediates: response.intermediates,
            crl: response.crl,
        })
    }
}
