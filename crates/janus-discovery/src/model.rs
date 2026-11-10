use serde::Serialize;
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiverProtocol {
    GoogleCast,
    #[serde(rename = "airplay")]
    AirPlay,
    JanusMoq,
}

impl ReceiverProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::GoogleCast => "Google Cast",
            Self::AirPlay => "AirPlay",
            Self::JanusMoq => "Janus",
        }
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::GoogleCast => "google-cast",
            Self::AirPlay => "airplay",
            Self::JanusMoq => "janus",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    /// DNS-SD alone does not prove reachability, idle state or mirroring support.
    Unknown,
    AdvertisedReady,
    AdvertisedBusy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AdvertisedOutputs {
    pub video: Option<bool>,
    pub audio: Option<bool>,
}

impl AdvertisedOutputs {
    /// Unknown receivers remain visible for later qualification. An explicitly
    /// audio-only receiver must not be offered as a screen/window destination.
    pub fn may_support(self, kind: OutputKind) -> bool {
        match kind {
            OutputKind::Video => self.video != Some(false),
            OutputKind::Audio => self.audio != Some(false),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// Browse identity only; neither this key nor a TXT id authenticates a device.
    pub key: String,
    pub protocol: ReceiverProtocol,
    pub instance: String,
    pub advertised_id: Option<String>,
    pub display_name: String,
    pub model: Option<String>,
    pub endpoints: Vec<SocketAddr>,
    pub availability: Availability,
    /// Output hardware claims, NOT proof of screen-mirroring compatibility.
    pub advertised_outputs: AdvertisedOutputs,
}
