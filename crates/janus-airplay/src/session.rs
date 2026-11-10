//! Legacy raw pairing + FairPlay + NTP/SETUP, and the HomeKit-paired SETUP
//! sequence. Security and media are selected by live capabilities and explicit
//! responses, never a receiver name/model.
use crate::{
    Error, Profile,
    fairplay::{FairPlay, random},
    hap,
    wire::{self, Connection, dict, integer, number},
};
use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use ctr::Ctr128BE;
use plist::{Dictionary, Value};
use ring::{
    agreement, digest,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    pub profile: Profile,
    pub screen: bool,
    pub audio: bool,
    pub width: u32,
    pub height: u32,
    /// Janus can't cast until the user acts (password, or a PIN pairing that
    /// hasn't been stored yet).
    pub pairing_required: bool,
    /// The receiver supports one-time PIN pairing that Janus can perform.
    pub pin_pairing: bool,
    /// Highest display frame rate the receiver advertises (default 60).
    pub max_fps: u32,
}
pub(crate) struct Info {
    pub capability: Capability,
    pub features: u64,
    pub http: Option<crate::http::Legacy>,
    /// HomeKit-style pairing: feature 27 (raw pairing) absent and a CoreUtils
    /// pairing feature (43, 46 or 48) advertised.
    pub hap: Option<hap::Kind>,
    pub device: Option<String>,
    pub password: bool,
    pub server: Option<String>,
}
impl Info {
    pub(crate) fn parse(d: &Dictionary) -> Result<Self, Error> {
        let features = number(d, "features").ok_or(Error::InvalidResponse {
            stage: "GET /info",
            reason: "missing or non-integer features",
        })?;
        let flags = if d.contains_key("statusFlags") {
            number(d, "statusFlags").ok_or(Error::InvalidResponse {
                stage: "GET /info",
                reason: "non-integer statusFlags",
            })?
        } else {
            0
        };
        let audio = if let Some(formats) = d.get("supportedFormats") {
            let formats = formats.as_dictionary().ok_or(Error::InvalidResponse {
                stage: "GET /info",
                reason: "supportedFormats is not a dictionary",
            })?;
            number(formats, "screenStream").unwrap_or(0) & 0x40000 != 0
        } else {
            features & (1 << 19) != 0
        };
        let mut max_fps = 60;
        let (mut width, mut height) = if features & (1 << 28) != 0 {
            (1920, 1080)
        } else {
            (1280, 720)
        };
        if let Some(displays) = d.get("displays").and_then(Value::as_array)
            && let Some(display) = displays.first().and_then(Value::as_dictionary)
        {
            if let Some(fps) = number(display, "maxFPS") {
                max_fps = fps.clamp(24, 120) as u32;
            }
            // The maximum mode bounds what may be sent; the current mode is
            // the fallback when no maximum is advertised.
            let w = number(display, "widthPixelsMax")
                .or_else(|| number(display, "widthPixels"))
                .or_else(|| number(display, "width"));
            let h = number(display, "heightPixelsMax")
                .or_else(|| number(display, "heightPixels"))
                .or_else(|| number(display, "height"));
            if let (Some(w), Some(h)) = (w, h) {
                if !(320..=16384).contains(&w) || !(240..=16384).contains(&h) {
                    return Err(Error::InvalidResponse {
                        stage: "GET /info",
                        reason: "display dimensions outside supported bounds",
                    });
                }
                width = w as u32;
                height = h as u32;
            }
        }
        let password = flags & (1 << 7) != 0;
        let hap = (features & (1 << 27) == 0
            && features & ((1 << 43) | (1 << 46) | (1 << 48)) != 0)
            .then(|| hap::Kind::for_features(features));
        let device = d
            .get("deviceID")
            .and_then(Value::as_string)
            .filter(|s| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic()))
            .map(str::to_owned);
        Ok(Self {
            capability: Capability {
                profile: if hap.is_some() {
                    Profile::Paired
                } else {
                    Profile::Rtsp
                },
                screen: features & (1 << 7) != 0,
                audio,
                // Paired receivers may take up to 4K; the older profiles keep
                // their verified 1080p bound.
                width: width.min(if hap.is_some() { 3840 } else { 1920 }),
                height: height.min(if hap.is_some() { 2160 } else { 1080 }),
                // A HAP receiver always needs a stored pairing; see resolve().
                pairing_required: hap.is_some() || flags & ((1 << 7) | (1 << 9)) != 0,
                pin_pairing: hap.is_some() && !password,
                max_fps,
            },
            features,
            http: None,
            hap,
            device,
            password,
            server: None,
        })
    }
    /// A stored PIN pairing for this deviceID satisfies a HAP receiver.
    pub(crate) fn resolve(&mut self) {
        if self.hap.is_some()
            && !self.password
            && self
                .device
                .as_deref()
                .is_some_and(crate::pairings::is_paired)
        {
            self.capability.pairing_required = false;
        }
    }
}
pub(crate) fn valid_endpoint(address: SocketAddr) -> bool {
    if address.port() == 0 {
        return false;
    }
    match address.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}
pub(crate) async fn connect(
    endpoints: &[SocketAddr],
) -> Result<(Connection<TcpStream>, Info, SocketAddr), Error> {
    if endpoints.is_empty() || endpoints.len() > 8 || endpoints.iter().any(|a| !valid_endpoint(*a))
    {
        return Err(Error::Protocol);
    }
    let mut error = Error::Timeout;
    for endpoint in endpoints {
        let stream = match tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(endpoint),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                error = e.into();
                continue;
            }
            Err(_) => continue,
        };
        stream.set_nodelay(true)?;
        let mut connection = Connection::new(stream);
        // Once a receiver answers, authentication/profile failures are final.
        let mut info = match get_info(&mut connection).await {
            Ok(info) => info,
            Err(err) => {
                // Authentication failures must never select an unencrypted profile.
                if matches!(err, Error::Status(401 | 403 | 470)) {
                    return Err(err);
                }
                if let Some(info) = crate::hisense::probe_direct(*endpoint).await
                    && let Ok(Ok(fresh)) =
                        tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(endpoint))
                            .await
                {
                    fresh.set_nodelay(true)?;
                    return Ok((Connection::new(fresh), info, *endpoint));
                }
                return Err(err);
            }
        };
        info.resolve();
        if info.capability.screen && info.hap.is_none() {
            if crate::hisense::is_hisense(*endpoint, info.server.as_deref()).await {
                info.capability.profile = Profile::Hisense;
                info.capability.audio = true;
                // The AirPlay /info size (1920×1080 on the tested board) is not
                // the EShare canvas; the board reports that itself (3840×2160).
                if let Some((width, height)) = crate::hisense::canvas(endpoint.ip()).await {
                    info.capability.width = width;
                    info.capability.height = height;
                }
                return Ok((connection, info, *endpoint));
            }
            info.http = crate::http::probe(*endpoint).await?;
            if let Some(http) = &info.http {
                info.capability.profile = Profile::LegacyHttp;
                info.capability.width = info.capability.width.min(http.width);
                info.capability.height = info.capability.height.min(http.height);
                // The older profile's separate audio protocol is not implemented.
                info.capability.audio = false;
            }
        }
        return Ok((connection, info, *endpoint));
    }
    Err(error)
}
pub async fn probe(endpoints: &[SocketAddr]) -> Result<Capability, Error> {
    let (_, info, _) = connect(endpoints).await?;
    Ok(info.capability)
}
pub(crate) async fn get_info<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
) -> Result<Info, Error> {
    let r = c
        .request("GET", "/info", "application/x-apple-binary-plist", &[], &[])
        .await?;
    let dictionary = wire::decode_response(&r.body, "GET /info")?;
    let mut info = Info::parse(&dictionary)?;
    info.server = r.headers.get("server").cloned();
    Ok(info)
}
pub(crate) fn derive(label: &str, secret: &[u8]) -> [u8; 16] {
    let mut h = digest::Context::new(&digest::SHA512);
    h.update(label.as_bytes());
    h.update(secret);
    h.finish().as_ref()[..16].try_into().expect("digest size")
}
pub(crate) async fn pair<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    features: u64,
) -> Result<Option<Vec<u8>>, Error> {
    // Raw transient pairing proves key possession. It is not a manufacturer
    // identity certificate, and no long-term trust is inferred from discovery.
    let identity =
        Ed25519KeyPair::from_seed_unchecked(&random::<32>()?).map_err(|_| Error::Entropy)?;
    let setup = c
        .request(
            "POST",
            "/pair-setup",
            "application/octet-stream",
            identity.public_key().as_ref(),
            &[],
        )
        .await?;
    if setup.body.len() != 32 {
        return Err(Error::Authentication);
    }
    let private =
        agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
            .map_err(|_| Error::Entropy)?;
    let public = private.compute_public_key().map_err(|_| Error::Entropy)?;
    let mut v1 = vec![1, 0, 0, 0];
    v1.extend_from_slice(public.as_ref());
    v1.extend_from_slice(identity.public_key().as_ref());
    let mix = features & (1 << 27) == 0;
    let headers = if mix {
        vec![("X-Apple-PD", "1")]
    } else {
        Vec::new()
    };
    let response = c
        .request(
            "POST",
            "/pair-verify",
            "application/octet-stream",
            &v1,
            &headers,
        )
        .await?;
    if response.body.len() != 96 {
        return Err(Error::Authentication);
    }
    let remote = &response.body[..32];
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, remote);
    let shared = agreement::agree_ephemeral(private, &peer, |key| key.to_vec())
        .map_err(|_| Error::Authentication)?;
    let key = derive("Pair-Verify-AES-Key", &shared);
    let iv = derive("Pair-Verify-AES-IV", &shared);
    let mut cipher = Ctr128BE::<Aes128>::new(&key.into(), &iv.into());
    let mut proof = response.body[32..].to_vec();
    cipher.apply_keystream(&mut proof);
    let mut signed = remote.to_vec();
    signed.extend_from_slice(public.as_ref());
    signature::UnparsedPublicKey::new(&signature::ED25519, &setup.body)
        .verify(&signed, &proof)
        .map_err(|_| Error::Authentication)?;
    let mut signed = public.as_ref().to_vec();
    signed.extend_from_slice(remote);
    let mut signature = identity.sign(&signed).as_ref().to_vec();
    cipher.apply_keystream(&mut signature);
    let mut v3 = vec![0; 4];
    v3.extend_from_slice(&signature);
    let response = c
        .request(
            "POST",
            "/pair-verify",
            "application/octet-stream",
            &v3,
            &headers,
        )
        .await?;
    if !response.body.is_empty() {
        return Err(Error::Authentication);
    }
    Ok(mix.then_some(shared))
}
pub(crate) struct StreamKey {
    pub raw: [u8; 16],
    pub wrapped: [u8; 72],
    pub iv: [u8; 16],
}
pub(crate) async fn exchange_key<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
) -> Result<StreamKey, Error> {
    let mut fp = FairPlay::new()?;
    let m2 = c
        .request(
            "POST",
            "/fp-setup",
            "application/octet-stream",
            &FairPlay::message1(),
            &[("X-Apple-ET", "32")],
        )
        .await?;
    let m3 = fp.exchange(&m2.body)?;
    let m4 = c
        .request(
            "POST",
            "/fp-setup",
            "application/octet-stream",
            &m3,
            &[("X-Apple-ET", "32")],
        )
        .await?;
    fp.confirm(&m4.body)?;
    let raw_key = random()?;
    let ekey = fp.wrap(raw_key)?;
    let iv = random()?;
    Ok(StreamKey {
        raw: raw_key,
        wrapped: ekey,
        iv,
    })
}
pub(crate) struct Negotiated {
    pub video_port: u16,
    pub audio_port: Option<u16>,
    pub audio_control_port: Option<u16>,
    pub timing_port: Option<u16>,
    pub event_port: Option<u16>,
    pub key: [u8; 16],
    pub iv: [u8; 16],
    pub video_id: u64,
    pub uri: String,
    pub skip_record: bool,
}
fn port(d: &Dictionary, name: &str) -> Result<Option<u16>, Error> {
    number(d, name)
        .map(|n| {
            u16::try_from(n)
                .ok()
                .filter(|p| *p != 0)
                .ok_or(Error::Protocol)
        })
        .transpose()
}
fn stream_ports(response: &Dictionary, kind: u64) -> Result<(u16, Option<u16>), Error> {
    let streams = response
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    if streams.len() > 8 {
        return Err(Error::Protocol);
    }
    let mut selected = None;
    for s in streams {
        let d = s.as_dictionary().ok_or(Error::Protocol)?;
        if number(d, "type") == Some(kind) {
            if selected.is_some() {
                return Err(Error::Protocol);
            }
            selected = Some((
                port(d, "dataPort")?.ok_or(Error::Protocol)?,
                port(d, "controlPort")?,
            ));
        }
    }
    selected.ok_or(Error::Protocol)
}
pub(crate) struct Setup {
    pub audio: bool,
    pub timing_port: u16,
    pub control_port: u16,
    pub delay_ms: u32,
}
pub(crate) async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    info: &Info,
    endpoint: SocketAddr,
    setup: Setup,
) -> Result<Negotiated, Error> {
    if info.capability.pairing_required {
        return Err(Error::PairingRequired);
    }
    if !info.capability.screen {
        return Err(Error::Unsupported);
    }
    if setup.audio && !info.capability.audio {
        return Err(Error::AudioUnsupported);
    }
    // Do not fall back to a different security protocol after a failed proof.
    let shared = pair(c, info.features).await?;
    let material = exchange_key(c).await?;
    let raw_key = material.raw;
    let ekey = material.wrapped;
    let iv = material.iv;
    let key = if let Some(shared) = shared {
        let mut h = digest::Context::new(&digest::SHA512);
        h.update(&raw_key);
        h.update(&shared);
        h.finish().as_ref()[..16].try_into().expect("digest size")
    } else {
        raw_key
    };
    let video_id = u64::from_be_bytes(random()?) & i64::MAX as u64;
    let audio_id = u64::from_be_bytes(random()?) & i64::MAX as u64;
    let uri = format!("rtsp://{endpoint}/{audio_id}");
    let video_uri = format!("rtsp://{endpoint}/{video_id}");
    let mut device = random::<6>()?;
    device[0] = (device[0] | 2) & 0xfe;
    let id = device
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let mut uuid_bytes = random::<16>()?;
    uuid_bytes[6] = (uuid_bytes[6] & 15) | 0x40;
    uuid_bytes[8] = (uuid_bytes[8] & 63) | 0x80;
    let hex = uuid_bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    let mut root = dict([
        ("deviceID", Value::String(id.clone())),
        ("macAddress", Value::String(id)),
        ("sessionUUID", Value::String(uuid)),
        ("sourceVersion", Value::String("280.33".into())),
        ("model", Value::String("Janus".into())),
        ("name", Value::String(janus_media::identity::sender_name())),
        ("isScreenMirroringSession", Value::Boolean(true)),
        ("timingProtocol", Value::String("NTP".into())),
        ("timingPort", integer(setup.timing_port.into())),
        ("ekey", Value::Data(ekey.to_vec())),
        ("eiv", Value::Data(iv.to_vec())),
        ("et", integer(32)),
    ]);
    let mut accepted = false;
    let result = async {
        let body = wire::encode(&root)?;
        let control = c
            .request(
                "SETUP",
                &uri,
                "application/x-apple-binary-plist",
                &body,
                &[],
            )
            .await
            .map_err(|e| e.response("control SETUP", "malformed RTSP headers or framing"));
        let (legacy, response) = match control {
            Ok(r) => {
                accepted = true;
                // Control SETUP only returns optional timing/event metadata.
                // A successful empty reply creates the session too; media
                // SETUP still requires a plist containing its stream ports.
                let response = if r.body.is_empty() {
                    Dictionary::new()
                } else {
                    wire::decode_response(&r.body, "control SETUP")?
                };
                (false, response)
            }
            Err(Error::Status(400 | 405 | 406 | 415 | 455 | 501)) => (true, Dictionary::new()),
            Err(e) => return Err(e),
        };
        let mut skip_record = response.get("skipRecord").and_then(Value::as_boolean) == Some(true);
        let mut event_port = port(&response, "eventPort")?;
        let mut timing_port = port(&response, "timingPort")?;
        let mut audio_port = None;
        let mut audio_control_port = None;
        if setup.audio {
            let stream = dict([
                ("type", integer(96)),
                ("streamConnectionID", integer(audio_id)),
                ("ct", integer(2)),
                ("spf", integer(352)),
                ("sr", integer(44100)),
                ("audioFormat", integer(0x40000)),
                ("audioMode", Value::String("default".into())),
                ("usingScreen", Value::Boolean(true)),
                ("controlPort", integer(setup.control_port.into())),
                ("latencyMin", integer(0)),
                (
                    "latencyMax",
                    integer(u64::from(setup.delay_ms) * 44100 / 1000),
                ),
            ]);
            let request = stream_request(&root, stream, legacy);
            let r = c.plist("SETUP", &uri, &request, "audio SETUP").await?;
            accepted = true;
            let (data, control) = stream_ports(&r, 96)
                .map_err(|e| e.response("audio SETUP", "missing or invalid audio stream ports"))?;
            audio_port = Some(data);
            audio_control_port = Some(control.ok_or(Error::InvalidResponse {
                stage: "audio SETUP",
                reason: "missing controlPort",
            })?);
            event_port = port(&r, "eventPort")?.or(event_port);
            timing_port = port(&r, "timingPort")?.or(timing_port);
            if legacy {
                skip_record = r.get("skipRecord").and_then(Value::as_boolean) == Some(true);
            }
        }
        // Legacy control is plaintext. Only the wrapped ekey belongs on it;
        // never publish the raw stream key in a shk descriptor. UxPlay's
        // raop_handlers.h unwraps ekey and derives video keys from the ID.
        let video = dict([
            ("type", integer(110)),
            ("streamConnectionID", integer(video_id)),
            ("latencyMs", integer(setup.delay_ms.into())),
            (
                "timestampInfo",
                Value::Array(
                    ["SubSu", "BePxT", "AfPxT", "BefEn", "EmEnc"]
                        .into_iter()
                        .map(|name| dict([("name", Value::String(name.into()))]))
                        .collect(),
                ),
            ),
        ]);
        let request = stream_request(&root, video, legacy && !setup.audio);
        let r = c
            .plist("SETUP", &video_uri, &request, "video SETUP")
            .await?;
        accepted = true;
        let video_port = stream_ports(&r, 110)
            .map_err(|e| e.response("video SETUP", "missing or invalid video stream dataPort"))?
            .0;
        event_port = port(&r, "eventPort")?.or(event_port);
        timing_port = port(&r, "timingPort")?.or(timing_port);
        Ok(Negotiated {
            video_port,
            audio_port,
            audio_control_port,
            timing_port,
            event_port,
            key,
            iv,
            video_id,
            uri: uri.clone(),
            skip_record,
        })
    }
    .await;
    if result.is_err() && (accepted || c.setup_created) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            c.request("TEARDOWN", &uri, "", &[], &[]),
        )
        .await;
    }
    // Root contains session credentials; it is never logged.
    if let Some(d) = root.as_dictionary_mut() {
        d.clear();
    }
    result.map_err(|e| match e {
        Error::Protocol => e.response("SETUP", "invalid eventPort or timingPort"),
        other => other,
    })
}
fn stream_request(root: &Value, stream: Value, include_root: bool) -> Value {
    let mut request = if include_root {
        root.as_dictionary().expect("local dictionary").clone()
    } else {
        Dictionary::new()
    };
    request.insert("streams".into(), Value::Array(vec![stream]));
    Value::Dictionary(request)
}

fn identifiers() -> Result<(String, String), Error> {
    let mut device = random::<6>()?;
    device[0] = (device[0] | 2) & 0xfe;
    let id = device
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let mut u = random::<16>()?;
    u[6] = (u[6] & 15) | 0x40;
    u[8] = (u[8] & 63) | 0x80;
    let hex = u.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    Ok((id, uuid))
}
fn screen_audio(
    id: u64,
    key: &[u8; 32],
    control_port: u16,
    delay_ms: u32,
    connections: bool,
) -> Value {
    let mut stream = plist::Dictionary::new();
    for (k, v) in [
        ("type", integer(96)),
        ("streamConnectionID", integer(id)),
        ("ct", integer(2)),
        ("spf", integer(352)),
        ("sr", integer(44100)),
        ("audioFormat", integer(0x40000)),
        ("audioMode", Value::String("default".into())),
        ("usingScreen", Value::Boolean(true)),
        ("latencyMin", integer(0)),
        ("latencyMax", integer(u64::from(delay_ms) * 44100 / 1000)),
        ("shk", Value::Data(key.to_vec())),
    ] {
        stream.insert(k.into(), v);
    }
    if connections {
        stream.insert("isMedia".into(), Value::Boolean(false));
        stream.insert("supportsDynamicStreamID".into(), Value::Boolean(true));
        stream.insert(
            "streamConnections".into(),
            dict([
                (
                    "streamConnectionTypeRTP",
                    dict([(
                        "streamConnectionKeyUseStreamEncryptionKey",
                        Value::Boolean(true),
                    )]),
                ),
                (
                    "streamConnectionTypeRTCP",
                    dict([("streamConnectionKeyPort", integer(control_port.into()))]),
                ),
            ]),
        );
    } else {
        stream.insert("controlPort".into(), integer(control_port.into()));
    }
    Value::Dictionary(stream)
}

/// HomeKit-paired SETUP on the encrypted control channel, ported from
/// Doubletake 5e2f1cb `setupMirrorSession`: control-first SETUP (one
/// media-first transition on an explicit protocol rejection), the screen-audio
/// stream, then the type-110 video stream carrying shk/shiv. No FairPlay: the
/// screen data key is HKDF(pair-verify secret, video streamConnectionID).
pub(crate) async fn negotiate_hap<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    info: &Info,
    endpoint: SocketAddr,
    setup: Setup,
    shk: [u8; 16],
    shiv: [u8; 16],
    audio_key: [u8; 32],
) -> Result<Negotiated, Error> {
    if info.hap.is_none() {
        return Err(Error::Unsupported);
    }
    if setup.audio && !info.capability.audio {
        return Err(Error::AudioUnsupported);
    }
    let video_id = u64::from_be_bytes(random()?) & i64::MAX as u64;
    let audio_id = u64::from_be_bytes(random()?) & i64::MAX as u64;
    let uri = format!("rtsp://{endpoint}/{audio_id}");
    let video_uri = format!("rtsp://{endpoint}/{video_id}");
    let (id, uuid) = identifiers()?;
    let root = dict([
        ("deviceID", Value::String(id.clone())),
        ("macAddress", Value::String(id)),
        ("sessionUUID", Value::String(uuid)),
        ("sourceVersion", Value::String("980.71.1".into())),
        ("isScreenMirroringSession", Value::Boolean(true)),
        ("timingProtocol", Value::String("NTP".into())),
        ("timingPort", integer(setup.timing_port.into())),
        ("osBuildVersion", Value::String("13F69".into())),
        ("model", Value::String("Linux".into())),
        ("name", Value::String(janus_media::identity::sender_name())),
    ]);
    let mut control = root.as_dictionary().expect("local dictionary").clone();
    control.insert("updateSessionRequest".into(), Value::Boolean(false));
    control.insert(
        "combinedGetInfoWithControlSetup".into(),
        Value::Boolean(true),
    );
    let connections = info.features & (1 << 59) != 0;
    let mut accepted = false;
    let result = async {
        let body = wire::encode(&Value::Dictionary(control))?;
        let first = c
            .request(
                "SETUP",
                &uri,
                "application/x-apple-binary-plist",
                &body,
                &[],
            )
            .await
            .map_err(|e| e.response("control SETUP", "malformed RTSP headers or framing"));
        let (legacy, response) = match first {
            Ok(r) => {
                accepted = true;
                let response = if r.body.is_empty() {
                    Dictionary::new()
                } else {
                    wire::decode_response(&r.body, "control SETUP")?
                };
                (false, response)
            }
            Err(Error::Status(400 | 405 | 406 | 415 | 455 | 501)) => (true, Dictionary::new()),
            Err(e) => return Err(e),
        };
        let mut skip_record = response.get("skipRecord").and_then(Value::as_boolean) == Some(true);
        let mut event_port = port(&response, "eventPort")?;
        let mut timing_port = port(&response, "timingPort")?;
        let audio = |connections| {
            stream_request(
                &root,
                screen_audio(
                    audio_id,
                    &audio_key,
                    setup.control_port,
                    setup.delay_ms,
                    connections,
                ),
                legacy,
            )
        };
        // One alternate descriptor shape on an explicit shape rejection.
        let r = match c
            .plist("SETUP", &uri, &audio(connections), "audio SETUP")
            .await
        {
            Err(Error::Status(400 | 406 | 415 | 455 | 500 | 501)) => {
                c.plist("SETUP", &uri, &audio(!connections), "audio SETUP")
                    .await?
            }
            other => other?,
        };
        accepted = true;
        event_port = port(&r, "eventPort")?.or(event_port);
        timing_port = port(&r, "timingPort")?.or(timing_port);
        if legacy {
            skip_record = r.get("skipRecord").and_then(Value::as_boolean) == Some(true);
        }
        // The screen-audio stream is always set up (as the reference sender
        // does); its ports matter only when Sound is on.
        let (audio_port, audio_control_port) = if setup.audio {
            let (data, control) = stream_ports(&r, 96)
                .map_err(|e| e.response("audio SETUP", "missing or invalid audio stream ports"))?;
            (
                Some(data),
                Some(control.ok_or(Error::InvalidResponse {
                    stage: "audio SETUP",
                    reason: "missing controlPort",
                })?),
            )
        } else {
            (None, None)
        };
        let video = dict([
            ("type", integer(110)),
            ("streamConnectionID", integer(video_id)),
            ("latencyMs", integer(setup.delay_ms.into())),
            (
                "timestampInfo",
                Value::Array(
                    ["SubSu", "BePxT", "AfPxT", "BefEn", "EmEnc"]
                        .into_iter()
                        .map(|name| dict([("name", Value::String(name.into()))]))
                        .collect(),
                ),
            ),
            ("shk", Value::Data(shk.to_vec())),
            ("shiv", Value::Data(shiv.to_vec())),
        ]);
        let r = c
            .plist(
                "SETUP",
                &video_uri,
                &stream_request(&root, video, false),
                "video SETUP",
            )
            .await?;
        let video_port = stream_ports(&r, 110)
            .map_err(|e| e.response("video SETUP", "missing or invalid video stream dataPort"))?
            .0;
        event_port = port(&r, "eventPort")?.or(event_port);
        timing_port = port(&r, "timingPort")?.or(timing_port);
        Ok(Negotiated {
            video_port,
            audio_port,
            audio_control_port,
            timing_port,
            event_port,
            key: shk,
            iv: shiv,
            video_id,
            uri: uri.clone(),
            skip_record,
        })
    }
    .await;
    if result.is_err() && (accepted || c.setup_created) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            c.request("TEARDOWN", &uri, "", &[], &[]),
        )
        .await;
    }
    result.map_err(|e| match e {
        Error::Protocol => e.response("SETUP", "invalid eventPort or timingPort"),
        other => other,
    })
}
