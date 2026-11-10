//! Hisense Interactive Display (`EShare` / `HiScreenShare`) protocol adapter.
//!
//! Activated only when the alpha Hisense integration switch is enabled.
//! Implements the reverse-engineered EShare/HiScreenShare connection flow:
//! 1. Optional TCP 8121 control handshake (`getServerInfo` / `sayHello`).
//! 2. Optional TCP 57395 master-control registration (`reportClientInfo` /
//!    `requestCast: 7` / 1 s `heartbeat` / `disconnect`).
//! 3. RTSP `OPTIONS` + single-step video `SETUP` with `androdstream: true` and
//!    `MacOSXStream: true` (`User-Agent: EShare (EShare 1.0)`), avoiding the
//!    control-first `SETUP` crash in EShare's `RTSPResponder`.
//! 4. Unencrypted Annex-B H.264 streaming over TCP `dataPort` (default 51030)
//!    using EShare's 128-byte little-endian header (`0x0100` for SPS/PPS,
//!    `0x0101` for H.264 access units), with 2 s RTSP `OPTIONS` keepalives and
//!    explicit `TEARDOWN` on stop.

use crate::{
    Capability, Error, Profile, Stats,
    session::Info,
    wire::{self, Connection, Response, dict, integer, number},
};
use plist::{Dictionary, Value};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
};

const CONTROL_PORT: u16 = 8121;
const MASTER_CONTROL_PORT: u16 = 57395;
const ESHARE_HTTP_PORT: u16 = 8000;
pub(crate) const ESHARE_RTSP_PORT: u16 = 51040;
pub(crate) const DEFAULT_VIDEO_PORT: u16 = 51030;
pub(crate) const FALLBACK_VIDEO_PORT: u16 = 51035;
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Checks whether a live receiver endpoint is a Hisense / EShare interactive
/// display board.
pub(crate) async fn is_hisense(endpoint: SocketAddr, server_header: Option<&str>) -> bool {
    server_header.is_some_and(eshare_server) || probe_http_port(endpoint.ip()).await
}

// AirTunes/211.3 and sourceVersion are shared by ordinary AirPlay receivers.
// Only an explicit EShare server identity selects this unencrypted protocol.
pub(crate) fn eshare_server(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value == "eshare" || value.starts_with("eshare/") || value.starts_with("eshare ")
}

/// Fallback probe when a Hisense endpoint does not serve Apple `/info`.
pub(crate) async fn probe_direct(endpoint: SocketAddr) -> Option<Info> {
    if !probe_http_port(endpoint.ip()).await {
        return None;
    }
    let (width, height) = canvas(endpoint.ip()).await.unwrap_or((1920, 1080));
    Some(Info {
        capability: Capability {
            profile: Profile::Hisense,
            screen: true,
            audio: true,
            width,
            height,
            pairing_required: false,
            pin_pairing: false,
            max_fps: 60,
        },
        features: 0x527ffff7,
        http: None,
        hap: None,
        device: None,
        password: false,
        server: Some("AirTunes/211.3".into()),
    })
}

/// The board's casting canvas, read with one keep-alive `OPTIONS` on the
/// EShare RTSP port before any `SETUP`. The reply's encoder attributes
/// (e.g. `"casting_win_width":"3840","casting_win_height":"2160"`) are the
/// size native senders encode at; nothing starts on the board.
pub(crate) async fn canvas(ip: IpAddr) -> Option<(u32, u32)> {
    let stream = tokio::time::timeout(
        PROBE_TIMEOUT,
        TcpStream::connect(SocketAddr::new(ip, ESHARE_RTSP_PORT)),
    )
    .await
    .ok()?
    .ok()?;
    stream.set_nodelay(true).ok()?;
    let mut control = RtspControl::new(stream, ip);
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        control.request("OPTIONS", &[], &[], "Hisense canvas OPTIONS"),
    )
    .await
    .ok()?
    .ok()?;
    canvas_size(&response.body)
}

/// Even canvas size from an encoder-attribute body, within 320×240–7680×4320.
#[doc(hidden)]
pub fn canvas_size(body: &[u8]) -> Option<(u32, u32)> {
    let width = attribute(body, "casting_win_width")?;
    let height = attribute(body, "casting_win_height")?;
    ((320..=7680).contains(&width) && (240..=4320).contains(&height))
        .then_some((width & !1, height & !1))
}

async fn probe_http_port(ip: IpAddr) -> bool {
    let addr = SocketAddr::new(ip, ESHARE_HTTP_PORT);
    let Ok(Ok(mut stream)) = tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await
    else {
        return false;
    };
    let _ = stream.set_nodelay(true);
    let request = format!("HEAD / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, async {
            stream.write_all(request.as_bytes()).await?;
            stream.flush().await?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                if header.len() >= 4096 {
                    return Ok::<_, std::io::Error>(false);
                }
                header.push(stream.read_u8().await?);
            }
            let Ok(header) = std::str::from_utf8(&header) else {
                return Ok(false);
            };
            Ok(header.lines().skip(1).any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("server") && eshare_server(value)
                })
            }))
        })
        .await,
        Ok(Ok(true))
    )
}

/// Runs the best-effort EShare control (TCP 8121) and master-control (TCP 57395)
/// handshakes before RTSP `SETUP`.
pub(crate) async fn handshake_auxiliary(ip: IpAddr, mut stop_rx: watch::Receiver<bool>) {
    let name = janus_media::identity::sender_name();
    if let Ok(Ok(mut stream)) = tokio::time::timeout(
        PROBE_TIMEOUT,
        TcpStream::connect(SocketAddr::new(ip, CONTROL_PORT)),
    )
    .await
    {
        let _ = stream.set_nodelay(true);
        let _ = tokio::time::timeout(PROBE_TIMEOUT, async {
            stream.write_all(b"getServerInfo\r\n1\r\n\r\n").await?;
            stream.flush().await?;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(format!("sayHello\r\n{name}\r\n\r\n").as_bytes())
                .await?;
            stream.flush().await
        })
        .await;
    }

    if let Ok(Ok(mut master)) = tokio::time::timeout(
        PROBE_TIMEOUT,
        TcpStream::connect(SocketAddr::new(ip, MASTER_CONTROL_PORT)),
    )
    .await
    {
        let _ = master.set_nodelay(true);
        let report = format!(
            "{{\"reportClientInfo\":\"{name}\",\"clientName\":\"{name}\",\"clientType\":3,\"isTouchable\":0,\"versionName\":\"v1.8.514\",\"versionCode\":20260514}}\r\n{{\"requestCast\":7}}\r\n"
        );
        if tokio::time::timeout(PROBE_TIMEOUT, async {
            master.write_all(report.as_bytes()).await?;
            master.flush().await
        })
        .await
        .is_ok()
        {
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut count = 0u64;
                loop {
                    if *stop_rx.borrow() {
                        break;
                    }
                    tokio::select! {
                        biased;
                        _ = stop_rx.changed() => break,
                        _ = tick.tick() => {
                            let line = format!("{{\"heartbeat\":{count}}}\r\n");
                            count = count.wrapping_add(1);
                            if tokio::time::timeout(
                                Duration::from_secs(1),
                                master.write_all(line.as_bytes()),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    master.write_all(format!("{{\"disconnect\":\"{name}\"}}\r\n").as_bytes()),
                )
                .await;
            });
        }
    }
}

/// EShare mirroring has its own RTSP service; mDNS advertises the separate
/// AirPlay service (usually 51010). Only a failed TCP connection permits using
/// the advertised connection. A receiver's RTSP rejection is final.
pub(crate) async fn connect_rtsp(
    advertised: Connection<TcpStream>,
    endpoint: SocketAddr,
) -> Result<RtspControl<TcpStream>, Error> {
    if endpoint.port() != ESHARE_RTSP_PORT
        && let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(SocketAddr::new(endpoint.ip(), ESHARE_RTSP_PORT)),
        )
        .await
    {
        stream.set_nodelay(true)?;
        return Ok(RtspControl::new(stream, endpoint.ip()));
    }
    Ok(RtspControl::from_connection(advertised, endpoint.ip()))
}

pub(crate) struct NegotiatedHisense {
    pub video_port: u16,
    pub event_port: Option<u16>,
}

/// EShare RTSP control client (`User-Agent: EShare (EShare 1.0)`).
pub(crate) struct RtspControl<S> {
    pub stream: S,
    uri: String,
    seq: u32,
    session: Option<String>,
    usable: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> RtspControl<S> {
    pub(crate) fn new(stream: S, ip: IpAddr) -> Self {
        Self {
            stream,
            uri: format!("rtsp://{ip}"),
            seq: 0,
            session: None,
            usable: true,
        }
    }

    pub(crate) fn from_connection(connection: Connection<S>, ip: IpAddr) -> Self {
        Self {
            stream: connection.stream,
            uri: format!("rtsp://{ip}"),
            seq: connection.seq,
            session: connection.session,
            usable: true,
        }
    }

    async fn request(
        &mut self,
        method: &str,
        body: &[u8],
        extra: &[(&str, &str)],
        stage: &'static str,
    ) -> Result<Response, Error> {
        if !self.usable || body.len() > wire::MAX_BODY {
            return Err(Error::Protocol);
        }
        self.seq = self.seq.checked_add(1).ok_or(Error::Protocol)?;
        let mut header = format!("{method} {} RTSP/1.0\r\nCSeq: {}\r\n", self.uri, self.seq);
        if !body.is_empty() {
            header.push_str(&format!(
                "Content-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        header.push_str("User-Agent: EShare (EShare 1.0)\r\n");
        if let Some(session) = &self.session {
            header.push_str(&format!("Session: {session}\r\n"));
        }
        for (k, v) in extra {
            header.push_str(&format!("{k}: {v}\r\n"));
        }
        header.push_str("\r\n");
        self.usable = false;
        let result = tokio::time::timeout(wire::DEADLINE, async {
            self.stream.write_all(header.as_bytes()).await?;
            self.stream.write_all(body).await?;
            self.stream.flush().await?;
            wire::read_response(&mut self.stream, self.seq).await
        })
        .await
        .map_err(|_| Error::RequestTimeout { stage })?;
        let response =
            result.map_err(|e| e.response(stage, "malformed RTSP headers or framing"))?;
        self.usable = true;
        if let Some(session) = response.headers.get("session") {
            let token = session.split(';').next().unwrap_or("").trim();
            if !token.is_empty()
                && token.len() <= 128
                && token.bytes().all(|b| b.is_ascii_graphic())
            {
                self.session = Some(token.to_owned());
            }
        }
        match response.status {
            200..=299 => Ok(response),
            code => Err(Error::Status(code)),
        }
    }

    pub(crate) async fn options(&mut self) -> Result<(), Error> {
        self.request("OPTIONS", &[], &[], "Hisense OPTIONS")
            .await
            .map(|_| ())
    }

    /// Keep-alive `OPTIONS`; returns whether the board asked for an IDR.
    /// Native -[RTSPClient checkChangeEncoderAttribute:] reads the JSON body,
    /// e.g. `{"Framerate":"15","idr_req":"0","casting_win_width":"3840",…}`.
    pub(crate) async fn keepalive(&mut self) -> Result<bool, Error> {
        let response = self.request("OPTIONS", &[], &[], "Hisense OPTIONS").await?;
        Ok(idr_requested(&response.body))
    }

    pub(crate) async fn setup_video(&mut self) -> Result<NegotiatedHisense, Error> {
        let name = janus_media::identity::sender_name();
        let stream = dict([
            ("type", integer(110)),
            ("androdstream", Value::Boolean(true)),
            ("MacOSXStream", Value::Boolean(true)),
            ("dataPort", integer(0)),
            ("controlPort", integer(0)),
            ("androidVersion", integer(33)),
            ("machine_name", Value::Data(name.as_bytes().to_vec())),
            ("showKeyboardMenu", Value::Boolean(false)),
        ]);
        let payload = wire::encode(&dict([("streams", Value::Array(vec![stream]))]))?;
        let response = self
            .request(
                "SETUP",
                &payload,
                &[("DeviceName", &name), ("SwitchFullscreen", "1")],
                "video SETUP",
            )
            .await?;
        let parsed = if response.body.is_empty() {
            Dictionary::new()
        } else {
            wire::decode_response(&response.body, "video SETUP")?
        };
        let video_port = extract_video_port(&parsed).unwrap_or(DEFAULT_VIDEO_PORT);
        let event_port = number(&parsed, "eventPort")
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0);
        Ok(NegotiatedHisense {
            video_port,
            event_port,
        })
    }

    /// Native -[RTSPClient setupAudio]: AAC-ELD, 44.1 kHz stereo.
    /// The receiver chooses its UDP port; it is not the default video port.
    pub(crate) async fn setup_audio(&mut self) -> Result<u16, Error> {
        let stream = dict([("type", integer(96)), ("audioFormat", integer(0x0100_0000))]);
        let payload = wire::encode(&dict([("streams", Value::Array(vec![stream]))]))?;
        let response = self
            .request("SETUP", &payload, &[], "Hisense audio SETUP")
            .await?;
        let reply = wire::decode_response(&response.body, "Hisense audio SETUP")?;
        reply
            .get("streams")
            .and_then(Value::as_array)
            .and_then(|streams| {
                streams
                    .iter()
                    .filter_map(Value::as_dictionary)
                    .find(|stream| number(stream, "type") == Some(96))
            })
            .and_then(|stream| number(stream, "dataPort"))
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
            .ok_or(Error::InvalidResponse {
                stage: "Hisense audio SETUP",
                reason: "missing or invalid audio dataPort",
            })
    }

    pub(crate) async fn teardown(&mut self) -> Result<(), Error> {
        // Native -[RTSPClient teardown] sends only the video stream type.
        let stream = dict([("type", integer(110))]);
        let payload = wire::encode(&dict([("streams", Value::Array(vec![stream]))]))?;
        self.request("TEARDOWN", &payload, &[], "TEARDOWN")
            .await
            .map(|_| ())
    }
}

/// Whether an EShare encoder-attribute body asks for an IDR
/// (`"idr_req":"1"`, string or number). A malformed body is no.
#[doc(hidden)]
pub fn idr_requested(body: &[u8]) -> bool {
    attribute(body, "idr_req").is_some_and(|v| v > 0)
}

/// Bounded scan of a flat EShare JSON body (≤4 KiB) for one unsigned
/// attribute, written as a string or a number.
fn attribute(body: &[u8], key: &str) -> Option<u32> {
    if body.len() > 4096 {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let quoted = format!("\"{key}\"");
    let at = text.find(&quoted)?;
    let rest = text[at + quoted.len()..].trim_start().strip_prefix(':')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let digits: String = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .take(9)
        .collect();
    digits.parse().ok()
}

fn extract_video_port(d: &Dictionary) -> Option<u16> {
    if let Some(streams) = d.get("streams").and_then(Value::as_array) {
        for s in streams {
            if let Some(sd) = s.as_dictionary()
                && number(sd, "type") == Some(110)
                && let Some(port) = number(sd, "dataPort")
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
            {
                return Some(port);
            }
        }
    }
    number(d, "dataPort")
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
}

pub(crate) async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    control: &mut RtspControl<S>,
) -> Result<NegotiatedHisense, Error> {
    control.options().await?;
    control.setup_video().await
}

pub(crate) async fn control_loop<S: AsyncRead + AsyncWrite + Unpin>(
    mut control: RtspControl<S>,
    stop: watch::Sender<bool>,
    mut cancelled: watch::Receiver<bool>,
    stats: watch::Sender<Stats>,
) -> (Option<Error>, bool) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut failure = None;
    loop {
        if *cancelled.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = cancelled.changed() => break,
            _ = interval.tick() => {
                let result = tokio::select! {
                    biased;
                    _ = cancelled.changed() => break,
                    result = control.keepalive() => result,
                };
                let idr = match result {
                    Ok(idr) => idr,
                    Err(error) => {
                        failure = Some(error);
                        stop.send_replace(true);
                        break;
                    }
                };
                stats.send_modify(|s| {
                    s.feedback_replies += 1;
                    s.key_frame_requests += u64::from(idr);
                });
            }
        }
    }
    let teardown = matches!(
        tokio::time::timeout(Duration::from_secs(2), control.teardown()).await,
        Ok(Ok(()))
    );
    (failure, teardown)
}

/// Native ESAudioSession RTPPackAndSend:withlen:. The clock is milliseconds,
/// not the AirPlay sample clock. SSRC bytes are copied exactly from the native
/// sender (01 00 00 00); the AAC access unit has no ADTS or AU-length header.
pub(crate) fn audio_packet(data: &[u8], sequence: u16, millis: u32) -> Result<Vec<u8>, Error> {
    if data.is_empty() || data.len() > 1460 {
        return Err(Error::Protocol);
    }
    let mut packet = Vec::with_capacity(12 + data.len());
    packet.extend_from_slice(&[0x80, 0x60]);
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&millis.to_be_bytes());
    packet.extend_from_slice(&[1, 0, 0, 0]);
    packet.extend_from_slice(data);
    Ok(packet)
}
