//! Cast Streaming OFFER/ANSWER (namespace `urn:x-cast:com.google.cast.webrtc`).
//! Field names follow Open Screen `offer_messages.cc`, `answer_messages.cc`
//! and `receiver_message.cc` at commit 308a18e24e717ce27231c0aaa713364a41416e54.
use serde::Deserialize;
use thiserror::Error;

pub const WEBRTC_NAMESPACE: &str = "urn:x-cast:com.google.cast.webrtc";
/// Chrome's mirroring payload types for Android/Google TV receivers
/// (Open Screen `kVideoHackForAndroidTV` / `kAudioHackForAndroidTV`).
pub const VIDEO_PAYLOAD_TYPE: u8 = 96;
pub const AUDIO_PAYLOAD_TYPE: u8 = 127;
pub const VIDEO_TIMEBASE: u32 = 90_000;
pub const AUDIO_TIMEBASE: u32 = 48_000;
const MAX_STREAMS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum Media {
    Video {
        codec: &'static str,
        max_frame_rate: u32,
        max_bit_rate: u32,
        width: u32,
        height: u32,
    },
    Audio {
        codec: &'static str,
        bit_rate: u32,
        channels: u32,
    },
}

/// One offered stream. Keys are per-session secrets; Debug redacts them.
#[derive(Clone, PartialEq)]
pub struct StreamOffer {
    pub index: u32,
    pub media: Media,
    pub ssrc: u32,
    pub target_delay_ms: u32,
    pub aes_key: [u8; 16],
    pub aes_iv_mask: [u8; 16],
}

impl std::fmt::Debug for StreamOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOffer")
            .field("index", &self.index)
            .field("media", &self.media)
            .field("ssrc", &self.ssrc)
            .field("keys", &"[redacted]")
            .finish()
    }
}

impl StreamOffer {
    pub fn payload_type(&self) -> u8 {
        match self.media {
            Media::Video { .. } => VIDEO_PAYLOAD_TYPE,
            Media::Audio { .. } => AUDIO_PAYLOAD_TYPE,
        }
    }

    pub fn timebase(&self) -> u32 {
        match self.media {
            Media::Video { .. } => VIDEO_TIMEBASE,
            Media::Audio { .. } => AUDIO_TIMEBASE,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut stream = serde_json::json!({
            "index": self.index,
            "rtpPayloadType": self.payload_type(),
            "rtpProfile": "cast",
            "ssrc": self.ssrc,
            "targetDelay": self.target_delay_ms,
            "aesKey": hex(&self.aes_key),
            "aesIvMask": hex(&self.aes_iv_mask),
            "receiverRtcpEventLog": true,
            "timeBase": format!("1/{}", self.timebase()),
            "codecParameter": "",
        });
        let fields = stream.as_object_mut().expect("object literal");
        match &self.media {
            Media::Video {
                codec,
                max_frame_rate,
                max_bit_rate,
                width,
                height,
            } => {
                fields.insert("type".into(), "video_source".into());
                fields.insert("channels".into(), 1.into());
                fields.insert("codecName".into(), (*codec).into());
                fields.insert("maxFrameRate".into(), max_frame_rate.to_string().into());
                fields.insert("maxBitRate".into(), (*max_bit_rate).into());
                fields.insert("protection".into(), "".into());
                fields.insert("profile".into(), "".into());
                fields.insert("level".into(), "".into());
                fields.insert("errorRecoveryMode".into(), "".into());
                fields.insert(
                    "resolutions".into(),
                    serde_json::json!([{ "width": width, "height": height }]),
                );
            }
            Media::Audio {
                codec,
                bit_rate,
                channels,
            } => {
                fields.insert("type".into(), "audio_source".into());
                fields.insert("channels".into(), (*channels).into());
                fields.insert("codecName".into(), (*codec).into());
                fields.insert("bitRate".into(), (*bit_rate).into());
            }
        }
        stream
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The full OFFER message text for the mirroring session.
pub fn offer_message(sequence: u32, streams: &[StreamOffer]) -> String {
    serde_json::json!({
        "type": "OFFER",
        "seqNum": sequence,
        "offer": {
            "castMode": "mirroring",
            "supportedStreams": streams.iter().map(StreamOffer::to_json).collect::<Vec<_>>(),
        },
    })
    .to_string()
}

/// The receiver's selection: which offered stream indexes it will play, with
/// its own SSRC for each, and the UDP port to send RTP to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub udp_port: u16,
    pub send_indexes: Vec<u32>,
    pub ssrcs: Vec<u32>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnswerError {
    #[error("not an ANSWER for this OFFER")]
    NotAnswer,
    #[error("receiver rejected the offer: {0}")]
    Rejected(String),
    #[error("invalid ANSWER")]
    Invalid,
}

#[derive(Deserialize)]
struct AnswerEnvelope {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "seqNum")]
    sequence: Option<u32>,
    result: Option<String>,
    answer: Option<AnswerBody>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnswerBody {
    udp_port: Option<i64>,
    send_indexes: Option<Vec<i64>>,
    ssrcs: Option<Vec<u64>>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: Option<i64>,
    description: Option<String>,
}

/// Parses an ANSWER, accepting only the reply to `sequence`. Indexes must
/// refer to offered streams and pair one-to-one with receiver SSRCs.
pub fn parse_answer(
    text: &str,
    sequence: u32,
    offered: &[StreamOffer],
) -> Result<Answer, AnswerError> {
    let envelope: AnswerEnvelope = serde_json::from_str(text).map_err(|_| AnswerError::Invalid)?;
    if envelope.kind.as_deref() != Some("ANSWER") || envelope.sequence != Some(sequence) {
        return Err(AnswerError::NotAnswer);
    }
    if envelope.result.as_deref() != Some("ok") {
        let detail = envelope
            .error
            .map(|error| {
                format!(
                    "{} {}",
                    error.code.unwrap_or_default(),
                    error.description.unwrap_or_default()
                )
            })
            .unwrap_or_default();
        return Err(AnswerError::Rejected(detail.chars().take(160).collect()));
    }
    let body = envelope.answer.ok_or(AnswerError::Invalid)?;
    let udp_port = body
        .udp_port
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .ok_or(AnswerError::Invalid)?;
    let send_indexes = body.send_indexes.ok_or(AnswerError::Invalid)?;
    let ssrcs = body.ssrcs.ok_or(AnswerError::Invalid)?;
    if send_indexes.is_empty()
        || send_indexes.len() > MAX_STREAMS
        || send_indexes.len() != ssrcs.len()
    {
        return Err(AnswerError::Invalid);
    }
    let send_indexes = send_indexes
        .into_iter()
        .map(|index| {
            u32::try_from(index)
                .ok()
                .filter(|index| offered.iter().any(|stream| stream.index == *index))
                .ok_or(AnswerError::Invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ssrcs = ssrcs
        .into_iter()
        .map(|ssrc| u32::try_from(ssrc).map_err(|_| AnswerError::Invalid))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, ssrc) in send_indexes.iter().zip(&ssrcs) {
        let offer = offered.iter().find(|stream| stream.index == *index);
        if offer.is_some_and(|offer| offer.ssrc == *ssrc) {
            return Err(AnswerError::Invalid);
        }
    }
    Ok(Answer {
        udp_port,
        send_indexes,
        ssrcs,
    })
}
