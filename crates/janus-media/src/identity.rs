//! The name receivers show for this sender: `janus@<host>` (e.g.
//! `janus@fedora`), shared by every protocol (Google Cast `userAgent`, AirPlay
//! SETUP/pairing client name, Hisense/EShare SETUP and control lines).
//! It is a display name only; pairing identities are separate and stable.

/// `janus@<short host name>` for this computer, or `janus` when the host name
/// cannot be read. Uses only files and environment (no `unsafe` system call).
pub fn sender_name() -> String {
    let host = ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_default();
    sender_name_for(&host)
}

/// `janus@<short host name>` keeping only ASCII letters, digits, `-` and `_`
/// (at most 48), so the name is safe inside JSON, plists and RTSP headers.
pub fn sender_name_for(host: &str) -> String {
    let short = host.trim().split('.').next().unwrap_or_default();
    let clean: String = short
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(48)
        .collect();
    if clean.is_empty() {
        "janus".to_owned()
    } else {
        format!("janus@{clean}")
    }
}
