//! Bounded sequential RTSP/HTTP and binary plist. A partial exchange poisons
//! its connection: no late response can be mistaken for another request.
use crate::Error;
use plist::{
    Dictionary, Value,
    stream::{Event, Reader},
};
use std::{collections::BTreeMap, io::Cursor, net::SocketAddr, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const MAX_BODY: usize = 64 * 1024;
pub(crate) const DEADLINE: Duration = Duration::from_secs(5);
pub(crate) struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub(crate) struct Connection<S> {
    pub stream: S,
    pub(crate) seq: u32,
    usable: bool,
    pub(crate) session: Option<String>,
    pub setup_created: bool,
    http: Option<SocketAddr>,
    agent: &'static str,
}
impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            seq: 0,
            usable: true,
            session: None,
            setup_created: false,
            http: None,
            agent: "280.33",
        }
    }
    /// RTSP `User-Agent: AirPlay/<version>` for this connection.
    pub(crate) fn set_agent(&mut self, agent: &'static str) {
        self.agent = agent;
    }
    /// Wraps the byte stream (e.g. to install channel encryption) while
    /// keeping CSeq, session and poisoning state.
    pub(crate) fn map_stream<T>(self, f: impl FnOnce(S) -> T) -> Connection<T> {
        Connection {
            stream: f(self.stream),
            seq: self.seq,
            usable: self.usable,
            session: self.session,
            setup_created: self.setup_created,
            http: self.http,
            agent: self.agent,
        }
    }
    pub(crate) fn http(stream: S, authority: SocketAddr) -> Self {
        Self {
            http: Some(authority),
            ..Self::new(stream)
        }
    }
    pub(crate) async fn request(
        &mut self,
        method: &str,
        path: &str,
        kind: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<Response, Error> {
        self.request_within(method, path, kind, body, extra, DEADLINE)
            .await
    }
    /// As [`Self::request`] with an explicit deadline, for receiver steps that
    /// wait on the user or the screen (e.g. showing a pairing code).
    pub(crate) async fn request_within(
        &mut self,
        method: &str,
        path: &str,
        kind: &str,
        body: &[u8],
        extra: &[(&str, &str)],
        deadline: Duration,
    ) -> Result<Response, Error> {
        if !self.usable
            || body.len() > MAX_BODY
            || [method, path, kind]
                .iter()
                .any(|s| s.contains(['\r', '\n']))
        {
            return Err(Error::Protocol);
        }
        self.seq = self.seq.checked_add(1).ok_or(Error::Protocol)?;
        let (protocol, agent) = if self.http.is_some() {
            ("HTTP/1.1", "200.54")
        } else {
            ("RTSP/1.0", self.agent)
        };
        let mut header = format!(
            "{method} {path} {protocol}\r\nCSeq: {}\r\nUser-Agent: AirPlay/{agent}\r\nContent-Length: {}\r\n",
            self.seq,
            body.len()
        );
        if let Some(authority) = self.http {
            header.push_str(&format!("Host: {authority}\r\n"));
        }
        if !kind.is_empty() && !body.is_empty() {
            header.push_str(&format!("Content-Type: {kind}\r\n"));
        }
        if let Some(session) = &self.session {
            header.push_str(&format!("Session: {session}\r\n"));
        }
        for (key, value) in extra {
            if key.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(Error::Protocol);
            }
            header.push_str(&format!("{key}: {value}\r\n"));
        }
        header.push_str("\r\n");
        self.usable = false;
        let result = tokio::time::timeout(deadline, async {
            self.stream.write_all(header.as_bytes()).await?;
            self.stream.write_all(body).await?;
            self.stream.flush().await?;
            read_response(&mut self.stream, self.seq).await
        })
        .await
        .map_err(|_| Error::RequestTimeout {
            stage: response_stage(method, path),
        })?;
        let response = result.map_err(|e| {
            e.response(
                response_stage(method, path),
                "malformed RTSP headers or framing",
            )
        })?;
        self.usable = true;
        if method == "SETUP" && (200..=299).contains(&response.status) {
            self.setup_created = true;
        }
        if let Some(session) = response.headers.get("session") {
            let session = session.split(';').next().ok_or(Error::Protocol)?;
            if session.len() > 128
                || session.is_empty()
                || !session.bytes().all(|b| b.is_ascii_graphic())
            {
                self.usable = false;
                return Err(Error::InvalidResponse {
                    stage: response_stage(method, path),
                    reason: "invalid Session header",
                });
            }
            self.session = Some(session.to_owned());
        }
        match response.status {
            200..=299 => Ok(response),
            401 | 403 | 470 => Err(Error::PairingRequired),
            code => Err(Error::Status(code)),
        }
    }
    pub(crate) async fn plist(
        &mut self,
        method: &str,
        path: &str,
        value: &Value,
        stage: &'static str,
    ) -> Result<Dictionary, Error> {
        let body = encode(value)?;
        let r = self
            .request(method, path, "application/x-apple-binary-plist", &body, &[])
            .await
            .map_err(|e| e.response(stage, "malformed RTSP headers or framing"))?;
        decode_response(&r.body, stage)
    }
    /// `/stream` changes this HTTP connection into the media byte stream.
    /// The historical receiver sends no HTTP response; ownership prevents reuse.
    pub(crate) async fn into_mirroring(mut self, body: &[u8], device: u64) -> Result<S, Error> {
        let authority = self.http.ok_or(Error::Protocol)?;
        if !self.usable || body.len() > MAX_BODY || device > 0xffffffffffff {
            return Err(Error::Protocol);
        }
        self.usable = false;
        let header = format!(
            "POST /stream HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: AirPlay/200.54\r\nX-Apple-Device-ID: 0x{device:012x}\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        tokio::time::timeout(DEADLINE, async {
            self.stream.write_all(header.as_bytes()).await?;
            self.stream.write_all(body).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| Error::RequestTimeout {
            stage: "HTTP /stream upload",
        })??;
        Ok(self.stream)
    }
}
// Only local, fixed labels leave this layer. RTSP paths may contain session IDs.
fn response_stage(method: &str, path: &str) -> &'static str {
    match (method, path) {
        ("GET", "/info") => "GET /info",
        ("GET", "/stream.xml") => "HTTP /stream.xml",
        ("POST", "/pair-setup") => "pair-setup",
        ("POST", "/pair-verify") => "pair-verify",
        ("POST", "/pair-pin-start") => "pair-pin-start",
        ("POST", "/fp-setup") => "FairPlay setup",
        ("SETUP", _) => "SETUP",
        ("RECORD", _) => "RECORD",
        ("POST", "/feedback") => "feedback",
        ("TEARDOWN", _) => "TEARDOWN",
        _ => "RTSP exchange",
    }
}
pub(crate) async fn read_response<S: AsyncRead + Unpin>(
    stream: &mut S,
    seq: u32,
) -> Result<Response, Error> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err(Error::Protocol);
        }
        header.push(stream.read_u8().await?);
    }
    let text = std::str::from_utf8(&header).map_err(|_| Error::Protocol)?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or(Error::Protocol)?;
    let mut parts = status.split_whitespace();
    if !matches!(parts.next(), Some("RTSP/1.0" | "HTTP/1.0" | "HTTP/1.1")) {
        return Err(Error::Protocol);
    }
    let status = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|n| (100..=599).contains(n))
        .ok_or(Error::Protocol)?;
    let mut headers = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        if headers.len() >= 48 {
            return Err(Error::Protocol);
        }
        let (k, v) = line.split_once(':').ok_or(Error::Protocol)?;
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(Error::Protocol);
        }
        if headers
            .insert(k.to_ascii_lowercase(), v.trim().to_owned())
            .is_some()
        {
            return Err(Error::Protocol);
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err(Error::Protocol);
    }
    if let Some(cseq) = headers.get("cseq")
        && cseq.parse::<u32>().ok() != Some(seq)
    {
        return Err(Error::Protocol);
    }
    let len = headers
        .get("content-length")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| Error::Protocol)?
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(Error::Protocol);
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body).await?;
    Ok(Response {
        status,
        headers,
        body,
    })
}
pub(crate) fn encode(value: &Value) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    value
        .to_writer_binary(&mut bytes)
        .map_err(|_| Error::Protocol)?;
    if bytes.len() > MAX_BODY {
        return Err(Error::Protocol);
    }
    Ok(bytes)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Dictionary, Error> {
    if bytes.len() > MAX_BODY || !bytes.starts_with(b"bplist00") {
        return Err(Error::Protocol);
    }
    decode_events(bytes)
}
pub(crate) fn decode_xml(bytes: &[u8]) -> Result<Dictionary, Error> {
    if bytes.len() > MAX_BODY {
        return Err(Error::Protocol);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Protocol)?
        .trim();
    if !(text.starts_with("<?xml") || text.starts_with("<plist")) {
        return Err(Error::Protocol);
    }
    decode_events(text.as_bytes())
}
fn decode_events(bytes: &[u8]) -> Result<Dictionary, Error> {
    let mut events = Vec::new();
    let mut depth = 0usize;
    let mut scalar_bytes = 0usize;
    for event in Reader::new(Cursor::new(bytes)) {
        let event = event.map_err(|_| Error::Protocol)?;
        match &event {
            Event::StartArray(n) | Event::StartDictionary(n) => {
                if n.is_some_and(|n| n > 256) {
                    return Err(Error::Protocol);
                }
                depth += 1;
            }
            Event::EndCollection => {
                depth = depth.checked_sub(1).ok_or(Error::Protocol)?;
            }
            Event::Data(v) => scalar_bytes += v.len(),
            Event::String(v) => scalar_bytes += v.len(),
            _ => {}
        }
        if depth > 8 || events.len() >= 1024 || scalar_bytes > MAX_BODY {
            return Err(Error::Protocol);
        }
        events.push(Ok(event));
    }
    Value::from_events(events)
        .map_err(|_| Error::Protocol)?
        .into_dictionary()
        .ok_or(Error::Protocol)
}
pub(crate) fn decode_response(bytes: &[u8], stage: &'static str) -> Result<Dictionary, Error> {
    let reason = if bytes.is_empty() {
        "empty plist response body"
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<plist") {
        "XML plist response is not supported yet"
    } else if !bytes.starts_with(b"bplist00") {
        "response body is not a binary plist"
    } else {
        "malformed or over-limit binary plist body"
    };
    decode(bytes).map_err(|e| e.response(stage, reason))
}
pub(crate) fn dict(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
pub(crate) fn integer(n: u64) -> Value {
    Value::Integer(n.into())
}
pub(crate) fn number(d: &Dictionary, key: &str) -> Option<u64> {
    d.get(key).and_then(|v| {
        v.as_unsigned_integer()
            .or_else(|| v.as_signed_integer().map(|n| n as u64))
    })
}

#[doc(hidden)]
pub fn validate_plist(bytes: &[u8]) -> bool {
    decode(bytes).is_ok()
}
