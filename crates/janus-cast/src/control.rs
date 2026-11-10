//! Receiver control messages: virtual connections, heartbeat, status and
//! app-availability queries, plus LAUNCH/STOP of the mirroring app, which
//! `session` sends only on an explicit user Cast/Stop. No load or volume
//! mutation. A transport must complete device trust first. Field formats
//! follow Chromium `cast_message_util.cc`.
use crate::wire::{
    self, CONNECTION_NAMESPACE, HEARTBEAT_NAMESPACE, Message, Payload, RECEIVER,
    RECEIVER_NAMESPACE, SENDER,
};
use serde::Deserialize;
use thiserror::Error;

pub const MAX_APPLICATIONS: usize = 8;
const MAX_NAMESPACES: usize = 32;
const MAX_AVAILABILITY_ENTRIES: usize = 8;
/// Receiver app that renders Cast Streaming screen mirroring (audio+video).
pub const SCREEN_MIRRORING_APP: &str = "0F5096E8";
/// Receiver app for audio-only Cast Streaming mirroring.
pub const AUDIO_MIRRORING_APP: &str = "85CDB22F";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid receiver control message")]
    Invalid,
    #[error("invalid receiver message route")]
    Route,
    #[error("receiver response request mismatch")]
    Correlation,
    #[error("invalid receiver JSON")]
    Json(#[from] serde_json::Error),
    #[error("invalid receiver wire framing")]
    Wire(#[from] wire::Error),
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Application {
    pub app_id: String,
    pub session_id: String,
    pub transport_id: String,
    #[serde(default)]
    pub namespaces: Vec<Namespace>,
}

#[derive(Clone, Deserialize)]
pub struct Namespace {
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Volume {
    pub level: Option<f64>,
    pub muted: Option<bool>,
}

#[derive(Clone, Deserialize)]
pub struct ReceiverStatus {
    #[serde(default)]
    pub applications: Vec<Application>,
    pub volume: Option<Volume>,
}

impl std::fmt::Debug for ReceiverStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiverStatus")
            .field("application_count", &self.applications.len())
            .finish_non_exhaustive()
    }
}

impl ReceiverStatus {
    /// Observation only: an existing application is never preempted implicitly.
    pub fn has_active_application(&self) -> bool {
        !self.applications.is_empty()
    }
}

/// A receiver's answer for one app ID in GET_APP_AVAILABILITY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAvailability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug)]
pub enum Event {
    Ping,
    Pong,
    Status {
        request_id: Option<u32>,
        status: ReceiverStatus,
    },
    /// The receiver refused a LAUNCH (`LAUNCH_ERROR` or `INVALID_REQUEST`).
    LaunchError {
        request_id: Option<u32>,
        reason: String,
    },
    /// Correlated by request ID only; the reply carries no `type` field.
    AppAvailability {
        request_id: u32,
        apps: Vec<(String, AppAvailability)>,
    },
    Closed,
    Other,
}

#[derive(Deserialize)]
struct JsonMessage {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "responseType")]
    response_type: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<u32>,
    status: Option<ReceiverStatus>,
    availability: Option<std::collections::BTreeMap<String, String>>,
    reason: Option<String>,
}

/// Virtual connection to an app transport (e.g. the mirroring app).
pub fn connect_to(destination: &str) -> Result<Message, Error> {
    let mut message = connect()?;
    message.destination = destination.into();
    Message::new(
        &message.source,
        destination,
        &message.namespace,
        message.payload,
    )
    .map_err(Into::into)
}

/// Closes a virtual connection to an app transport.
pub fn close_to(destination: &str) -> Result<Message, Error> {
    let message = close()?;
    Message::new(
        &message.source,
        destination,
        &message.namespace,
        message.payload,
    )
    .map_err(Into::into)
}

/// Starts `app_id` on the receiver. Sent only on explicit user Cast.
pub fn launch(request_id: u32, app_id: &str) -> Result<Message, Error> {
    if request_id == 0 || app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::Invalid);
    }
    let body = serde_json::json!({"type": "LAUNCH", "appId": app_id, "requestId": request_id});
    Ok(Message::new(
        SENDER,
        RECEIVER,
        RECEIVER_NAMESPACE,
        Payload::Text(body.to_string()),
    )?)
}

/// Stops the app session Janus launched. Sent only on explicit Stop.
pub fn stop(request_id: u32, session_id: &str) -> Result<Message, Error> {
    if request_id == 0 || session_id.is_empty() || session_id.len() > 128 {
        return Err(Error::Invalid);
    }
    let body =
        serde_json::json!({"type": "STOP", "sessionId": session_id, "requestId": request_id});
    Ok(Message::new(
        SENDER,
        RECEIVER,
        RECEIVER_NAMESPACE,
        Payload::Text(body.to_string()),
    )?)
}

/// Opens the platform virtual connection (Chromium CreateVirtualConnectionRequest,
/// strong connection to receiver-0). Read-only: launches nothing.
pub fn connect() -> Result<Message, Error> {
    let body = serde_json::json!({
        "type": "CONNECT",
        "userAgent": janus_media::identity::sender_name(),
        "connType": 0,
        "origin": {},
        "senderInfo": {
            "sdkType": 2,
            "version": env!("CARGO_PKG_VERSION"),
            "browserVersion": env!("CARGO_PKG_VERSION"),
            "platform": 4,
            "connectionType": 1,
        },
    });
    Ok(Message::new(
        SENDER,
        RECEIVER,
        CONNECTION_NAMESPACE,
        Payload::Text(body.to_string()),
    )?)
}

/// Closes the platform virtual connection before the channel is dropped.
pub fn close() -> Result<Message, Error> {
    Ok(Message::new(
        SENDER,
        RECEIVER,
        CONNECTION_NAMESPACE,
        Payload::Text("{\"type\":\"CLOSE\",\"reasonCode\":5}".into()),
    )?)
}

pub fn ping() -> Result<Message, Error> {
    Ok(Message::new(
        SENDER,
        RECEIVER,
        HEARTBEAT_NAMESPACE,
        Payload::Text("{\"type\":\"PING\"}".into()),
    )?)
}

/// Asks whether the receiver can run each app, without launching any.
pub fn get_app_availability(request_id: u32, app_ids: &[&str]) -> Result<Message, Error> {
    if request_id == 0 || app_ids.is_empty() || app_ids.len() > MAX_AVAILABILITY_ENTRIES {
        return Err(Error::Invalid);
    }
    let body = serde_json::json!({
        "type": "GET_APP_AVAILABILITY",
        "appId": app_ids,
        "requestId": request_id,
    });
    Ok(Message::new(
        SENDER,
        RECEIVER,
        RECEIVER_NAMESPACE,
        Payload::Text(body.to_string()),
    )?)
}

pub fn get_status(request_id: u32) -> Result<Message, Error> {
    if request_id == 0 {
        return Err(Error::Invalid);
    }
    Ok(Message::new(
        SENDER,
        RECEIVER,
        RECEIVER_NAMESPACE,
        Payload::Text(serde_json::json!({"type":"GET_STATUS", "requestId":request_id}).to_string()),
    )?)
}

pub fn pong() -> Result<Message, Error> {
    Ok(Message::new(
        SENDER,
        RECEIVER,
        HEARTBEAT_NAMESPACE,
        Payload::Text("{\"type\":\"PONG\"}".into()),
    )?)
}

pub fn parse(message: &Message, expected_request: Option<u32>) -> Result<Event, Error> {
    if message.source != RECEIVER || (message.destination != SENDER && message.destination != "*") {
        return Err(Error::Route);
    }
    let known = matches!(
        message.namespace.as_str(),
        HEARTBEAT_NAMESPACE | RECEIVER_NAMESPACE | CONNECTION_NAMESPACE
    );
    if !known {
        return Ok(Event::Other);
    }
    let Payload::Text(text) = &message.payload else {
        return Err(Error::Invalid);
    };
    if text.is_empty() || text.len() > wire::MAX_FRAME_BYTES {
        return Err(Error::Invalid);
    }
    let body: JsonMessage = serde_json::from_str(text)?;
    if message.namespace == RECEIVER_NAMESPACE
        && body.kind.is_none()
        && body.response_type.as_deref() == Some("GET_APP_AVAILABILITY")
    {
        return parse_availability(body, expected_request);
    }
    let Some(kind) = body.kind.as_deref() else {
        return Ok(Event::Other);
    };
    match (message.namespace.as_str(), kind) {
        (HEARTBEAT_NAMESPACE, "PING") => Ok(Event::Ping),
        (HEARTBEAT_NAMESPACE, "PONG") => Ok(Event::Pong),
        (CONNECTION_NAMESPACE, "CLOSE") => Ok(Event::Closed),
        (RECEIVER_NAMESPACE, "LAUNCH_ERROR" | "INVALID_REQUEST") => Ok(Event::LaunchError {
            request_id: body.request_id,
            reason: body
                .reason
                .unwrap_or_else(|| kind.to_owned())
                .chars()
                .take(64)
                .collect(),
        }),
        (RECEIVER_NAMESPACE, "RECEIVER_STATUS") => {
            if expected_request.is_some() && body.request_id != expected_request {
                return Err(Error::Correlation);
            }
            let status = body.status.ok_or(Error::Invalid)?;
            if status.applications.len() > MAX_APPLICATIONS {
                return Err(Error::Invalid);
            }
            let valid_id = |id: &str| {
                !id.is_empty() && id.len() <= 128 && id.bytes().all(|byte| byte.is_ascii_graphic())
            };
            for app in &status.applications {
                if !valid_id(&app.app_id)
                    || !valid_id(&app.session_id)
                    || !valid_id(&app.transport_id)
                    || app.namespaces.len() > MAX_NAMESPACES
                    || app.namespaces.iter().any(|namespace| {
                        !namespace.name.starts_with("urn:x-cast:") || !valid_id(&namespace.name)
                    })
                {
                    return Err(Error::Invalid);
                }
            }
            if status.volume.is_some_and(|volume| {
                volume
                    .level
                    .is_some_and(|level| !level.is_finite() || !(0.0..=1.0).contains(&level))
            }) {
                return Err(Error::Invalid);
            }
            Ok(Event::Status {
                request_id: body.request_id,
                status,
            })
        }
        _ => Ok(Event::Other),
    }
}

fn parse_availability(body: JsonMessage, expected_request: Option<u32>) -> Result<Event, Error> {
    let request_id = body.request_id.ok_or(Error::Invalid)?;
    if expected_request.is_some_and(|expected| expected != request_id) {
        return Err(Error::Correlation);
    }
    let entries = body.availability.ok_or(Error::Invalid)?;
    if entries.len() > MAX_AVAILABILITY_ENTRIES {
        return Err(Error::Invalid);
    }
    let apps = entries
        .into_iter()
        .map(|(app, state)| {
            if app.is_empty() || app.len() > 32 || !app.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err(Error::Invalid);
            }
            let state = match state.as_str() {
                "APP_AVAILABLE" => AppAvailability::Available,
                "APP_UNAVAILABLE" => AppAvailability::Unavailable,
                _ => AppAvailability::Unknown,
            };
            Ok((app, state))
        })
        .collect::<Result<_, _>>()?;
    Ok(Event::AppAvailability { request_id, apps })
}
