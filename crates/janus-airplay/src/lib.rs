//! Native, bounded AirPlay mirroring: the legacy raw-pairing/FairPlay
//! profiles and HomeKit-style PIN pairing with encrypted control. No external
//! sender runtime.
mod audio;
mod fairplay;
mod hap;
mod hisense;
mod http;
mod pairings;
mod sap;
mod secure;
mod session;
mod streaming;
mod tables;
mod video;
mod wire;
pub use audio::{SAMPLES as AUDIO_SAMPLES, alac};
pub use pairings::{PinPairing, begin_pin_pairing, default_store_path};
pub use session::{Capability, probe};
pub use streaming::{Sender, Stats, start};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Rtsp,
    LegacyHttp,
    /// HomeKit-style pairing, encrypted control and ChaCha20-Poly1305 video.
    Paired,
    /// Hisense interactive display (EShare / HiScreenShare) mirroring.
    Hisense,
}
impl Profile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rtsp => "RTSP mirroring",
            Self::LegacyHttp => "legacy HTTP mirroring",
            Self::Paired => "paired RTSP mirroring",
            Self::Hisense => "Hisense interactive display",
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("couldn't create fresh AirPlay keys")]
    Entropy,
    #[error("AirPlay authentication failed")]
    Authentication,
    #[error("invalid AirPlay response")]
    Protocol,
    #[error("invalid AirPlay response during {stage}: {reason}")]
    InvalidResponse {
        stage: &'static str,
        reason: &'static str,
    },
    #[error("AirPlay connection failed: {0}")]
    Io(String),
    #[error("AirPlay request timed out")]
    Timeout,
    #[error("AirPlay request timed out during {stage}")]
    RequestTimeout { stage: &'static str },
    #[error(
        "AirPlay request timed out during {stage} (timing: {timing_requests} requests received, {timing_replies} replies sent, {timing_ignored} other datagrams ignored)"
    )]
    StartupTimeout {
        stage: &'static str,
        timing_requests: u64,
        timing_replies: u64,
        timing_ignored: u64,
    },
    #[error("this AirPlay security or media profile is not supported yet")]
    Unsupported,
    #[error(
        "this receiver requires pairing or a password; Janus does not support that AirPlay pairing flow yet"
    )]
    PairingRequired,
    #[error("Janus isn't paired with this TV yet; pair it once with the code the TV shows")]
    NotPaired,
    #[error("the pairing code wasn't accepted; start pairing again and type the code the TV shows")]
    PinRejected,
    #[error("the receiver refused pairing (HomeKit error {0})")]
    PairingRejected(u8),
    #[error("couldn't find a config directory to store the AirPlay pairing")]
    NoPairingStore,
    #[error("Sound isn't available for this AirPlay receiver/profile yet; turn Sound off")]
    AudioUnsupported,
    #[error("AirPlay receiver rejected the request (status {0})")]
    Status(u16),
    #[error("AirPlay sharing stopped")]
    Stopped,
    #[error("AirPlay media queue is full")]
    QueueFull,
    #[error("the receiver ended AirPlay sharing")]
    ReceiverEnded,
}
impl Error {
    pub(crate) fn response(self, stage: &'static str, reason: &'static str) -> Self {
        match self {
            Self::Protocol => Self::InvalidResponse { stage, reason },
            Self::InvalidResponse { reason, .. } => Self::InvalidResponse { stage, reason },
            Self::Timeout | Self::RequestTimeout { .. } => Self::RequestTimeout { stage },
            other => other,
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Facts only: this facade cannot construct a live or trusted session.
#[doc(hidden)]
pub mod testing;
