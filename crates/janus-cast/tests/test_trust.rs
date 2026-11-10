//! Offline Cast device trust against real public vectors (provenance in
//! trust/README.md). Expected results mirror Open Screen's
//! cast_cert_validator_unittest.cc and cast_crl_unittest.cc.
use janus_cast::trust::{
    DevicePolicy, Error, FALLBACK_CRL_REFRESHED_UNIX, RevocationSource, testing,
};
use prost::Message as _;
use x509_parser::pem::Pem;

const APRIL_2016: i64 = 1_459_468_800;
const JANUARY_2015: i64 = 1_420_070_400;
const MARCH_2037: i64 = 2_119_478_400;
const APRIL_2020: i64 = 1_585_699_200;
const DAY: i64 = 86_400;

macro_rules! data {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/", $name)).as_slice()
    };
}

fn blocks(data: &[u8], label: &str) -> Vec<Vec<u8>> {
    Pem::iter_from_buffer(data)
        .map(|block| block.expect("valid PEM"))
        .filter(|block| block.label == label)
        .map(|block| block.contents)
        .collect()
}

fn certs(data: &[u8]) -> Vec<Vec<u8>> {
    blocks(data, "CERTIFICATE")
}

fn production_anchors() -> Vec<Vec<u8>> {
    vec![
        testing::CAST_ROOT_CA.to_vec(),
        testing::EUREKA_ROOT_CA.to_vec(),
    ]
}

/// Builtin anchors, or the file's last certificate as the sole anchor.
fn check(file: &[u8], time: i64, root_in_file: bool) -> Result<(String, DevicePolicy), Error> {
    let mut chain = certs(file);
    let anchors = if root_in_file {
        vec![chain.pop().unwrap()]
    } else {
        production_anchors()
    };
    testing::check_path(&chain, &anchors, time)
}

#[test]
fn test_embedded_anchors_are_rsa_root_cas() {
    for der in [
        testing::CAST_ROOT_CA,
        testing::EUREKA_ROOT_CA,
        testing::CAST_CRL_ROOT_CA,
    ] {
        assert!(testing::is_rsa_root_ca(der));
    }
}

#[test]
fn test_real_cast_devices_chain_to_embedded_anchors() {
    use DevicePolicy::*;
    for (file, name, policy) in [
        (
            data!("chromecast_gen1.pem"),
            "2ZZBG9 FA8FCA3EF91A",
            Unrestricted,
        ),
        (
            data!("chromecast_gen2.pem"),
            "3ZZAK6 FA8FCA3F0D35",
            Unrestricted,
        ),
        (data!("fugu.pem"), "-6394818897508095075", Unrestricted),
        (data!("vizio.pem"), "9V0000VB FA8FCA784D01", Unrestricted),
        (
            data!("chromecast_audio.pem"),
            "4ZZDZJ FA8FCA7EFE3C",
            AudioOnly,
        ),
    ] {
        assert_eq!(
            check(file, APRIL_2016, false),
            Ok((name.to_owned(), policy)),
            "{name}"
        );
    }
}

#[test]
fn test_untrusted_non_device_and_out_of_date_paths_fail() {
    assert_eq!(
        check(data!("unchained.pem"), APRIL_2016, false),
        Err(Error::Untrusted)
    );
    // A trust anchor is not a device certificate.
    assert_eq!(
        testing::check_path(
            &[testing::CAST_ROOT_CA.to_vec()],
            &production_anchors(),
            APRIL_2016
        ),
        Err(Error::Restrictions)
    );
    for time in [JANUARY_2015, MARCH_2037] {
        assert_eq!(
            check(data!("chromecast_gen2.pem"), time, false),
            Err(Error::Date)
        );
    }
}

#[test]
fn test_audio_only_policy_anywhere_in_path_restricts_device() {
    use DevicePolicy::*;
    for (file, policy) in [
        (data!("policies_ica_audioonly_leaf_none.pem"), AudioOnly),
        (data!("policies_ica_none_leaf_audioonly.pem"), AudioOnly),
        (data!("policies_ica_none_leaf_none.pem"), Unrestricted),
        (data!("policies_ica_anypolicy_leaf_foo.pem"), Unrestricted),
    ] {
        assert_eq!(check(file, APRIL_2016, true).map(|r| r.1), Ok(policy));
    }
}

#[test]
fn test_name_and_pathlen_constraints_are_enforced() {
    assert!(check(data!("nc.pem"), APRIL_2020, true).is_ok());
    assert_eq!(
        check(data!("nc_fail.pem"), APRIL_2020, true),
        Err(Error::Untrusted)
    );
    assert_eq!(
        check(
            data!("violates_root_pathlen_constraint.pem"),
            APRIL_2016,
            true
        ),
        Err(Error::Pathlen)
    );
}

#[test]
fn test_real_device_signature_verifies_only_exact_data() {
    let device = certs(data!("chromecast_gen1.pem")).remove(0);
    let file = data!("2ZZBG9_FA8FCA3EF91A.pem");
    let message = &blocks(file, "MESSAGE")[0];
    let sha256 = &blocks(file, "SIGNATURE SHA256")[0];
    let sha1 = &blocks(file, "SIGNATURE SHA1")[0];
    assert!(testing::signature_matches(&device, message, sha256));
    assert!(!testing::signature_matches(&device, b"STRINH", sha256));
    assert!(!testing::signature_matches(&device, message, sha1));
    let mut tampered = sha256.clone();
    tampered[10] ^= 1;
    assert!(!testing::signature_matches(&device, message, &tampered));
}

#[test]
fn test_embedded_fallback_crl_is_signed_by_cast_crl_root_within_its_window() {
    let start = FALLBACK_CRL_REFRESHED_UNIX;
    let end = start + testing::FALLBACK_CRL_VALIDITY_SECONDS;
    let valid_at = |now, anchor: &[u8]| {
        testing::crl_is_valid(
            testing::FALLBACK_CRL,
            now,
            testing::FALLBACK_CRL_SIGNER_UNIX,
            &[anchor.to_vec()],
            Some((start, end)),
        )
    };
    let crl_root = testing::CAST_CRL_ROOT_CA;
    assert!(valid_at(start, crl_root));
    assert!(valid_at(end, crl_root));
    assert!(!valid_at(start - 1, crl_root));
    assert!(!valid_at(end + 1, crl_root));
    // Signed by the CRL root, not by a device root.
    assert!(!valid_at(start, testing::CAST_ROOT_CA));
}

/// Regression: the fallback signer expired in 2023, yet the fallback must
/// still permit unrevoked devices throughout its production window. A real
/// AzubaTV check was wrongly reported as revoked before this was fixed.
#[test]
fn test_fallback_crl_permits_unrevoked_devices_across_production_window() {
    let start = FALLBACK_CRL_REFRESHED_UNIX;
    let end = start + testing::FALLBACK_CRL_VALIDITY_SECONDS;
    for file in [
        data!("chromecast_gen1.pem"),
        data!("chromecast_gen2.pem"),
        data!("fugu.pem"),
        data!("vizio.pem"),
    ] {
        for crl_time in [start, start + DAY, end] {
            assert_eq!(
                testing::fallback_crl_permits(
                    &certs(file),
                    &production_anchors(),
                    APRIL_2016,
                    crl_time
                ),
                Ok(true)
            );
        }
        assert_eq!(
            testing::fallback_crl_permits(&certs(file), &production_anchors(), APRIL_2016, end + 1),
            Err(Error::RevocationUnavailable)
        );
    }
}

fn gen1(
    signature: Vec<u8>,
    crl: Option<Vec<u8>>,
    fallback: bool,
    now: i64,
) -> Result<(DevicePolicy, RevocationSource), Error> {
    let mut chain = certs(data!("chromecast_gen1.pem"));
    let device = chain.remove(0);
    let message = blocks(data!("2ZZBG9_FA8FCA3EF91A.pem"), "MESSAGE").remove(0);
    let window = fallback.then_some((APRIL_2016 - DAY, APRIL_2016 + DAY));
    testing::verify_parts(window, message, signature, device, chain, crl, now)
}

#[test]
fn test_full_verification_accepts_real_device_and_fails_closed() {
    let signature = blocks(data!("2ZZBG9_FA8FCA3EF91A.pem"), "SIGNATURE SHA256").remove(0);
    assert_eq!(
        gen1(signature.clone(), None, true, APRIL_2016),
        Ok((DevicePolicy::Unrestricted, RevocationSource::FallbackCrl))
    );
    let mut wrong = signature.clone();
    wrong[0] ^= 0x80;
    assert_eq!(gen1(wrong, None, true, APRIL_2016), Err(Error::Signature));
    // No device CRL and no fallback: revocation cannot be checked.
    for crl in [None, Some(b"not a crl bundle".to_vec())] {
        assert_eq!(
            gen1(signature.clone(), crl, false, APRIL_2016),
            Err(Error::RevocationUnavailable)
        );
    }
    // An expired fallback window fails closed too.
    assert_eq!(
        gen1(signature, None, true, APRIL_2016 + 2 * DAY),
        Err(Error::RevocationUnavailable)
    );
}

#[derive(Clone, PartialEq, prost::Message)]
struct DeviceCertTestSuite {
    #[prost(message, repeated, tag = "1")]
    tests: Vec<DeviceCertTest>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct DeviceCertTest {
    #[prost(string, optional, tag = "1")]
    description: Option<String>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    der_cert_path: Vec<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    crl_bundle: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "4")]
    expected_result: Option<i32>,
    #[prost(uint64, optional, tag = "5")]
    cert_verification_time_seconds: Option<u64>,
    #[prost(uint64, optional, tag = "6")]
    crl_verification_time_seconds: Option<u64>,
    #[prost(bool, optional, tag = "7")]
    use_test_trust_anchors: Option<bool>,
}

/// Open Screen cast_crl_unittest RunTest semantics, case by case.
#[test]
fn test_open_screen_crl_conformance_suite() {
    let suite = DeviceCertTestSuite::decode(data!("testsuite1.pb")).unwrap();
    let test_device = certs(data!("cast_test_root_ca.pem"));
    let test_crl = certs(data!("cast_crl_test_root_ca.pem"));
    let production_crl = vec![testing::CAST_CRL_ROOT_CA.to_vec()];
    let production_device = production_anchors();
    let mut failures = Vec::new();
    let mut categories = [0usize; 7];
    for case in &suite.tests {
        let expected = case.expected_result.unwrap_or(0);
        categories[expected.clamp(0, 6) as usize] += 1;
        let (device_anchors, crl_anchors) = if case.use_test_trust_anchors.unwrap_or(true) {
            (&test_device, &test_crl)
        } else {
            (&production_device, &production_crl)
        };
        let cert_time = case.cert_verification_time_seconds.unwrap_or(0) as i64;
        let crl_time = match case.crl_verification_time_seconds.unwrap_or(0) {
            0 => cert_time,
            time => time as i64,
        };
        let bundle = case.crl_bundle.clone().unwrap_or_default();
        let chain = &case.der_cert_path;
        let path_ok = testing::check_path(chain, device_anchors, cert_time).is_ok();
        let crl_ok = testing::crl_is_valid(&bundle, crl_time, crl_time, crl_anchors, None);
        let permits = testing::crl_permits(
            &bundle,
            crl_time,
            crl_anchors,
            chain,
            device_anchors,
            cert_time,
        );
        let passed = match expected {
            1 => path_ok && (bundle.is_empty() || permits == Ok(Some(true))),
            2 => !path_ok,
            3 => !crl_ok,
            4 | 6 => path_ok && permits == Ok(Some(false)),
            5 => path_ok && !crl_ok,
            _ => false,
        };
        if !passed {
            failures.push(case.description.clone().unwrap_or_default());
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:#?}");
    // Success, path, CRL and revocation outcomes must all be exercised.
    assert_eq!(categories[0], 0);
    assert!(
        categories[1..=5].iter().all(|count| *count > 0),
        "{categories:?}"
    );
    assert!(suite.tests.len() >= 30, "{}", suite.tests.len());
}
