//! Stable Protobuf tags and length bounds. The schema is in proto/control.proto.
//! Prost derives keep the offline build independent of a protoc executable.

use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN: &[u8] = b"janus-ctrl/1";
pub const PROTOCOL_MAJOR: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Clone, PartialEq, Message)]
pub struct Envelope {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub session_id: Vec<u8>,
    #[prost(uint64, tag = "3")]
    pub generation: u64,
    #[prost(uint64, tag = "4")]
    pub sequence: u64,
    #[prost(uint64, tag = "5")]
    pub reply_to: u64,
    #[prost(uint32, tag = "6")]
    pub schema_minor: u32,
    #[prost(oneof = "Payload", tags = "10, 11, 12, 13, 14")]
    pub payload: Option<Payload>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Payload {
    #[prost(message, tag = "10")]
    Hello(Hello),
    #[prost(message, tag = "11")]
    Offer(Offer),
    #[prost(message, tag = "12")]
    Status(Status),
    #[prost(message, tag = "13")]
    Heartbeat(Empty),
    #[prost(message, tag = "14")]
    Stop(Empty),
}

#[derive(Clone, PartialEq, Message)]
pub struct Hello {
    #[prost(uint32, tag = "1")]
    pub major: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Offer {
    #[prost(enumeration = "Mode", tag = "1")]
    pub mode: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Status {
    #[prost(enumeration = "Code", tag = "1")]
    pub code: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Empty {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum Mode {
    Unspecified = 0,
    Synthetic = 1,
    Video = 2,
    AudioOnly = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum Code {
    Unspecified = 0,
    Ok = 1,
    Disabled = 2,
    Busy = 3,
    Stale = 4,
    Expired = 5,
    Unsupported = 6,
    Invalid = 7,
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("invalid control message length")]
    Length,
    #[error("invalid control envelope")]
    Invalid,
    #[error("invalid protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("control I/O: {0}")]
    Io(#[from] std::io::Error),
}

impl Envelope {
    pub fn validate(&self) -> Result<(), WireError> {
        if self.request_id == 0 || self.sequence == 0 || self.payload.is_none() {
            return Err(WireError::Invalid);
        }
        if !self.session_id.is_empty() && self.session_id.len() != 16 {
            return Err(WireError::Invalid);
        }
        match self.payload.as_ref().ok_or(WireError::Invalid)? {
            Payload::Hello(_) => {
                if !self.session_id.is_empty() || self.generation != 0 {
                    return Err(WireError::Invalid);
                }
            }
            Payload::Offer(offer) => {
                if self.session_id.len() != 16
                    || self.generation != 0
                    || !matches!(
                        Mode::try_from(offer.mode),
                        Ok(Mode::Synthetic | Mode::Video | Mode::AudioOnly)
                    )
                {
                    return Err(WireError::Invalid);
                }
            }
            Payload::Heartbeat(_) | Payload::Stop(_) => {
                if self.session_id.len() != 16 || self.generation == 0 {
                    return Err(WireError::Invalid);
                }
            }
            Payload::Status(status) => {
                if self.reply_to == 0
                    || !matches!(Code::try_from(status.code), Ok(code) if code != Code::Unspecified)
                {
                    return Err(WireError::Invalid);
                }
            }
        }
        Ok(())
    }
}

pub fn decode(bytes: &[u8]) -> Result<Envelope, WireError> {
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::Length);
    }
    let envelope = Envelope::decode(bytes)?;
    envelope.validate()?;
    Ok(envelope)
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Envelope, WireError> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(WireError::Length);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    decode(&bytes)
}

pub async fn write(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &Envelope,
) -> Result<(), WireError> {
    message.validate()?;
    let length = message.encoded_len();
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(WireError::Length);
    }
    writer.write_u32(length as u32).await?;
    writer.write_all(&message.encode_to_vec()).await?;
    writer.flush().await?;
    Ok(())
}
