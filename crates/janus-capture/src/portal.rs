//! xdg-desktop-portal ScreenCast: shows the system picker for a monitor or a
//! window and returns the PipeWire remote and node to read. A restore token
//! (persist mode "until revoked") lets a later cast reuse the same choice
//! without the dialog when the desktop allows it.
use crate::{CaptureError, SourceKind};
use ashpd::desktop::{
    PersistMode, Session,
    screencast::{CursorMode, Screencast, SourceType},
};
use std::os::fd::OwnedFd;

/// The chosen source. Keep it alive while capturing; dropping it (or
/// [`PortalCapture::close`]) ends the portal session.
pub struct PortalCapture {
    pub fd: OwnedFd,
    pub node_id: u32,
    pub size: Option<(i32, i32)>,
    pub restore_token: Option<String>,
    session: Session<'static, Screencast<'static>>,
    _proxy: Screencast<'static>,
}

impl PortalCapture {
    pub async fn close(self) {
        let _ = self.session.close().await;
    }
}

fn unavailable(error: ashpd::Error) -> CaptureError {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => CaptureError::Cancelled,
        other => CaptureError::Unavailable(other.to_string()),
    }
}

/// Asks the user (system dialog) for a source of `kind`.
pub async fn select(
    kind: SourceKind,
    restore_token: Option<&str>,
) -> Result<PortalCapture, CaptureError> {
    let proxy = Screencast::new().await.map_err(unavailable)?;
    let session = proxy.create_session().await.map_err(unavailable)?;
    let cursor = match proxy.available_cursor_modes().await {
        Ok(modes) if modes.contains(CursorMode::Embedded) => CursorMode::Embedded,
        _ => CursorMode::Hidden,
    };
    let types = match kind {
        SourceKind::Screen => SourceType::Monitor.into(),
        SourceKind::Window => SourceType::Window.into(),
    };
    proxy
        .select_sources(
            &session,
            cursor,
            types,
            false,
            restore_token,
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(unavailable)?
        .response()
        .map_err(unavailable)?;
    let streams = proxy
        .start(&session, None)
        .await
        .map_err(unavailable)?
        .response()
        .map_err(unavailable)?;
    let stream = streams
        .streams()
        .first()
        .ok_or_else(|| CaptureError::Unavailable("no stream selected".into()))?;
    let node_id = stream.pipe_wire_node_id();
    let size = stream.size();
    let restore_token = streams.restore_token().map(str::to_owned);
    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(unavailable)?;
    Ok(PortalCapture {
        fd,
        node_id,
        size,
        restore_token,
        session,
        _proxy: proxy,
    })
}
