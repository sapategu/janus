//! Native Cast V2 wire framing. Field tags follow the Open Screen protocol
//! schema, blob a1ecf3471a642a5a639d71cd4feb98e7ec1e0dca.
//! https://chromium.googlesource.com/openscreen/+/refs/heads/main/cast/common/channel/proto/cast_channel.proto

use bytes::Bytes;
use prost::Message as _;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 65_536;
const MAX_ID_BYTES: usize = 128;
const MAX_NAMESPACE_BYTES: usize = 128;
pub const SENDER: &str = "sender-0";
pub const RECEIVER: &str = "receiver-0";
pub const AUTH_NAMESPACE: &str = "urn:x-cast:com.google.cast.tp.deviceauth";
pub const CONNECTION_NAMESPACE: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const HEARTBEAT_NAMESPACE: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const RECEIVER_NAMESPACE: &str = "urn:x-cast:com.google.cast.receiver";

// Required fields are represented as Option so presence is checked explicitly
// rather than accepting proto2's default scalar values for absent fields.
/// Raw Cast V2 envelope schema, exposed for wire tests. Use [`Message`].
#[doc(hidden)]
#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    #[prost(int32, optional, tag = "1")]
    pub version: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub source: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub destination: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub namespace: Option<String>,
    #[prost(int32, optional, tag = "5")]
    pub payload_type: Option<i32>,
    #[prost(string, optional, tag = "6")]
    pub text: Option<String>,
    #[prost(bytes = "bytes", optional, tag = "7")]
    pub binary: Option<Bytes>,
    #[prost(bool, optional, tag = "8")]
    pub continued: Option<bool>,
    #[prost(uint32, optional, tag = "9")]
    pub remaining: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Text(String),
    Binary(Bytes),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: Payload,
}

// Protocol payloads can contain session identifiers or certificate material.
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastMessage")
            .field("payload", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid Cast frame length")]
    Length,
    #[error("invalid Cast message fields")]
    Invalid,
    #[error("unsupported Cast wire version or chunking")]
    Unsupported,
    #[error("invalid Cast protobuf")]
    Protobuf(#[from] prost::DecodeError),
    #[error("Cast connection I/O failed")]
    Io(#[from] std::io::Error),
}

impl Message {
    pub fn new(
        source: &str,
        destination: &str,
        namespace: &str,
        payload: Payload,
    ) -> Result<Self, Error> {
        let message = Self {
            source: source.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload,
        };
        message.validate()?;
        Ok(message)
    }

    fn validate(&self) -> Result<(), Error> {
        let valid_id = |id: &str| {
            !id.is_empty()
                && id.len() <= MAX_ID_BYTES
                && id.bytes().all(|byte| byte.is_ascii_graphic())
        };
        if !valid_id(&self.source)
            || !valid_id(&self.destination)
            || !self.namespace.starts_with("urn:x-cast:")
            || self.namespace.len() <= "urn:x-cast:".len()
            || self.namespace.len() > MAX_NAMESPACE_BYTES
            || !self.namespace.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::Invalid);
        }
        let length = match &self.payload {
            Payload::Text(text) => text.len(),
            Payload::Binary(bytes) => bytes.len(),
        };
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(Error::Length);
        }
        Ok(())
    }
}

pub fn encode(message: &Message) -> Result<Vec<u8>, Error> {
    message.validate()?;
    let (payload_type, text, binary) = match &message.payload {
        Payload::Text(text) => (0, Some(text.clone()), None),
        Payload::Binary(bytes) => (1, None, Some(bytes.clone())),
    };
    let envelope = Envelope {
        version: Some(0),
        source: Some(message.source.clone()),
        destination: Some(message.destination.clone()),
        namespace: Some(message.namespace.clone()),
        payload_type: Some(payload_type),
        text,
        binary,
        continued: None,
        remaining: None,
    };
    if envelope.encoded_len() > MAX_FRAME_BYTES {
        return Err(Error::Length);
    }
    Ok(envelope.encode_to_vec())
}

pub fn decode(bytes: &[u8]) -> Result<Message, Error> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Length);
    }
    let envelope = Envelope::decode(bytes)?;
    let version = envelope.version.ok_or(Error::Invalid)?;
    if version != 0
        || envelope.continued == Some(true)
        || envelope.remaining.is_some_and(|remaining| remaining > 0)
    {
        return Err(Error::Unsupported);
    }
    let payload = match (envelope.payload_type, envelope.text, envelope.binary) {
        (Some(0), Some(text), None) => Payload::Text(text),
        (Some(1), None, Some(binary)) => Payload::Binary(binary),
        _ => return Err(Error::Invalid),
    };
    Message::new(
        &envelope.source.ok_or(Error::Invalid)?,
        &envelope.destination.ok_or(Error::Invalid)?,
        &envelope.namespace.ok_or(Error::Invalid)?,
        payload,
    )
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Message, Error> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(Error::Length);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    decode(&bytes)
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), message: &Message) -> Result<(), Error> {
    let bytes = encode(message)?;
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
