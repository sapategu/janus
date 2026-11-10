//! Offline Cast device trust. Verifies the receiver's device certificate path to
//! the embedded Cast anchors, checks revocation (device CRL, else the embedded
//! fallback CRL), then verifies the receiver's RSA signature over
//! `sender_nonce || TLS leaf DER`. Only [`verify`] can mint a [`VerifiedDevice`].
//!
//! Rules follow Open Screen's `BoringSSLTrustStore`/`VerifyDeviceCert` and
//! Chromium's `CRL_REQUIRED_WITH_FALLBACK` policy; provenance is recorded in
//! `trust/README.md`. Everything is local: no account, cloud or SDK call.
use crate::auth::UnverifiedCredentials;
use prost::Message as _;
use ring::{digest, signature};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use x509_parser::{
    extensions::{GeneralName, NameConstraints, ParsedExtension},
    oid_registry::{
        OID_PKCS1_RSAENCRYPTION, OID_PKCS1_SHA1WITHRSA, OID_PKCS1_SHA256WITHRSA,
        OID_PKCS1_SHA384WITHRSA, OID_PKCS1_SHA512WITHRSA, OID_X509_EXT_BASIC_CONSTRAINTS,
        OID_X509_EXT_KEY_USAGE, OID_X509_EXT_NAME_CONSTRAINTS,
    },
    prelude::{FromDer, X509Certificate},
    public_key::PublicKey,
    x509::X509Name,
};

const CAST_ROOT_CA: &[u8] = include_bytes!("../trust/cast_root_ca.der");
const EUREKA_ROOT_CA: &[u8] = include_bytes!("../trust/eureka_root_ca.der");
const CAST_CRL_ROOT_CA: &[u8] = include_bytes!("../trust/cast_crl_root_ca.der");
const FALLBACK_CRL: &[u8] = include_bytes!("../trust/cast_fallback_crl.pb");

/// Start of the embedded fallback CRL window (2026-11-09T00:00:00Z). Chromium
/// uses build time + 20 weeks; refresh `trust/` and this value per release.
pub const FALLBACK_CRL_REFRESHED_UNIX: i64 = 1_794_182_400;
const FALLBACK_CRL_VALIDITY_SECONDS: i64 = 20 * 7 * 24 * 60 * 60;
/// Chromium checks the fallback CRL signer at this fixed published time.
const FALLBACK_CRL_SIGNER_UNIX: i64 = 1_692_255_600;

const MIN_RSA_BITS: usize = 2048;
const MAX_CERT_BYTES: usize = 16_384;
const MAX_INTERMEDIATES: usize = 5;
const MAX_CRL_BUNDLE_BYTES: usize = crate::wire::MAX_FRAME_BYTES;
const MAX_CRLS: usize = 8;
const MAX_REVOKED_ENTRIES: usize = 16_384;
const MAX_PATHLEN: u32 = 0xff;
/// DER content of 1.3.6.1.4.1.11129.2.5.2, Cast's audio-only device policy.
const AUDIO_ONLY_POLICY: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0xD6, 0x79, 0x02, 0x05, 0x02];
/// DER content of 2.5.29.32.0 (anyPolicy).
const ANY_POLICY: &[u8] = &[0x55, 0x1D, 0x20, 0x00];

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    #[error("receiver certificate could not be parsed")]
    Parse,
    #[error("receiver certificate is not valid at the current time")]
    Date,
    #[error("receiver certificate is not a device certificate")]
    Restrictions,
    #[error("receiver certificate path violates a length constraint")]
    Pathlen,
    #[error("receiver certificate is not issued by a trusted Cast authority")]
    Untrusted,
    #[error("receiver certificate was revoked")]
    Revoked,
    #[error("no valid revocation data is available")]
    RevocationUnavailable,
    #[error("receiver signature does not match this connection")]
    Signature,
}

/// Cast's certificate policy. Audio-only devices must never be offered a screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevicePolicy {
    Unrestricted,
    AudioOnly,
}

/// Which signed revocation list approved the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationSource {
    DeviceCrl,
    FallbackCrl,
}

/// Proof that this TLS channel's key was signed by a non-revoked Cast device
/// identity. It is NOT proof of idle state, mirroring support or user consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDevice {
    policy: DevicePolicy,
    revocation: RevocationSource,
}

impl VerifiedDevice {
    pub fn policy(&self) -> DevicePolicy {
        self.policy
    }
    pub fn revocation(&self) -> RevocationSource {
        self.revocation
    }
}

/// Verify credentials with the embedded production anchors at `now`.
pub fn verify(
    credentials: &UnverifiedCredentials,
    now: SystemTime,
) -> Result<VerifiedDevice, Error> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .ok_or(Error::Date)?;
    verify_with(&TrustConfig::production(), credentials, now)
}

pub(crate) struct TrustConfig<'a> {
    device_anchors: Vec<&'a [u8]>,
    crl_anchors: Vec<&'a [u8]>,
    fallback_crl: Option<&'a [u8]>,
    fallback_window: (i64, i64),
    fallback_signer_time: i64,
}

impl TrustConfig<'static> {
    pub(crate) fn production() -> Self {
        Self {
            device_anchors: vec![CAST_ROOT_CA, EUREKA_ROOT_CA],
            crl_anchors: vec![CAST_CRL_ROOT_CA],
            fallback_crl: Some(FALLBACK_CRL),
            fallback_window: (
                FALLBACK_CRL_REFRESHED_UNIX,
                FALLBACK_CRL_REFRESHED_UNIX + FALLBACK_CRL_VALIDITY_SECONDS,
            ),
            fallback_signer_time: FALLBACK_CRL_SIGNER_UNIX,
        }
    }
}

pub(crate) fn verify_with(
    config: &TrustConfig<'_>,
    credentials: &UnverifiedCredentials,
    now: i64,
) -> Result<VerifiedDevice, Error> {
    let crl_anchors = parse_all(&config.crl_anchors)?;
    // An absent or invalid device CRL is not fatal by itself; the fallback
    // decides below, exactly as Chromium's CRL_REQUIRED_WITH_FALLBACK does.
    let device_crl = credentials
        .crl()
        .filter(|crl| !crl.is_empty())
        .and_then(|crl| VerifiedCrl::parse(crl, now, now, &crl_anchors, None));
    let fallback = config.fallback_crl.and_then(|crl| {
        VerifiedCrl::parse(
            crl,
            now,
            config.fallback_signer_time,
            &crl_anchors,
            Some(config.fallback_window),
        )
    });

    let anchors = parse_all(&config.device_anchors)?;
    let intermediates = credentials.intermediates();
    if intermediates.len() > MAX_INTERMEDIATES {
        return Err(Error::Parse);
    }
    let intermediates = parse_all(intermediates)?;
    let target = parse(credentials.certificate())?;
    let path = find_path(&target, &intermediates, &anchors, now, true)?;

    if let Some(fallback) = &fallback {
        if !fallback.permits(&path, now) {
            return Err(Error::Revoked);
        }
    } else if device_crl.is_none() {
        return Err(Error::RevocationUnavailable);
    }
    let revocation = match &device_crl {
        None => RevocationSource::FallbackCrl,
        Some(crl) if crl.permits(&path, now) => RevocationSource::DeviceCrl,
        Some(_) => return Err(Error::Revoked),
    };

    // Auth parsing already required RSASSA-PKCS1-v1_5 with SHA-256.
    if credentials.signature().is_empty()
        || !verify_rsa(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            path[0],
            credentials.signed_input(),
            credentials.signature(),
        )
    {
        return Err(Error::Signature);
    }

    let audio_only = path.iter().any(|cert| has_policy(cert, AUDIO_ONLY_POLICY));
    Ok(VerifiedDevice {
        policy: if audio_only {
            DevicePolicy::AudioOnly
        } else {
            DevicePolicy::Unrestricted
        },
        revocation,
    })
}

fn parse(der: &[u8]) -> Result<X509Certificate<'_>, Error> {
    if der.is_empty() || der.len() > MAX_CERT_BYTES {
        return Err(Error::Parse);
    }
    let (rest, cert) = X509Certificate::from_der(der).map_err(|_| Error::Parse)?;
    // Treat malformed known extensions as a parse failure rather than absent.
    if !rest.is_empty()
        || cert
            .extensions()
            .iter()
            .any(|ext| ext.parsed_extension().error().is_some())
    {
        return Err(Error::Parse);
    }
    Ok(cert)
}

fn parse_all<T: AsRef<[u8]>>(ders: &[T]) -> Result<Vec<X509Certificate<'_>>, Error> {
    ders.iter().map(|der| parse(der.as_ref())).collect()
}

fn valid_at(cert: &X509Certificate<'_>, time: i64) -> bool {
    let validity = cert.validity();
    validity.not_before.timestamp() <= time && time <= validity.not_after.timestamp()
}

fn rsa_bits(cert: &X509Certificate<'_>) -> usize {
    let key = cert.public_key();
    if key.algorithm.algorithm != OID_PKCS1_RSAENCRYPTION {
        return 0;
    }
    let Ok(PublicKey::RSA(rsa)) = key.parsed() else {
        return 0;
    };
    let modulus = rsa.modulus;
    let Some(first) = modulus.iter().position(|byte| *byte != 0) else {
        return 0;
    };
    (modulus.len() - first) * 8 - modulus[first].leading_zeros() as usize
}

fn matching_signature_algorithms(cert: &X509Certificate<'_>) -> bool {
    let outer = &cert.signature_algorithm;
    let inner = &cert.tbs_certificate.signature;
    outer.algorithm == inner.algorithm
        && outer
            .parameters
            .as_ref()
            .map(|any| (any.header.tag(), any.data))
            == inner
                .parameters
                .as_ref()
                .map(|any| (any.header.tag(), any.data))
}

fn verify_rsa(
    algorithm: &'static signature::RsaParameters,
    signer: &X509Certificate<'_>,
    message: &[u8],
    signature_bytes: &[u8],
) -> bool {
    let key = signer.public_key();
    key.algorithm.algorithm == OID_PKCS1_RSAENCRYPTION
        && signature::UnparsedPublicKey::new(algorithm, key.subject_public_key.data.as_ref())
            .verify(message, signature_bytes)
            .is_ok()
}

fn verify_issued(issuer: &X509Certificate<'_>, subject: &X509Certificate<'_>) -> bool {
    let oid = &subject.signature_algorithm.algorithm;
    let algorithm: &'static signature::RsaParameters = if *oid == OID_PKCS1_SHA256WITHRSA {
        &signature::RSA_PKCS1_2048_8192_SHA256
    } else if *oid == OID_PKCS1_SHA384WITHRSA {
        &signature::RSA_PKCS1_2048_8192_SHA384
    } else if *oid == OID_PKCS1_SHA512WITHRSA {
        &signature::RSA_PKCS1_2048_8192_SHA512
    } else if *oid == OID_PKCS1_SHA1WITHRSA {
        // Existing Cast roots/intermediates are SHA-1 signed; Open Screen
        // accepts them for chain links only. Response signatures stay SHA-256.
        &signature::RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY
    } else {
        return false;
    };
    verify_rsa(
        algorithm,
        issuer,
        subject.tbs_certificate.as_ref(),
        subject.signature_value.data.as_ref(),
    )
}

/// Canonical name form for issuer/subject matching: per-RDN sorted attributes,
/// ASCII-lowercased with collapsed whitespace for string values (as BoringSSL's
/// X509_NAME_cmp canonicalization), raw DER otherwise.
type CanonicalName = Vec<Vec<(Vec<u8>, Vec<u8>)>>;

fn canonical(name: &X509Name<'_>) -> CanonicalName {
    name.iter_rdn()
        .map(|rdn| {
            let mut attrs: Vec<_> = rdn
                .iter()
                .map(|attr| {
                    let value = match attr.as_str() {
                        Ok(text) => text
                            .split_ascii_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .to_ascii_lowercase()
                            .into_bytes(),
                        Err(_) => attr.attr_value().data.to_vec(),
                    };
                    (attr.attr_type().as_bytes().to_vec(), value)
                })
                .collect();
            attrs.sort();
            attrs
        })
        .collect()
}

fn self_issued(cert: &X509Certificate<'_>) -> bool {
    canonical(cert.subject()) == canonical(cert.issuer())
}

fn has_policy(cert: &X509Certificate<'_>, policy: &[u8]) -> bool {
    cert.extensions().iter().any(|ext| {
        matches!(ext.parsed_extension(), ParsedExtension::CertificatePolicies(policies)
            if policies.iter().any(|info| info.policy_id.as_bytes() == policy))
    })
}

/// Directory-name constraints only. Other subtree kinds fail closed because
/// this verifier does not evaluate SAN/email forms.
struct DirectoryConstraints {
    permitted: Vec<CanonicalName>,
    excluded: Vec<CanonicalName>,
}

impl DirectoryConstraints {
    fn from(constraints: &NameConstraints<'_>) -> Result<Self, Error> {
        let collect = |subtrees: &Option<Vec<x509_parser::extensions::GeneralSubtree<'_>>>| {
            subtrees
                .iter()
                .flatten()
                .map(|subtree| match &subtree.base {
                    GeneralName::DirectoryName(name) => Ok(canonical(name)),
                    _ => Err(Error::Untrusted),
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            permitted: collect(&constraints.permitted_subtrees)?,
            excluded: collect(&constraints.excluded_subtrees)?,
        })
    }

    fn allows(&self, subject: &X509Certificate<'_>) -> bool {
        let name = canonical(subject.subject());
        let within = |base: &CanonicalName| name.starts_with(base);
        (self.permitted.is_empty() || self.permitted.iter().any(within))
            && !self.excluded.iter().any(within)
    }
}

/// Depth-first issuer search, anchors first. `require_digital_signature`
/// applies the device-certificate leaf rule; CRL signers skip it (Chromium).
fn find_path<'c>(
    target: &'c X509Certificate<'c>,
    intermediates: &'c [X509Certificate<'c>],
    anchors: &'c [X509Certificate<'c>],
    time: i64,
    require_digital_signature: bool,
) -> Result<Vec<&'c X509Certificate<'c>>, Error> {
    if !valid_at(target, time) {
        return Err(Error::Date);
    }
    if rsa_bits(target) < MIN_RSA_BITS || !matching_signature_algorithms(target) {
        return Err(Error::Untrusted);
    }
    if require_digital_signature {
        match target.key_usage() {
            Ok(Some(usage)) if usage.value.digital_signature() => {}
            _ => return Err(Error::Restrictions),
        }
    }
    let mut path = vec![target];
    let mut last_error = None;
    if extend(&mut path, intermediates, anchors, time, &mut last_error) {
        Ok(path)
    } else {
        Err(last_error.unwrap_or(Error::Untrusted))
    }
}

fn extend<'c>(
    path: &mut Vec<&'c X509Certificate<'c>>,
    intermediates: &'c [X509Certificate<'c>],
    anchors: &'c [X509Certificate<'c>],
    time: i64,
    last_error: &mut Option<Error>,
) -> bool {
    let wanted = canonical(path[path.len() - 1].issuer());
    for anchor in anchors {
        if canonical(anchor.subject()) == wanted {
            path.push(anchor);
            match verify_chain(path, time) {
                Ok(()) => return true,
                Err(error) => {
                    *last_error = Some(error);
                    path.pop();
                }
            }
        }
    }
    // Bound: target + every intermediate once + anchor.
    if path.len() > MAX_INTERMEDIATES {
        return false;
    }
    for candidate in intermediates {
        let subject = canonical(candidate.subject());
        if subject == wanted && !path.iter().any(|cert| canonical(cert.subject()) == subject) {
            path.push(candidate);
            if extend(path, intermediates, anchors, time, last_error) {
                return true;
            }
            path.pop();
        }
    }
    false
}

/// `path` is target..anchor. Mirrors Open Screen's VerifyCertificateChain.
fn verify_chain(path: &[&X509Certificate<'_>], time: i64) -> Result<(), Error> {
    let mut max_pathlen = path.len().saturating_sub(2);
    let mut constraints: Vec<DirectoryConstraints> = Vec::new();
    for index in (1..path.len()).rev() {
        let issuer = path[index];
        let subject = path[index - 1];
        let is_root = index == path.len() - 1;
        let issuer_self_issued = if is_root {
            true
        } else {
            if !valid_at(issuer, time) {
                return Err(Error::Date);
            }
            if self_issued(issuer) {
                true
            } else {
                if max_pathlen == 0 {
                    return Err(Error::Pathlen);
                }
                max_pathlen -= 1;
                false
            }
        };

        match issuer.key_usage() {
            Ok(Some(usage)) if !usage.value.key_cert_sign() => return Err(Error::Untrusted),
            Err(_) => return Err(Error::Untrusted),
            _ => {}
        }
        match issuer.basic_constraints() {
            Ok(Some(constraint)) if constraint.value.ca => {
                if let Some(limit) = constraint.value.path_len_constraint {
                    if limit > MAX_PATHLEN {
                        return Err(Error::Untrusted);
                    }
                    max_pathlen = max_pathlen.min(limit as usize);
                }
            }
            _ => return Err(Error::Untrusted),
        }
        if !matching_signature_algorithms(issuer) || rsa_bits(issuer) < MIN_RSA_BITS {
            return Err(Error::Untrusted);
        }
        if !issuer_self_issued && !constraints.iter().all(|nc| nc.allows(subject)) {
            return Err(Error::Untrusted);
        }
        match issuer.name_constraints() {
            Ok(Some(nc)) => constraints.push(DirectoryConstraints::from(nc.value)?),
            Ok(None) => {}
            Err(_) => return Err(Error::Untrusted),
        }
        match issuer.policy_mappings() {
            Ok(Some(mappings))
                if mappings.value.mappings.iter().any(|mapping| {
                    mapping.issuer_domain_policy.as_bytes() == ANY_POLICY
                        || mapping.subject_domain_policy.as_bytes() == ANY_POLICY
                }) =>
            {
                return Err(Error::Untrusted);
            }
            Err(_) => return Err(Error::Untrusted),
            _ => {}
        }
        let handled = [
            &OID_X509_EXT_NAME_CONSTRAINTS,
            &OID_X509_EXT_BASIC_CONSTRAINTS,
            &OID_X509_EXT_KEY_USAGE,
        ];
        if issuer
            .extensions()
            .iter()
            .any(|ext| ext.critical && !handled.contains(&&ext.oid))
        {
            return Err(Error::Untrusted);
        }
        if !verify_issued(issuer, subject) {
            return Err(Error::Untrusted);
        }
    }
    if constraints.iter().all(|nc| nc.allows(path[0])) {
        Ok(())
    } else {
        Err(Error::Untrusted)
    }
}

// Open Screen revocation.proto field assignments.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CrlBundle {
    #[prost(message, repeated, tag = "1")]
    pub crls: Vec<Crl>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Crl {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub tbs_crl: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signer_cert: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub signature: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct TbsCrl {
    #[prost(uint64, optional, tag = "1")]
    version: Option<u64>,
    #[prost(uint64, optional, tag = "2")]
    not_before_seconds: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    not_after_seconds: Option<u64>,
    #[prost(bytes = "vec", repeated, tag = "4")]
    revoked_public_key_hashes: Vec<Vec<u8>>,
    #[prost(message, repeated, tag = "5")]
    revoked_serial_number_ranges: Vec<SerialNumberRange>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct SerialNumberRange {
    #[prost(bytes = "vec", optional, tag = "1")]
    issuer_public_key_hash: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "2")]
    first_serial_number: Option<u64>,
    #[prost(uint64, optional, tag = "3")]
    last_serial_number: Option<u64>,
}

pub(crate) struct VerifiedCrl {
    not_before: i64,
    not_after: i64,
    revoked_keys: HashSet<Vec<u8>>,
    revoked_serials: HashMap<Vec<u8>, Vec<(u64, u64)>>,
}

impl VerifiedCrl {
    /// Returns the first version-0 CRL in the bundle if it verifies. As in
    /// Open Screen, a failing version-0 CRL rejects the whole bundle.
    /// `fallback_window` replaces the signed validity range (Chromium's
    /// build-time window for the embedded fallback CRL).
    pub(crate) fn parse(
        bundle: &[u8],
        now: i64,
        signer_time: i64,
        anchors: &[X509Certificate<'_>],
        fallback_window: Option<(i64, i64)>,
    ) -> Option<Self> {
        if bundle.len() > MAX_CRL_BUNDLE_BYTES {
            return None;
        }
        let bundle = CrlBundle::decode(bundle).ok()?;
        for crl in bundle.crls.iter().take(MAX_CRLS) {
            let Some(tbs_bytes) = &crl.tbs_crl else {
                continue;
            };
            let Ok(tbs) = TbsCrl::decode(tbs_bytes.as_slice()) else {
                continue;
            };
            if tbs.version.unwrap_or(0) != 0 {
                continue;
            }
            return Self::verify(
                crl,
                tbs_bytes,
                tbs,
                now,
                signer_time,
                anchors,
                fallback_window,
            );
        }
        None
    }

    fn verify(
        crl: &Crl,
        tbs_bytes: &[u8],
        tbs: TbsCrl,
        now: i64,
        signer_time: i64,
        anchors: &[X509Certificate<'_>],
        fallback_window: Option<(i64, i64)>,
    ) -> Option<Self> {
        let signer = parse(crl.signer_cert.as_deref()?).ok()?;
        let path = find_path(&signer, &[], anchors, signer_time, false).ok()?;
        if !verify_rsa(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            path[0],
            tbs_bytes,
            crl.signature.as_deref()?,
        ) {
            return None;
        }
        let seconds = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
        let (not_before, not_after) = match fallback_window {
            Some(window) => window,
            None => (
                seconds(tbs.not_before_seconds)?,
                seconds(tbs.not_after_seconds)?,
            ),
        };
        if now < not_before || not_after < now {
            return None;
        }
        // A device CRL expires with its signer. The embedded fallback does
        // not: its signer is checked at a fixed past time and its window is
        // Janus's own (Chromium `if (!is_fallback_crl)` in VerifyCRL).
        let not_after = match fallback_window {
            Some(_) => not_after,
            None => not_after.min(signer.validity().not_after.timestamp()),
        };
        if tbs.revoked_public_key_hashes.len() + tbs.revoked_serial_number_ranges.len()
            > MAX_REVOKED_ENTRIES
        {
            return None;
        }
        let mut revoked_serials: HashMap<Vec<u8>, Vec<(u64, u64)>> = HashMap::new();
        for range in tbs.revoked_serial_number_ranges {
            let first = range.first_serial_number.unwrap_or(0);
            let last = range.last_serial_number.unwrap_or(0);
            if last < first {
                return None;
            }
            revoked_serials
                .entry(range.issuer_public_key_hash.unwrap_or_default())
                .or_default()
                .push((first, last));
        }
        Some(Self {
            not_before,
            not_after,
            revoked_keys: tbs.revoked_public_key_hashes.into_iter().collect(),
            revoked_serials,
        })
    }

    /// Open Screen CheckRevocation: walk anchor → target, rejecting revoked
    /// SPKI hashes and subordinate serials revoked under the issuer's SPKI.
    pub(crate) fn permits(&self, path: &[&X509Certificate<'_>], now: i64) -> bool {
        if path.is_empty() || now < self.not_before || self.not_after < now {
            return false;
        }
        for index in (0..path.len()).rev() {
            let hash = digest::digest(&digest::SHA256, path[index].public_key().raw);
            if self.revoked_keys.contains(hash.as_ref()) {
                return false;
            }
            if index == 0 {
                continue;
            }
            let Some(ranges) = self.revoked_serials.get(hash.as_ref()) else {
                continue;
            };
            // Only positive serials that fit in 64 bits can be revoked by range.
            let Some(serial) = serial_u64(path[index - 1].raw_serial()) else {
                continue;
            };
            if ranges
                .iter()
                .any(|(first, last)| (*first..=*last).contains(&serial))
            {
                return false;
            }
        }
        true
    }
}

fn serial_u64(raw: &[u8]) -> Option<u64> {
    if raw.first().is_some_and(|byte| byte & 0x80 != 0) {
        return None;
    }
    let start = raw.iter().position(|byte| *byte != 0).unwrap_or(raw.len());
    let digits = &raw[start..];
    (digits.len() <= 8).then(|| {
        digits
            .iter()
            .fold(0, |acc, byte| (acc << 8) | u64::from(*byte))
    })
}

/// Test-vector access for `tests/test_trust.rs`. Returns plain facts, never a
/// [`VerifiedDevice`], so custom anchors cannot mint production trust. Not a
/// production API; production code calls [`verify`].
#[doc(hidden)]
pub mod testing {
    use super::*;

    pub const CAST_ROOT_CA: &[u8] = super::CAST_ROOT_CA;
    pub const EUREKA_ROOT_CA: &[u8] = super::EUREKA_ROOT_CA;
    pub const CAST_CRL_ROOT_CA: &[u8] = super::CAST_CRL_ROOT_CA;
    pub const FALLBACK_CRL: &[u8] = super::FALLBACK_CRL;
    pub const FALLBACK_CRL_SIGNER_UNIX: i64 = super::FALLBACK_CRL_SIGNER_UNIX;
    pub const FALLBACK_CRL_VALIDITY_SECONDS: i64 = super::FALLBACK_CRL_VALIDITY_SECONDS;

    /// RSA >= 2048, CA basic constraint and self-issued.
    pub fn is_rsa_root_ca(der: &[u8]) -> bool {
        parse(der).is_ok_and(|cert| {
            rsa_bits(&cert) >= MIN_RSA_BITS
                && self_issued(&cert)
                && cert
                    .basic_constraints()
                    .is_ok_and(|bc| bc.is_some_and(|bc| bc.value.ca))
        })
    }

    /// Device path only (Open Screen VerifyDeviceCert, CRL optional).
    /// Returns the device common name and Cast policy.
    pub fn check_path(
        chain: &[Vec<u8>],
        anchors: &[Vec<u8>],
        time: i64,
    ) -> Result<(String, DevicePolicy), Error> {
        let anchors = parse_all(anchors)?;
        let target = parse(chain.first().ok_or(Error::Parse)?)?;
        let intermediates = parse_all(&chain[1..])?;
        let path = find_path(&target, &intermediates, &anchors, time, true)?;
        let name = path[0]
            .subject()
            .iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok())
            .unwrap_or_default()
            .to_owned();
        let audio = path.iter().any(|cert| has_policy(cert, AUDIO_ONLY_POLICY));
        Ok((
            name,
            if audio {
                DevicePolicy::AudioOnly
            } else {
                DevicePolicy::Unrestricted
            },
        ))
    }

    /// RSASSA-PKCS1-v1_5 SHA-256 signature by `certificate`'s key.
    pub fn signature_matches(certificate: &[u8], message: &[u8], signature_bytes: &[u8]) -> bool {
        parse(certificate).is_ok_and(|cert| {
            verify_rsa(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                &cert,
                message,
                signature_bytes,
            )
        })
    }

    /// Whether `bundle` verifies against `crl_anchors` at `now`.
    pub fn crl_is_valid(
        bundle: &[u8],
        now: i64,
        signer_time: i64,
        crl_anchors: &[Vec<u8>],
        fallback_window: Option<(i64, i64)>,
    ) -> bool {
        parse_all(crl_anchors).is_ok_and(|anchors| {
            VerifiedCrl::parse(bundle, now, signer_time, &anchors, fallback_window).is_some()
        })
    }

    /// Whether the embedded fallback CRL, in its production window at
    /// `crl_time`, permits `chain` (path verified at `cert_time`).
    pub fn fallback_crl_permits(
        chain: &[Vec<u8>],
        device_anchors: &[Vec<u8>],
        cert_time: i64,
        crl_time: i64,
    ) -> Result<bool, Error> {
        let config = TrustConfig::production();
        let crl_anchors = parse_all(&config.crl_anchors)?;
        let device_anchors = parse_all(device_anchors)?;
        let target = parse(chain.first().ok_or(Error::Parse)?)?;
        let intermediates = parse_all(&chain[1..])?;
        let path = find_path(&target, &intermediates, &device_anchors, cert_time, true)?;
        let crl = VerifiedCrl::parse(
            FALLBACK_CRL,
            crl_time,
            config.fallback_signer_time,
            &crl_anchors,
            Some(config.fallback_window),
        )
        .ok_or(Error::RevocationUnavailable)?;
        Ok(crl.permits(&path, crl_time))
    }

    /// `None` when the CRL is invalid at `crl_time`; otherwise whether it
    /// permits the device path (which must verify at `cert_time`).
    pub fn crl_permits(
        bundle: &[u8],
        crl_time: i64,
        crl_anchors: &[Vec<u8>],
        chain: &[Vec<u8>],
        device_anchors: &[Vec<u8>],
        cert_time: i64,
    ) -> Result<Option<bool>, Error> {
        let crl_anchors = parse_all(crl_anchors)?;
        let device_anchors = parse_all(device_anchors)?;
        let target = parse(chain.first().ok_or(Error::Parse)?)?;
        let intermediates = parse_all(&chain[1..])?;
        let path = find_path(&target, &intermediates, &device_anchors, cert_time, true)?;
        Ok(
            VerifiedCrl::parse(bundle, crl_time, crl_time, &crl_anchors, None)
                .map(|crl| crl.permits(&path, cert_time)),
        )
    }

    /// Full production verification (embedded anchors) with an overridden
    /// fallback window, or no fallback CRL when `fallback_window` is `None`.
    pub fn verify_parts(
        fallback_window: Option<(i64, i64)>,
        signed_input: Vec<u8>,
        signature_bytes: Vec<u8>,
        certificate: Vec<u8>,
        intermediates: Vec<Vec<u8>>,
        crl: Option<Vec<u8>>,
        now: i64,
    ) -> Result<(DevicePolicy, RevocationSource), Error> {
        let mut config = TrustConfig::production();
        match fallback_window {
            Some(window) => config.fallback_window = window,
            None => config.fallback_crl = None,
        }
        let credentials = UnverifiedCredentials::from_parts(
            signed_input,
            signature_bytes,
            certificate,
            intermediates,
            crl,
        );
        verify_with(&config, &credentials, now).map(|device| (device.policy, device.revocation))
    }
}
