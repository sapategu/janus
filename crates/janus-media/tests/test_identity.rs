use janus_media::identity::{sender_name, sender_name_for};

/// Receivers list the sender as `janus@<host>`; the name must stay safe in
/// JSON control lines, plists and RTSP headers.
#[test]
fn sender_name_is_janus_at_short_hostname() {
    let cases = [
        ("fedora\n", "janus@fedora"),
        ("ubuntu-laptop", "janus@ubuntu-laptop"),
        ("kubuntu.example.lan", "janus@kubuntu"),
        ("bad\"name\r\nX: y", "janus@badnameXy"),
        ("", "janus"),
        ("  \n", "janus"),
    ];
    assert_eq!(cases.len(), 6);
    for (host, expected) in cases {
        assert_eq!(sender_name_for(host), expected, "{host:?}");
    }
    assert_eq!(sender_name_for(&"h".repeat(100)).len(), "janus@".len() + 48);
}

#[test]
fn this_computer_has_a_safe_sender_name() {
    let name = sender_name();
    assert!(name == "janus" || name.starts_with("janus@"), "{name}");
    assert!(
        name.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@-_".contains(&b))
    );
}
