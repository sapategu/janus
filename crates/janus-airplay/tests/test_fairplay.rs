//! Public upstream vectors from Doubletake 5e2f1cb/fpsap_test.go.
use janus_airplay::testing::{descriptor_vector, table_digest};
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn local() -> [u8; 128] {
    hex(concat!(
        "0001e4e3dd688293e6fa66b95ba41768e587c65f750218ff1be21543d573cefb",
        "087bd36e0c6363c3c8242f4abcfa6d660b801032015405eb4ab04dda7aeff38f",
        "fb36f4cfa48f0b5d92ae363f68b45925bbe6413ab6bdc4968f548d21e67d20f",
        "1912b6820e53f1013cde29df7350a9b9fa7c51320aea62d2949786c87642e34ba"
    ))
    .try_into()
    .unwrap()
}
#[test]
fn test_expanded_whitebox_tables_match_upstream_digest() {
    assert_eq!(
        table_digest(),
        hex("28d0986abebe30458348dfa2957aa1d52d6f3ad5a9468c5d8a9c4139b7ca2b43")
    );
}
#[test]
fn test_legacy_fairplay_matches_public_golden_vectors() {
    let mut cases = vec![
        ([0; 128], "6f627565f3e77f5b5ede91beee7baf92e4241e0b"),
        ([255; 128], "dc2cc74f2ed55484f59f95b96082f0f5c017dd17"),
    ];
    for (index, want) in [
        (0, "9bfb9556b8659c2ac94b7ef9e587d71e159ea624"),
        (63, "150d9fa4eb456e73ba48de5779c5c996b16b3b23"),
        (64, "a167db30424ff8890d085c0f1c92b2c5cc06fc45"),
        (127, "d246ec5e7adc8118994b8df77146529486ac7caf"),
    ] {
        let mut body = [0; 128];
        body[index] = 0x42;
        cases.push((body, want));
    }
    assert_eq!(cases.len(), 6);
    for (body, want) in cases {
        assert_eq!(
            descriptor_vector(local(), 3, body).unwrap().1.as_slice(),
            hex(want)
        );
    }
    assert_eq!(
        descriptor_vector(local(), 3, [0; 128])
            .unwrap()
            .0
            .as_slice(),
        hex("7e38958ffe4ed433743919fe7eb16376afa4eb9e")
    );
    assert!(descriptor_vector(local(), 4, [0; 128]).is_none());
}

#[test]
fn test_message_and_wrapped_key_match_separately_executed_go_reference() {
    let (m3, wrapped) = janus_airplay::testing::keywrap_vector().unwrap();
    assert_eq!(
        m3,
        hex(
            "46504c590301030000000098038f1a9cc6482c070fe4de48dace32b061ca1df4ae615b7289df84c524f1bc1825368d48dbcda5670172d6fb0eb538f3b9fce57d6aaf713be09191fda7c3a15d0081f2831dfa269ffa02b90f846a65c6367ae1087ecde279df2ea8f6c974f8b2d983d4a3ba6663a3350e95c45c5e5c70332ff0f6ac20f228f7012beb9fd6dec7d5a2d0d90b7669d2205067ca16d42f5669d78e859e8e3856"
        )
    );
    assert_eq!(
        wrapped,
        hex(
            "46504c59010201000000003c000000005a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a000000107635858e3466d61817f22bc5c82d344a10ee0967a065f76ae954ffb734e64795a89d19f6"
        )
    );
}
