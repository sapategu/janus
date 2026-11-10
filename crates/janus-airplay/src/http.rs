//! Older HTTP mirroring profile. Protocol references and observed capability
//! are recorded in PROVENANCE.md and docs/airplay-handoff.md. No clear video.
use crate::{
    Error, session,
    wire::{self, Connection, dict, integer, number},
};
use plist::Value;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader},
    net::TcpStream,
    sync::watch,
};

pub(crate) const MIRROR_PORT: u16 = 7100;
pub(crate) const TIMING_PORT: u16 = 7010;

pub(crate) struct Legacy {
    pub width: u32,
    pub height: u32,
}
impl Legacy {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let d = wire::decode_xml(bytes)
            .map_err(|e| e.response("HTTP /stream.xml", "invalid XML plist"))?;
        let width = number(&d, "width").ok_or(Error::Protocol)?;
        let height = number(&d, "height").ok_or(Error::Protocol)?;
        let version = d
            .get("version")
            .and_then(Value::as_string)
            .ok_or(Error::Protocol)?;
        if !(320..=16384).contains(&width)
            || !(240..=16384).contains(&height)
            || version.is_empty()
            || version.len() > 32
            || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err(Error::Protocol);
        }
        if let Some(rate) = d.get("refreshRate") {
            let rate = rate.as_real().ok_or(Error::Protocol)?;
            if !rate.is_finite() || !(0.001..=1.).contains(&rate) {
                return Err(Error::Protocol);
            }
        }
        Ok(Self {
            width: width as u32,
            height: height as u32,
        })
    }
}

/// Read-only protocol qualification, before authentication/media selection.
/// The standard mirroring port is never used without a valid capability plist.
pub(crate) async fn probe(mut endpoint: SocketAddr) -> Result<Option<Legacy>, Error> {
    endpoint.set_port(MIRROR_PORT);
    let stream =
        match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(endpoint)).await {
            Ok(Ok(stream)) => stream,
            _ => return Ok(None),
        };
    let mut c = Connection::http(stream, endpoint);
    let response = match c.request("GET", "/stream.xml", "", &[], &[]).await {
        Ok(response) => response,
        Err(Error::PairingRequired) => return Err(Error::PairingRequired),
        Err(Error::Status(404 | 405)) | Err(Error::Io(_)) | Err(Error::RequestTimeout { .. }) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Legacy::parse(&response.body)
        .map(Some)
        .map_err(|e| e.response("HTTP /stream.xml", "invalid legacy mirroring capability"))
}

pub(crate) fn stream_body(
    key: &session::StreamKey,
    device: u64,
    session: i32,
    delay: u32,
) -> Result<Vec<u8>, Error> {
    let names = |values: &[&str]| {
        Value::Array(
            values
                .iter()
                .map(|name| dict([("name", Value::String((*name).into()))]))
                .collect(),
        )
    };
    wire::encode(&dict([
        ("deviceID", integer(device)),
        ("sessionID", Value::Integer(session.into())),
        ("version", Value::String("200.54".into())),
        ("param1", Value::Data(key.wrapped.to_vec())),
        ("param2", Value::Data(key.iv.to_vec())),
        ("latencyMs", integer(delay.into())),
        (
            "fpsInfo",
            names(&[
                "SubS", "B4En", "EnDp", "IdEn", "IdDp", "EQDp", "QueF", "Sent",
            ]),
        ),
        (
            "timestampInfo",
            names(&[
                "SubSu", "BePxT", "AfPxT", "BefEn", "EmEnc", "QueFr", "SndFr",
            ]),
        ),
    ]))
}

/// HTTP FairPlay is fresh on the same connection that becomes the stream.
/// The caller separately verifies the advertised control endpoint's raw proof.
pub(crate) async fn begin<C: AsyncRead + AsyncWrite + Unpin, S: AsyncRead + AsyncWrite + Unpin>(
    authentication: &mut Connection<C>,
    features: u64,
    mut connection: Connection<S>,
    device: u64,
    session_id: i32,
    delay: u32,
) -> Result<(S, session::StreamKey, Instant), Error> {
    let _ = session::pair(authentication, features).await?;
    // Published sender order on the mirroring connection: GET /stream.xml,
    // then FairPlay, then POST /stream. The capability is re-validated here.
    let capability = connection
        .request("GET", "/stream.xml", "", &[], &[])
        .await?;
    Legacy::parse(&capability.body)
        .map_err(|e| e.response("HTTP /stream.xml", "invalid legacy mirroring capability"))?;
    let key = session::exchange_key(&mut connection).await?;
    let body = stream_body(&key, device, session_id, delay)?;
    let origin = Instant::now();
    let stream = connection.into_mirroring(&body, device).await?;
    Ok((stream, key, origin))
}

/// Standard 48-byte NTP, distinct from the RTSP profile's 32-byte packets.
pub(crate) fn timing_reply(request: &[u8], received: u64, sent: u64) -> Option<[u8; 48]> {
    if request.len() < 48 || request[0] & 7 != 3 || !matches!((request[0] >> 3) & 7, 3 | 4) {
        return None;
    }
    let mut reply = [0; 48];
    reply[0] = (request[0] & 0x38) | 4;
    reply[1] = 1;
    reply[2] = request[2];
    reply[3] = (-24i8) as u8;
    reply[12..16].copy_from_slice(b"AIRP");
    reply[24..32].copy_from_slice(&request[40..48]);
    reply[32..40].copy_from_slice(&received.to_be_bytes());
    reply[40..48].copy_from_slice(&sent.to_be_bytes());
    Some(reply)
}

#[doc(hidden)]
pub fn parse_capability(bytes: &[u8]) -> Result<(u32, u32), Error> {
    Legacy::parse(bytes).map(|l| (l.width, l.height))
}

/// What the receiver did on the response direction of the stream connection.
/// Labels are fixed local text plus status codes and timings; never payloads.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    pub error: Option<Error>,
    pub events: Vec<String>,
}
pub(crate) const MAX_EVENTS: usize = 6;
/// After local Stop, how long a reply that is already arriving may complete.
const LINGER: Duration = Duration::from_millis(200);
async fn cancelled_or_linger(cancelled: &mut watch::Receiver<bool>, stopped: bool) {
    if stopped {
        tokio::time::sleep(LINGER).await;
    } else {
        let _ = cancelled.changed().await;
    }
}
fn note(events: &mut Vec<String>, origin: Instant, what: String) {
    if events.len() < MAX_EVENTS {
        events.push(format!("{what} at {} ms", origin.elapsed().as_millis()));
    }
}

/// Some receivers send an optional HTTP response after the stream preamble.
/// One reader owns partial replies; video-frame arrivals never cancel parsing.
/// A ready read (reply, FIN or reset) wins over cancellation so the receiver's
/// own reason is recorded even when the media writer fails first.
pub(crate) async fn monitor<R: AsyncRead + Unpin>(
    read: R,
    stop: watch::Sender<bool>,
    mut cancelled: watch::Receiver<bool>,
    origin: Instant,
) -> Observed {
    let mut reader = BufReader::new(read);
    let mut events = Vec::new();
    let result = async {
        for _ in 0..4 {
            let stopped = *cancelled.borrow();
            let available = tokio::select! {
                biased;
                available = reader.fill_buf() => match available {
                    Ok(available) => available,
                    Err(e) => {
                        let what = if e.kind() == std::io::ErrorKind::ConnectionReset {
                            "receiver reset the stream connection".to_owned()
                        } else {
                            format!("stream read failed ({:?})", e.kind())
                        };
                        note(&mut events, origin, what);
                        return Err(e.into());
                    }
                },
                _ = cancelled_or_linger(&mut cancelled, stopped) => return Ok(()),
            };
            // This is a one-way upload after /stream. Read EOF may only close
            // the optional response direction; the writer detects media failure.
            if available.is_empty() {
                note(&mut events, origin, "receiver closed the stream connection (FIN)".into());
                return Ok(());
            }
            if !available.starts_with(b"HTTP/") && !b"HTTP/".starts_with(available) {
                note(&mut events, origin, format!("receiver sent {} non-HTTP bytes", available.len()));
                return Err(Error::Protocol);
            }
            let stopped = *cancelled.borrow();
            let response = tokio::select! {
                biased;
                _ = cancelled_or_linger(&mut cancelled, stopped) => return Ok(()),
                response = tokio::time::timeout(wire::DEADLINE, wire::read_response(&mut reader, 0)) => {
                    response.map_err(|_| Error::RequestTimeout { stage: "HTTP stream response" })??
                },
            };
            note(&mut events, origin, format!("receiver replied HTTP {}", response.status));
            match response.status {
                100..=299 => {},
                401 | 403 | 470 => return Err(Error::PairingRequired),
                other => return Err(Error::Status(other)),
            }
        }
        Err(Error::Protocol)
    }.await;
    let error = result.err().map(|error| {
        stop.send_replace(true);
        error.response("HTTP stream response", "too many or malformed HTTP replies")
    });
    Observed { error, events }
}
