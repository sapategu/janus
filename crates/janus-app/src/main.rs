//! Native Janus destination picker. Real DNS-SD results are the default.
//! Explicit --design-preview enables visual fixtures without networking.
use gpui::{
    Animation, AnimationExt, App, Application, Bounds, BoxShadow, Context, Decorations,
    FocusHandle, FontWeight, Image, ImageFormat, IntoElement, KeyBinding, MouseButton, Render,
    ResizeEdge, SharedString, Tiling, Window, WindowBounds, WindowDecorations, WindowOptions,
    actions, div, ease_out_quint, hsla, img, point, prelude::*, px, rgb, size,
};
use janus_cast::{
    session::{Capability, Failure, Phase, Session, SessionConfig, Snapshot},
    trust::{DevicePolicy, Error as TrustError},
};
use janus_discovery::{
    BrowseFailure, BrowsePhase, Browser, Candidate, DiscoverySnapshot, OutputKind, ReceiverProtocol,
};
use janus_mirror::{Casting, MirrorStats, Options as MirrorOptions, Source as MirrorSource};

const BACKGROUND: u32 = 0x171818;
const SURFACE: u32 = 0x222323;
const RAISED: u32 = 0x454747;
const TEXT: u32 = 0xf5f6f5;
const MUTED: u32 = 0xa0a5a3;
const MINT: u32 = 0xc6f8ed;
const LINE: u32 = 0x393c3b;
/// Recessed track behind segmented controls.
const TRACK: u32 = 0x1c1d1d;
/// Hairline window outline.
const OUTLINE: u32 = 0x2e3130;
/// Soft error text that stays legible on charcoal.
const ERROR: u32 = 0xffb4a8;
/// Window corner radius and the transparent margin around a client-side
/// window that carries its shadow and resize handles.
const WINDOW_RADIUS: f32 = 14.;
const WINDOW_INSET: f32 = 12.;

actions!(janus, [Stop, FocusNext, FocusPrevious, PageUp, PageDown]);

#[derive(Clone, Copy)]
struct ReceiverChoices {
    window: bool,
    audio: bool,
    resolution: janus_mirror::Resolution,
}
impl Default for ReceiverChoices {
    fn default() -> Self {
        Self {
            window: false,
            audio: true,
            resolution: janus_mirror::Resolution::default(),
        }
    }
}

impl ReceiverChoices {
    fn for_receiver(device: &Candidate) -> Self {
        Self {
            audio: device.advertised_outputs.audio != Some(false),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Cast,
    Receive,
    Settings,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fixture {
    Ready,
    Empty,
    Busy,
    PermissionDenied,
    Active,
    Recovering,
    Error,
}

#[derive(Clone, Default)]
enum ConnectionCheck {
    #[default]
    Idle,
    Live(Snapshot),
    AirPlayChecking,
    AirPlay(janus_airplay::Capability),
    AirPlayFailed(String),
}

impl ConnectionCheck {
    fn active(&self) -> bool {
        matches!(self, Self::AirPlayChecking)
            || matches!(self, Self::Live(snapshot)
            if !matches!(snapshot.phase, Phase::Failed(_) | Phase::Closed))
    }

    fn label(&self) -> &str {
        let snapshot = match self {
            Self::Live(snapshot) => snapshot,
            Self::AirPlayChecking => return "Checking AirPlay…",
            Self::AirPlay(info) if info.profile == janus_airplay::Profile::Hisense => {
                return "Hisense interactive display ready · Screen and sound.";
            }
            Self::AirPlay(info) if info.pairing_required && info.pin_pairing => {
                return "Pair once with the code the TV shows: press Cast.";
            }
            Self::AirPlay(info) if info.pairing_required => {
                return "This receiver needs a password; Janus doesn't support that yet.";
            }
            Self::AirPlay(info) if !info.screen => {
                return "This receiver doesn't advertise screen mirroring.";
            }
            Self::AirPlay(info) if !info.audio => {
                return "AirPlay screen mirroring advertised · Use Sound off.";
            }
            Self::AirPlay(_) => {
                return "AirPlay screen and ALAC audio advertised · Cast tests the connection.";
            }
            Self::AirPlayFailed(reason) => return reason,
            Self::Idle => return "Connect to the selected TV without starting a cast.",
        };
        match snapshot.phase {
            Phase::Connecting => "Connecting…",
            Phase::Reconnecting { .. } => "Connection lost · Reconnecting…",
            Phase::Closed => "Disconnected.",
            Phase::Connected => match (snapshot.policy, snapshot.screen_mirroring) {
                (Some(DevicePolicy::AudioOnly), _) => {
                    "Connected · Audio-only device, it can't show a screen"
                }
                (_, Capability::Available) => "Connected · This TV supports screen sharing",
                (_, Capability::Unavailable) => "Connected · This TV doesn't offer screen sharing",
                (_, Capability::Unknown) => "Connected · Checking what this TV supports…",
            },
            Phase::Failed(failure) => match failure {
                Failure::NoAddress => "No usable TV address. Refresh screens and try again.",
                Failure::NetworkDenied => "Local network access was denied.",
                Failure::Unreachable => "Couldn't reach the TV. Keep it awake on the same network.",
                Failure::TlsRejected => "TV TLS check failed. Check the TV and computer time.",
                Failure::AuthenticationRejected => "The TV's authentication response was rejected.",
                Failure::Untrusted(TrustError::Revoked) => {
                    "This TV's device certificate has been revoked."
                }
                Failure::Untrusted(TrustError::RevocationUnavailable) => {
                    "Janus's revocation data is out of date. Update Janus."
                }
                Failure::Untrusted(TrustError::Date) => {
                    "Couldn't verify the TV. Check the computer's date and time."
                }
                Failure::Untrusted(_) => "The TV couldn't prove it's a genuine Cast device.",
                Failure::Lost(_) => "The TV stopped responding. Check it and try again.",
            },
        }
    }
}

/// The real (non-fixture) cast shown on the Cast page.
#[derive(Clone)]
enum LiveCast {
    Idle,
    Starting { name: String },
    Live { name: String, stats: MirrorStats },
    Ended(String),
}

/// One-time AirPlay PIN pairing, entered when a cast reports "not paired".
#[derive(Clone, PartialEq, Eq)]
enum Pairing {
    Idle,
    /// Waiting for the TV to show its code.
    Starting {
        name: String,
    },
    Code {
        name: String,
        digits: String,
        error: Option<String>,
    },
    Finishing {
        name: String,
    },
}
const PIN_DIGITS: usize = 4;

enum StartFailure {
    NotPaired,
    Message(String),
}

struct Janus {
    focus: FocusHandle,
    logo: std::sync::Arc<Image>,
    receiver_choices: std::collections::HashMap<String, ReceiverChoices>,
    hovered_button: Option<SharedString>,
    hovered_receiver: Option<String>,
    scroll: gpui::ScrollHandle,
    page: Page,
    fixture: Fixture,
    selected: usize,
    source: usize,
    audio: bool,
    receiving: bool,
    muted: bool,
    design_preview: bool,
    smoke_pending: bool,
    /// Janus never maximizes (user decision); set while undoing one.
    unmaximizing: bool,
    discovery: DiscoverySnapshot,
    browser: Option<Browser>,
    selected_device: Option<String>,
    details_device: Option<Candidate>,
    details_failure: Option<BrowseFailure>,
    runtime: tokio::runtime::Handle,
    connection_check: ConnectionCheck,
    connection_session: Option<Session>,
    connection_probe: Option<tokio::task::AbortHandle>,
    connection_generation: u64,
    live_cast: LiveCast,
    cast_handles: Option<Casting>,
    cast_start: Option<tokio::task::AbortHandle>,
    cast_generation: u64,
    pairing: Pairing,
    pin_pairing: Option<janus_airplay::PinPairing>,
    pairing_device: Option<Candidate>,
    pairing_generation: u64,
}

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(16.))
        .bg(rgb(SURFACE))
}

/// Recessed pill that holds a segmented control.
fn track(height: f32) -> gpui::Div {
    div()
        .flex_shrink_0()
        .h(px(height))
        .p(px(4.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded_full()
        .bg(rgb(TRACK))
        .border_1()
        .border_color(rgb(OUTLINE))
}

/// One labelled row of the Settings card.
fn setting_row(label: &'static str, value: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(px(24.))
        .py(px(14.))
        .border_t_1()
        .border_color(rgb(LINE))
        .child(div().w(px(200.)).flex_shrink_0().child(label))
        .child(div().flex_1().min_w(px(0.)).child(caption(value)))
}

fn caption(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}

fn title(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div().text_size(px(18.)).child(text.into())
}

fn monitor(accent: bool) -> gpui::Div {
    div()
        .flex()
        .flex_shrink_0()
        .flex_col()
        .items_center()
        .gap(px(5.))
        .child(
            div()
                .w(px(36.))
                .h(px(25.))
                .rounded(px(6.))
                .border_2()
                .border_color(rgb(if accent { MINT } else { MUTED }))
                .child(
                    div()
                        .m(px(4.))
                        .h(px(12.))
                        .rounded(px(2.))
                        .bg(rgb(if accent { 0x38564f } else { 0x303432 })),
                ),
        )
        .child(
            div()
                .w(px(20.))
                .h(px(3.))
                .rounded_full()
                .bg(rgb(if accent { MINT } else { MUTED })),
        )
}

impl Janus {
    fn new(
        design_preview: bool,
        smoke_test: bool,
        runtime: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let browser = (!design_preview).then(|| Browser::start(runtime));
        if let Some(browser) = &browser {
            let mut updates = browser.subscribe();
            cx.spawn(async move |this, cx| {
                while updates.changed().await.is_ok() {
                    let snapshot = updates.borrow_and_update().clone();
                    if this
                        .update(cx, |this, cx| {
                            this.details_failure = match snapshot.phase {
                                BrowsePhase::Failed(reason) => Some(reason),
                                BrowsePhase::Searching => None,
                                BrowsePhase::Idle => this.details_failure,
                            };
                            if let Some(device) = this.selected_device.as_ref().and_then(|key| {
                                snapshot.devices.iter().find(|device| &device.key == key)
                            }) {
                                if this
                                    .details_device
                                    .as_ref()
                                    .is_some_and(|previous| previous.endpoints != device.endpoints)
                                {
                                    this.cancel_connection_check();
                                }
                                this.details_device = Some(device.clone());
                            }
                            if this.selected_device.as_ref().is_some_and(|key| {
                                !snapshot.devices.iter().any(|device| &device.key == key)
                            }) {
                                this.cancel_connection_check();
                                this.selected_device = None;
                            }
                            this.discovery = snapshot;
                            this.receiver_choices.retain(|key, _| {
                                this.discovery
                                    .devices
                                    .iter()
                                    .any(|device| &device.key == key)
                            });
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        Self {
            focus: cx.focus_handle(),
            logo: std::sync::Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../../docs/janus-logo.png").to_vec(),
            )),
            receiver_choices: std::collections::HashMap::new(),
            hovered_button: None,
            hovered_receiver: None,
            scroll: gpui::ScrollHandle::new(),
            page: Page::Cast,
            fixture: Fixture::Ready,
            selected: 0,
            source: 0,
            audio: true,
            receiving: false,
            muted: false,
            design_preview,
            smoke_pending: smoke_test,
            unmaximizing: false,
            discovery: DiscoverySnapshot::default(),
            browser,
            selected_device: None,
            details_device: None,
            details_failure: None,
            runtime: runtime.clone(),
            connection_check: ConnectionCheck::Idle,
            connection_session: None,
            connection_probe: None,
            connection_generation: 0,
            live_cast: LiveCast::Idle,
            cast_handles: None,
            cast_start: None,
            cast_generation: 0,
            pairing: Pairing::Idle,
            pin_pairing: None,
            pairing_device: None,
            pairing_generation: 0,
        }
    }

    fn casting(&self) -> bool {
        matches!(
            self.live_cast,
            LiveCast::Starting { .. } | LiveCast::Live { .. }
        )
    }

    /// Stops the real cast (media, the TV's mirroring app, the portal and
    /// the session) in the background. `reason` is shown afterwards.
    fn stop_cast(&mut self, reason: Option<String>) {
        self.cast_generation = self.cast_generation.wrapping_add(1);
        if let Some(start) = self.cast_start.take() {
            start.abort();
        }
        if let Some(casting) = self.cast_handles.take() {
            self.runtime.spawn(casting.stop());
        }
        self.live_cast = reason.map_or(LiveCast::Idle, LiveCast::Ended);
    }

    fn start_cast(&mut self, cx: &mut Context<Self>) {
        if self.casting() {
            self.stop_cast(None);
            cx.notify();
            return;
        }
        let Some(device) = self
            .selected_device
            .as_ref()
            .and_then(|key| self.discovery.devices.iter().find(|d| &d.key == key))
            .filter(|d| janus_mirror::supports_receiver(d))
            .cloned()
        else {
            return;
        };
        let choices = self
            .receiver_choices
            .get(&device.key)
            .copied()
            .unwrap_or_else(|| ReceiverChoices::for_receiver(&device));
        let source = if choices.window {
            MirrorSource::Window
        } else {
            MirrorSource::Screen
        };
        self.cancel_connection_check();
        self.cast_generation = self.cast_generation.wrapping_add(1);
        let generation = self.cast_generation;
        let name = device.display_name.clone();
        self.live_cast = LiveCast::Starting { name: name.clone() };
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        let options = MirrorOptions {
            source,
            audio: choices.audio,
            ..MirrorOptions::default().with_resolution(choices.resolution)
        };
        let pairing_device = device.clone();
        let task = self.runtime.spawn(async move {
            janus_mirror::start_receiver(&device, options)
                .await
                .map_err(|error| match error {
                    janus_mirror::MirrorError::Capture(janus_capture::CaptureError::Cancelled) => {
                        StartFailure::Message("Sharing was cancelled.".to_owned())
                    }
                    janus_mirror::MirrorError::AirPlay(janus_airplay::Error::NotPaired) => {
                        StartFailure::NotPaired
                    }
                    other => StartFailure::Message(format!("Couldn't start casting: {other}")),
                })
        });
        self.cast_start = Some(task.abort_handle());
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| {
                Err(StartFailure::Message(
                    "Casting stopped unexpectedly.".to_owned(),
                ))
            });
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(casting) if generation == this.cast_generation => {
                        this.cast_start = None;
                        let mut stats = casting.stats();
                        this.live_cast = LiveCast::Live {
                            name,
                            stats: stats.borrow().clone(),
                        };
                        this.cast_handles = Some(casting);
                        cx.spawn(async move |this, cx| {
                            while stats.changed().await.is_ok() {
                                let current = stats.borrow_and_update().clone();
                                let keep = this.update(cx, |this, cx| {
                                    if generation != this.cast_generation {
                                        return false;
                                    }
                                    if let Some(reason) = current.ended.clone() {
                                        this.stop_cast(Some(reason));
                                        cx.notify();
                                        return false;
                                    }
                                    if let LiveCast::Live { stats, .. } = &mut this.live_cast {
                                        *stats = current;
                                    }
                                    cx.notify();
                                    true
                                });
                                if !matches!(keep, Ok(true)) {
                                    break;
                                }
                            }
                        })
                        .detach();
                    }
                    // Stopped while starting: undo the cast that just began.
                    Ok(casting) => {
                        runtime.spawn(casting.stop());
                    }
                    Err(StartFailure::NotPaired) if generation == this.cast_generation => {
                        this.cast_start = None;
                        this.live_cast = LiveCast::Idle;
                        this.begin_pairing(pairing_device, cx);
                    }
                    Err(StartFailure::Message(reason)) if generation == this.cast_generation => {
                        this.live_cast = LiveCast::Ended(reason);
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks the TV to show its one-time code. The code is bound to this
    /// connection, which `pin_pairing` keeps open until the user submits.
    fn begin_pairing(&mut self, device: Candidate, cx: &mut Context<Self>) {
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        let generation = self.pairing_generation;
        let name = device.display_name.clone();
        self.pin_pairing = None;
        self.pairing = Pairing::Starting { name: name.clone() };
        self.pairing_device = Some(device.clone());
        let task = self
            .runtime
            .spawn(async move { janus_airplay::begin_pin_pairing(&device.endpoints).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.pairing_generation {
                    return;
                }
                match result {
                    Ok(Ok(pairing)) => {
                        this.pin_pairing = Some(pairing);
                        this.pairing = Pairing::Code {
                            name,
                            digits: String::new(),
                            error: None,
                        };
                    }
                    Ok(Err(error)) => {
                        this.cancel_pairing();
                        this.live_cast =
                            LiveCast::Ended(format!("Couldn't start pairing: {error}"));
                    }
                    Err(_) => this.cancel_pairing(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel_pairing(&mut self) {
        self.pairing_generation = self.pairing_generation.wrapping_add(1);
        self.pin_pairing = None;
        self.pairing = Pairing::Idle;
    }

    fn pairing_input(&mut self, key: &str, cx: &mut Context<Self>) {
        let Pairing::Code { digits, error, .. } = &mut self.pairing else {
            return;
        };
        match key {
            "backspace" => {
                digits.pop();
            }
            "enter" => {
                self.submit_pairing(cx);
                return;
            }
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                if digits.len() < PIN_DIGITS {
                    digits.push_str(digit);
                    *error = None;
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn submit_pairing(&mut self, cx: &mut Context<Self>) {
        let Pairing::Code { name, digits, .. } = &self.pairing else {
            return;
        };
        if digits.len() != PIN_DIGITS {
            return;
        }
        let Some(pairing) = self.pin_pairing.take() else {
            return;
        };
        let (name, pin) = (name.clone(), digits.clone());
        let generation = self.pairing_generation;
        self.pairing = Pairing::Finishing { name: name.clone() };
        let task = self
            .runtime
            .spawn(async move { pairing.finish(&pin).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.pairing_generation {
                    return;
                }
                match result {
                    Ok(Ok(())) => {
                        this.cancel_pairing();
                        // Paired: continue straight into the cast.
                        this.start_cast(cx);
                    }
                    Ok(Err(janus_airplay::Error::PinRejected)) => {
                        this.pairing = Pairing::Code {
                            name,
                            digits: String::new(),
                            error: Some(
                                "That code didn't match. Show a new code and try again.".into(),
                            ),
                        };
                    }
                    Ok(Err(error)) => {
                        this.cancel_pairing();
                        this.live_cast = LiveCast::Ended(format!("Pairing failed: {error}"));
                    }
                    Err(_) => this.cancel_pairing(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pairing_card(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (name, body) = match &self.pairing {
            Pairing::Idle => return None,
            Pairing::Starting { name } => (name, caption("Asking the TV to show a code…")),
            Pairing::Finishing { name } => (name, caption("Pairing…")),
            Pairing::Code {
                name,
                digits,
                error,
            } => {
                let mut boxes = div().flex().gap(px(8.));
                for index in 0..PIN_DIGITS {
                    let digit = digits
                        .chars()
                        .nth(index)
                        .map(String::from)
                        .unwrap_or_default();
                    boxes = boxes.child(
                        div()
                            .size(px(44.))
                            .rounded(px(10.))
                            .border_2()
                            .border_color(rgb(if index == digits.len() { MINT } else { LINE }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(20.))
                            .child(digit),
                    );
                }
                let mut column = div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(caption(
                        "Type the code shown on the TV, then press Enter. You only do this once.",
                    ))
                    .child(boxes);
                if let Some(error) = error {
                    column = column.child(div().text_color(rgb(ERROR)).child(error.clone()));
                }
                (name, column)
            }
        };
        let mut actions = div().flex().gap(px(8.));
        if let Pairing::Code { digits, error, .. } = &self.pairing {
            if error.is_some() {
                actions = actions.child(self.button(
                    "pin-new-code",
                    "Show a new code",
                    true,
                    true,
                    cx,
                    |this, _, cx| {
                        if let Some(device) = this.pairing_device.clone() {
                            this.begin_pairing(device, cx);
                        }
                    },
                ));
            } else {
                actions = actions.child(self.button(
                    "pin-submit",
                    "Pair",
                    true,
                    digits.len() == PIN_DIGITS,
                    cx,
                    |this, _, cx| this.submit_pairing(cx),
                ));
            }
        }
        actions =
            actions.child(
                self.button("pin-cancel", "Cancel", false, true, cx, |this, _, cx| {
                    this.cancel_pairing();
                    cx.notify();
                }),
            );
        Some(
            card()
                .w_full()
                .flex_shrink_0()
                .child(title(format!("Pair with {name}")))
                .child(body)
                .child(actions),
        )
    }

    fn cancel_connection_check(&mut self) {
        if let Some(task) = self.connection_probe.take() {
            task.abort();
        }
        if let Some(session) = self.connection_session.take() {
            // Graceful CLOSE on the runtime; dropping the handle also closes.
            self.runtime.spawn(session.close());
        }
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.connection_check = ConnectionCheck::Idle;
    }

    fn check_connection(&mut self, cx: &mut Context<Self>) {
        if self.connection_check.active() {
            self.cancel_connection_check();
            cx.notify();
            return;
        }
        let Some(device) = self
            .details_device
            .as_ref()
            .filter(|device| {
                self.selected_device.as_deref() == Some(device.key.as_str())
                    && matches!(
                        device.protocol,
                        ReceiverProtocol::GoogleCast | ReceiverProtocol::AirPlay
                    )
            })
            .cloned()
        else {
            return;
        };
        self.cancel_connection_check();
        let generation = self.connection_generation;
        if device.protocol == ReceiverProtocol::AirPlay {
            self.connection_check = ConnectionCheck::AirPlayChecking;
            let task = self
                .runtime
                .spawn(async move { janus_airplay::probe(&device.endpoints).await });
            self.connection_probe = Some(task.abort_handle());
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    if generation != this.connection_generation {
                        return;
                    }
                    this.connection_probe = None;
                    this.connection_check = match result {
                        Ok(Ok(info)) => ConnectionCheck::AirPlay(info),
                        Ok(Err(error)) => ConnectionCheck::AirPlayFailed(error.to_string()),
                        Err(_) => {
                            ConnectionCheck::AirPlayFailed("The connection check ended.".into())
                        }
                    };
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }

        let session = Session::start(device.endpoints, SessionConfig::default(), &self.runtime);
        let mut updates = session.subscribe();
        self.connection_check = ConnectionCheck::Live(session.snapshot());
        self.connection_session = Some(session);
        cx.spawn(async move |this, cx| {
            // Latest-value updates; tokio watch works on any executor.
            while updates.changed().await.is_ok() {
                let snapshot = updates.borrow_and_update().clone();
                let current = this.update(cx, |this, cx| {
                    if generation != this.connection_generation {
                        return false;
                    }
                    this.connection_check = ConnectionCheck::Live(snapshot);
                    cx.notify();
                    true
                });
                if !matches!(current, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        if self.casting() {
            self.stop_cast(None);
        }
        if self.pairing != Pairing::Idle {
            self.cancel_pairing();
        }
        self.cancel_connection_check();
        self.fixture = Fixture::Ready;
        self.receiving = false;
        cx.notify();
    }

    fn nav(
        &self,
        label: &'static str,
        page: Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        self.segment(
            label,
            label,
            self.page == page,
            true,
            cx,
            move |this, _, cx| {
                if page != Page::Help {
                    this.cancel_connection_check();
                }
                this.page = page;
                if let Some(browser) = &this.browser {
                    browser.set_enabled(page == Page::Cast);
                }
                cx.notify();
            },
        )
    }

    fn select_receiver(&mut self, key: &str) {
        if self.selected_device.as_deref() != Some(key) {
            self.cancel_connection_check();
            if !self.casting() {
                self.live_cast = LiveCast::Idle;
            }
        }
        self.selected_device = Some(key.to_owned());
        self.details_device = self
            .discovery
            .devices
            .iter()
            .find(|d| d.key == key)
            .cloned();
    }

    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl IntoElement,
        accent: bool,
        enabled: bool,
        cx: &mut Context<Self>,
        activate: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let id = id.into();
        let hovered = enabled && self.hovered_button.as_ref() == Some(&id);
        let base = if accent { MINT } else { RAISED };
        let over = if accent { 0xdafbf4 } else { 0x545b58 };
        let fill = div()
            .size_full()
            .px(px(12.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(base))
            .child(label);
        let fill = if hovered {
            fill.with_animation(
                "hover-fill",
                Animation::new(std::time::Duration::from_millis(140)).with_easing(ease_out_quint()),
                move |fill, delta| {
                    fill.bg(rgb(base).blend(gpui::Rgba {
                        a: delta,
                        ..rgb(over)
                    }))
                },
            )
            .into_any_element()
        } else {
            fill.into_any_element()
        };
        let activate = std::rc::Rc::new(activate);
        let click = activate.clone();
        div()
            .id(id.clone())
            .h(px(44.))
            .min_w(px(44.))
            .flex_shrink_0()
            .rounded_full()
            .border_2()
            .border_color(rgb(base))
            .text_color(rgb(if !enabled {
                MUTED
            } else if accent {
                BACKGROUND
            } else {
                TEXT
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.border_color(rgb(TEXT)))
                    .active(|style| style.opacity(0.85))
                    .on_hover(cx.listener(move |this, over, _, cx| {
                        if *over {
                            this.hovered_button = Some(id.clone());
                        } else if this.hovered_button.as_ref() == Some(&id) {
                            this.hovered_button = None;
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| click(this, window, cx)))
                    .on_key_down(cx.listener(
                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                            if !event.is_held
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                cx.stop_propagation();
                                activate(this, window, cx);
                            }
                        },
                    ))
            })
            .child(fill)
    }

    /// One option of a segmented control; the selected option is raised.
    fn segment(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        selected: bool,
        enabled: bool,
        cx: &mut Context<Self>,
        activate: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let activate = std::rc::Rc::new(activate);
        let click = activate.clone();
        div()
            .id(id.into())
            .h_full()
            .px(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(gpui::transparent_black())
            .text_color(rgb(if selected && enabled { TEXT } else { MUTED }))
            .when(selected, |segment| segment.bg(rgb(RAISED)))
            .when(enabled, |segment| {
                segment
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.border_color(rgb(TEXT)))
                    .when(!selected, |segment| {
                        segment.hover(|style| style.bg(rgb(0x2a2c2b)).text_color(rgb(TEXT)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| click(this, window, cx)))
                    .on_key_down(cx.listener(
                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                            if !event.is_held
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                cx.stop_propagation();
                                activate(this, window, cx);
                            }
                        },
                    ))
            })
            .child(label)
    }

    /// Small round title-bar button (minimize / close).
    fn window_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        activate: impl Fn(&mut Window) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .size(px(30.))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(rgb(MUTED))
            .bg(rgb(SURFACE))
            .cursor_pointer()
            .hover(move |style| {
                if danger {
                    style.bg(rgb(0xd9534f)).text_color(rgb(0xffffff))
                } else {
                    style.bg(rgb(RAISED)).text_color(rgb(TEXT))
                }
            })
            .active(|style| style.opacity(0.85))
            .on_click(cx.listener(move |_, _, window, _| activate(window)))
            .child(glyph)
    }

    fn discovered_target(
        &self,
        device: &Candidate,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected_device.as_deref() == Some(device.key.as_str());
        let choices = self
            .receiver_choices
            .get(&device.key)
            .copied()
            .unwrap_or_else(|| ReceiverChoices::for_receiver(device));
        let audio_available = device.advertised_outputs.audio != Some(false);
        let supported = janus_mirror::supports_receiver(device);
        let busy = self.casting();
        let active = busy && selected;
        let base = if selected { 0x293631 } else { SURFACE };
        let mut controls = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .flex_wrap()
            .items_center()
            .gap(px(8.));
        let mut source = track(44.);
        for (window_source, label) in [(false, "Screen"), (true, "Window")] {
            let key = device.key.clone();
            source = source.child(self.segment(
                format!("{}:source:{window_source}", device.key),
                label,
                choices.window == window_source,
                supported && !busy,
                cx,
                move |this, _, cx| {
                    if this.casting() {
                        return;
                    }
                    this.select_receiver(&key);
                    this.receiver_choices
                        .entry(key.clone())
                        .or_insert(choices)
                        .window = window_source;
                    cx.notify();
                },
            ));
        }
        controls = controls.child(source);
        let sound_enabled = supported && !busy && (audio_available || choices.audio);
        let sound =
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(4.))
                .child(div().size(px(8.)).rounded_full().bg(rgb(
                    if choices.audio && sound_enabled {
                        MINT
                    } else {
                        MUTED
                    },
                )))
                .child(if choices.audio {
                    "Sound on"
                } else {
                    "Sound off"
                });
        let key = device.key.clone();
        controls = controls.child(self.button(
            format!("{}:sound", device.key),
            sound,
            false,
            supported && !busy && (audio_available || choices.audio),
            cx,
            move |this, _, cx| {
                if this.casting() {
                    return;
                }
                this.select_receiver(&key);
                let choices = this.receiver_choices.entry(key.clone()).or_insert(choices);
                choices.audio = !choices.audio;
                cx.notify();
            },
        ));
        let key = device.key.clone();
        let resolution = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(4.))
            .child(div().text_color(rgb(MUTED)).child("Resolution"))
            .child(choices.resolution.label());
        controls = controls.child(self.button(
            format!("{}:resolution", device.key),
            resolution,
            false,
            supported && !busy,
            cx,
            move |this, _, cx| {
                if this.casting() {
                    return;
                }
                this.select_receiver(&key);
                let choices = this.receiver_choices.entry(key.clone()).or_insert(choices);
                choices.resolution = choices.resolution.next();
                cx.notify();
            },
        ));
        let label = match &self.live_cast {
            LiveCast::Starting { .. } if active => "Cancel · Esc",
            LiveCast::Live { .. } if active => "Stop · Esc",
            _ => "Cast",
        };
        let key = device.key.clone();
        let cast_action = self.button(
            format!("{}:cast", device.key),
            label,
            !busy,
            active || (supported && !busy),
            cx,
            move |this, _, cx| {
                if this.casting() {
                    if this.selected_device.as_deref() == Some(key.as_str()) {
                        this.stop_cast(None);
                    }
                } else {
                    this.select_receiver(&key);
                    this.start_cast(cx);
                }
                cx.notify();
            },
        );
        let hovered = self.hovered_receiver.as_deref() == Some(device.key.as_str());
        let background = div().absolute().inset_0().bg(rgb(base));
        let background = if hovered {
            background
                .with_animation(
                    "card-hover",
                    Animation::new(std::time::Duration::from_millis(140))
                        .with_easing(ease_out_quint()),
                    move |background, delta| {
                        background.bg(rgb(base).blend(gpui::Rgba {
                            a: delta,
                            ..rgb(0x303a36)
                        }))
                    },
                )
                .into_any_element()
        } else {
            background.into_any_element()
        };
        let mut body = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(12.))
            .bg(rgb(base))
            .child(background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(monitor(selected))
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(div().text_ellipsis().child(device.display_name.clone()))
                            .child(caption(
                                device.model.clone().unwrap_or_else(|| "Screen".into()),
                            )),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(0x52615b))
                            .bg(rgb(0x303b37))
                            .text_color(rgb(MINT))
                            .text_size(px(12.))
                            .px(px(10.))
                            .py(px(6.))
                            .child(device.protocol.label()),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(controls)
                    .child(cast_action),
            );
        if !audio_available {
            body = body.child(caption(
                "Video only · Sound isn't available for this receiver.",
            ));
        }
        if !supported {
            body = body.child(caption("Casting to this screen isn't available yet."));
        }
        if selected {
            let status = match &self.live_cast {
                LiveCast::Starting { name } => {
                    format!("Connecting to {name}… Choose a source in the system dialog.")
                }
                LiveCast::Live { name, stats } => {
                    format!("Sharing to {name} · {}×{}", stats.width, stats.height)
                }
                LiveCast::Ended(reason) => reason.clone(),
                LiveCast::Idle => String::new(),
            };
            if !status.is_empty() {
                body = body.child(caption(status));
            }
        }
        let key = device.key.clone();
        div()
            .id(SharedString::from(format!("receiver:{}", device.key)))
            .flex_shrink_0()
            .w_full()
            .min_w(px(0.))
            .rounded(px(12.))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(if selected { MINT } else { LINE }))
            .hover(|style| style.border_color(rgb(if selected { MINT } else { 0x69766f })))
            .on_hover(cx.listener(move |this, over, _, cx| {
                if *over {
                    this.hovered_receiver = Some(key.clone());
                } else if this.hovered_receiver.as_deref() == Some(key.as_str()) {
                    this.hovered_receiver = None;
                }
                cx.notify();
            }))
            .child(body)
    }

    fn live_cast(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut devices = div()
            .id("discovered-screens")
            .w_full()
            .min_w(px(0.))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(12.));
        let mut visible = 0;
        let mut active_visible = false;
        let mut receivers = self.discovery.devices.iter().collect::<Vec<_>>();
        receivers.sort_by_key(|device| {
            !(self.casting() && self.selected_device.as_deref() == Some(device.key.as_str()))
        });
        for device in receivers {
            if device.advertised_outputs.may_support(OutputKind::Video) {
                devices = devices.child(self.discovered_target(device, cx));
                visible += 1;
                active_visible |= self.selected_device.as_deref() == Some(device.key.as_str());
            }
        }
        if visible == 0 {
            devices = devices.child(
                div()
                    .py(px(24.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(monitor(false))
                    .child(match self.discovery.phase {
                        BrowsePhase::Failed(_) => "Discovery unavailable",
                        BrowsePhase::Idle => "Discovery paused",
                        BrowsePhase::Searching => "Looking for screens…",
                    })
                    .child(caption("Keep your TV awake on the same network.")),
            );
        }
        let mut actions = div().flex().items_center().gap(px(8.));
        // Stop must remain reachable even if the receiver's advertisement expires.
        if self.casting() && !active_visible {
            actions = actions.child(self.button(
                "stop-missing-receiver",
                "Stop · Esc",
                false,
                true,
                cx,
                |this, _, cx| {
                    this.stop_cast(None);
                    cx.notify();
                },
            ));
        }
        actions = actions.child(self.button(
            "refresh-discovery",
            "Refresh",
            false,
            true,
            cx,
            |this, _, cx| {
                if let Some(browser) = &this.browser {
                    browser.retry();
                }
                cx.notify();
            },
        ));
        card()
            .w_full()
            .min_w(px(0.))
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(title("Screens"))
                    .child(actions),
            )
            .child(devices)
    }

    fn target(
        &self,
        index: usize,
        name: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected == index;
        div()
            .id(("target", index))
            .flex()
            .items_center()
            .gap(px(12.))
            .p(px(12.))
            .w_full()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(if selected { MINT } else { LINE }))
            .bg(rgb(if selected { 0x293631 } else { SURFACE }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = index;
                cx.notify();
            }))
            .child(monitor(selected))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(name)
                    .child(caption(detail)),
            )
            .child(
                div()
                    .size(px(10.))
                    .rounded_full()
                    .bg(rgb(if selected { MINT } else { RAISED })),
            )
    }

    fn sources(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut choices = div().flex().flex_wrap().gap(px(6.));
        for (index, label) in ["Screen", "Window", "Sound"].into_iter().enumerate() {
            choices = choices.child(
                div()
                    .id(("source", index))
                    .cursor_pointer()
                    .px(px(10.))
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(rgb(if self.source == index { MINT } else { RAISED }))
                    .text_color(rgb(if self.source == index {
                        BACKGROUND
                    } else {
                        TEXT
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.source = index;
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        choices
    }

    fn cast(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let available = self.fixture == Fixture::Ready;
        let mut targets = card()
            .flex_1()
            .min_w(px(260.))
            .child(title("Nearby screens"))
            .child(caption("Choose where you want to share."));
        targets = match self.fixture {
            Fixture::Empty => targets.child(
                div()
                    .py(px(48.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(20.))
                    .child(monitor(false))
                    .child("Looking for Janus receivers")
                    .child(caption("Enable Receive casts on the other computer.")),
            ),
            _ => targets
                .child(self.target(
                    0,
                    "Living room",
                    if self.fixture == Fixture::Busy {
                        "Prototype · In use"
                    } else {
                        "Prototype · Ready to receive"
                    },
                    cx,
                ))
                .child(self.target(1, "Studio display", "Prototype · Ready to receive", cx))
                .child(self.target(2, "Meeting room", "Prototype · Ready to receive", cx)),
        };
        div()
            .flex()
            .flex_wrap()
            .gap(px(24.))
            .w_full()
            .child(targets)
            .child(
                div()
                    .w(px(260.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(
                        card()
                            .child(title("Make room for your ideas."))
                            .child(caption(
                                "A screen, a window, or just sound. Share what matters.",
                            ))
                            .child(
                                div()
                                    .h(px(112.))
                                    .rounded(px(18.))
                                    .bg(rgb(0x314b43))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(monitor(true)),
                            ),
                    )
                    .child(
                        card()
                            .child(title("Your source"))
                            .child(self.sources(cx))
                            .child(caption(
                                "The native source picker will open in the casting release.",
                            ))
                            .child(
                                div()
                                    .id("audio")
                                    .cursor_pointer()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .py(px(10.))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.audio = !this.audio;
                                        cx.notify();
                                    }))
                                    .child("Include system sound")
                                    .child(
                                        div()
                                            .rounded_full()
                                            .px(px(12.))
                                            .py(px(6.))
                                            .bg(rgb(if self.audio { MINT } else { RAISED }))
                                            .text_color(rgb(if self.audio {
                                                BACKGROUND
                                            } else {
                                                TEXT
                                            }))
                                            .child(if self.audio { "On" } else { "Off" }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("cast-preview")
                                    .cursor_pointer()
                                    .h(px(44.))
                                    .rounded_full()
                                    .bg(rgb(if available { MINT } else { RAISED }))
                                    .text_color(rgb(if available { BACKGROUND } else { MUTED }))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if available {
                                            this.fixture = Fixture::Active;
                                            cx.notify();
                                        }
                                    }))
                                    .child(if available {
                                        "Preview casting flow  ↗"
                                    } else {
                                        "Receiver unavailable"
                                    }),
                            ),
                    ),
            )
    }

    fn active(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let recovering = self.fixture == Fixture::Recovering;
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .w_full()
            .child(
                card()
                    .min_h(px(310.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(if recovering { SURFACE } else { 0x263b34 }))
                    .child(monitor(!recovering))
                    .child(title(if recovering {
                        "Reconnecting…"
                    } else {
                        "Your ideas, on a bigger screen."
                    }))
                    .child(caption(if recovering {
                        "Prototype state · media would be blanked and muted."
                    } else {
                        "Prototype preview · no screen content is captured."
                    })),
            )
            .child(
                card()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(title(
                                ["Living room", "Studio display", "Meeting room"][self.selected],
                            ))
                            .child(caption(if self.audio {
                                "Prototype · System sound enabled"
                            } else {
                                "Prototype · Sound off"
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(
                                div()
                                    .id("mute")
                                    .cursor_pointer()
                                    .px(px(24.))
                                    .py(px(14.))
                                    .rounded_full()
                                    .bg(rgb(RAISED))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.muted = !this.muted;
                                        cx.notify();
                                    }))
                                    .child(if self.muted { "Unmute" } else { "Mute" }),
                            )
                            .child(
                                div()
                                    .id("stop")
                                    .cursor_pointer()
                                    .px(px(24.))
                                    .py(px(14.))
                                    .rounded_full()
                                    .bg(rgb(MINT))
                                    .text_color(rgb(BACKGROUND))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.fixture = Fixture::Ready;
                                        this.receiving = false;
                                        cx.notify();
                                    }))
                                    .child("Stop · Esc"),
                            ),
                    ),
            )
    }

    fn receive(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        card().w_full().child(title("A bigger screen. One click away."))
            .child(caption("Let nearby computers running Janus cast here."))
            .child(div().id("receive-toggle").cursor_pointer().flex().items_center().justify_between().rounded(px(20.)).border_1().border_color(rgb(LINE)).p(px(24.))
                .on_click(cx.listener(|this, _, _, cx| { this.receiving = !this.receiving; cx.notify(); }))
                .child(div().flex().flex_col().gap(px(8.)).child("Receive casts").child(caption(if self.receiving { "Prototype · Ready to receive" } else { "Off · This computer is not discoverable" })))
                .child(div().rounded_full().px(px(20.)).py(px(10.)).bg(rgb(if self.receiving { MINT } else { RAISED })).text_color(rgb(if self.receiving { BACKGROUND } else { TEXT })).child(if self.receiving { "On" } else { "Off" })))
            .child(caption("When enabled in the casting release, reachable LAN devices can cast here without approval. Stop also turns receiving off."))
            .child(div().h(px(160.)).flex().items_center().justify_center().child(monitor(self.receiving)))
            .child(caption("Prototype only: this toggle does not open a network listener."))
    }

    fn settings(&self) -> impl IntoElement + use<> {
        card()
            .w_full()
            .gap(px(0.))
            .child(div().pb(px(14.)).child(title("Settings")))
            .child(setting_row("Appearance", "Dark"))
            .child(setting_row(
                "Resolution",
                "Auto uses your screen's own size, up to what the TV supports. Change it per screen on the Cast page.",
            ))
            .child(setting_row(
                "Sound",
                "Sound on shares everything this computer plays, even when you cast one window.",
            ))
            .child(setting_row(
                "Hisense interactive displays",
                "Supported automatically. Their casting isn't encrypted, so use them on a trusted network.",
            ))
            .child(setting_row(
                "AirPlay pairing",
                "Each TV that asks for a code is paired once; the pairing stays on this computer.",
            ))
    }

    fn help(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut details = card()
            .w_full()
            .child(title("Connection details"))
            .child(caption(format!("Janus {}", env!("CARGO_PKG_VERSION"))));
        if let Some(reason) = self.details_failure {
            details = details.child(caption(reason.message()));
        }
        if let Some(device) = &self.details_device {
            details = details
                .child(caption("Last selected"))
                .child(device.display_name.clone())
                .child(caption(device.protocol.label()))
                .child(caption(
                    "Use Check connection without sharing. Cast starts screen sharing.",
                ));
            if self.selected_device.as_deref() == Some(device.key.as_str())
                && matches!(
                    device.protocol,
                    ReceiverProtocol::GoogleCast | ReceiverProtocol::AirPlay
                )
            {
                let active = self.connection_check.active();
                details = details
                    .child(div().flex().child(self.button(
                        "check-connection",
                        if active {
                            "Disconnect"
                        } else {
                            "Check connection"
                        },
                        !active,
                        true,
                        cx,
                        |this, _, cx| this.check_connection(cx),
                    )))
                    .child(caption(self.connection_check.label().to_owned()))
                    .child(caption(
                        "No screen or sound is sent, and the TV's current app is left alone.",
                    ));
            }
        } else {
            details = details.child(caption("No screen selected."));
        }
        details.child(caption(
            "Janus manages casting here. Your system may still ask for screen-sharing permission.",
        ))
    }

    fn fixtures(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut row = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .items_center()
            .child(caption("DEV STATES"));
        for (index, (label, fixture)) in [
            ("Ready", Fixture::Ready),
            ("Empty", Fixture::Empty),
            ("Busy", Fixture::Busy),
            ("Denied", Fixture::PermissionDenied),
            ("Active", Fixture::Active),
            ("Recover", Fixture::Recovering),
            ("Error", Fixture::Error),
        ]
        .into_iter()
        .enumerate()
        {
            row = row.child(
                div()
                    .id(("fixture", index))
                    .cursor_pointer()
                    .rounded_full()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(12.))
                    .bg(rgb(if self.fixture == fixture {
                        RAISED
                    } else {
                        SURFACE
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = Page::Cast;
                        this.fixture = fixture;
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        row
    }
}

impl Drop for Janus {
    fn drop(&mut self) {
        self.stop_cast(None);
        self.cancel_connection_check();
    }
}

impl Render for Janus {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.smoke_pending) {
            // Register during drawing, so success follows this frame's submission.
            window.on_next_frame(|_, app| {
                println!("JANUS_SMOKE_OK: native window rendered; media not tested");
                app.quit();
            });
        }
        let mut navigation = track(40.).child(self.nav("Cast", Page::Cast, cx));
        if self.design_preview {
            navigation = navigation.child(self.nav("Receive", Page::Receive, cx));
        }
        navigation = navigation
            .child(self.nav("Settings", Page::Settings, cx))
            .child(self.nav("Help", Page::Help, cx));
        let mut content = div()
            .id("page-content")
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap(px(12.));
        content = match self.page {
            Page::Cast if !self.design_preview => content
                .children(self.pairing_card(cx))
                .child(self.live_cast(cx)),
            Page::Cast if matches!(self.fixture, Fixture::Active | Fixture::Recovering) => content.child(self.active(cx)),
            Page::Cast if self.fixture == Fixture::PermissionDenied || self.fixture == Fixture::Error => content.child(card()
                .child(title(if self.fixture == Fixture::PermissionDenied { "Sharing needs your permission." } else { "We couldn't reach that screen." }))
                .child(caption(if self.fixture == Fixture::PermissionDenied { "Allow screen sharing in system settings, then choose your source again." } else { "Check that both computers can reach each other and the receiver is on." }))
                .child(div().id("retry").cursor_pointer().rounded_full().bg(rgb(MINT)).text_color(rgb(BACKGROUND)).px(px(24.)).py(px(14.)).child("Back to screens")
                    .on_click(cx.listener(|this, _, _, cx| { this.fixture = Fixture::Ready; cx.notify(); })))),
            Page::Cast => content.child(self.cast(cx)),
            Page::Receive => content.child(self.receive(cx)),
            Page::Settings => content.child(self.settings()),
            Page::Help => content.child(self.help(cx)),
        };
        if self.design_preview {
            content = content.child(self.fixtures(cx));
        }
        if self.design_preview || self.page != Page::Cast {
            content = content.overflow_y_scroll().track_scroll(&self.scroll);
        }
        // Janus has no maximized state (user decision): a compositor maximize
        // (keyboard shortcut, drag to the top edge) is undone once.
        if window.is_maximized() {
            if !std::mem::replace(&mut self.unmaximizing, true) {
                window.zoom_window();
            }
        } else {
            self.unmaximizing = false;
        }
        let decorations = window.window_decorations();
        let client_side = matches!(decorations, Decorations::Client { .. });
        let tiling = match decorations {
            Decorations::Client { tiling } => tiling,
            Decorations::Server => Tiling::tiled(),
        };
        if client_side {
            window.set_client_inset(px(WINDOW_INSET));
        }
        let controls = client_side.then(|| self.window_controls(window, cx));
        let edges = if client_side && !tiling.is_tiled() {
            resize_edges()
        } else {
            Vec::new()
        };
        let radius = px(WINDOW_RADIUS);
        let surface = div()
            .id("janus-root")
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.pairing != Pairing::Idle && !event.is_held {
                    this.pairing_input(&event.keystroke.key, cx);
                }
            }))
            .on_action(cx.listener(Self::stop))
            .on_action(|_: &FocusNext, window, cx| {
                window.focus_next();
                cx.stop_propagation();
            })
            .on_action(|_: &FocusPrevious, window, cx| {
                window.focus_prev();
                cx.stop_propagation();
            })
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let mut offset = this.scroll.offset();
                offset.y = (offset.y - this.scroll.bounds().size.height * 0.8)
                    .max(-this.scroll.max_offset().height);
                this.scroll.set_offset(offset);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let mut offset = this.scroll.offset();
                offset.y = (offset.y + this.scroll.bounds().size.height * 0.8).min(px(0.));
                this.scroll.set_offset(offset);
                cx.notify();
            }))
            .size_full()
            .overflow_hidden()
            .bg(rgb(BACKGROUND))
            // Client-side window: rounded, outlined and lifted, except on
            // edges the compositor has tiled against the screen.
            .when(client_side, |surface| {
                surface
                    .border_color(rgb(OUTLINE))
                    .when(!tiling.top, |s| s.border_t_1())
                    .when(!tiling.bottom, |s| s.border_b_1())
                    .when(!tiling.left, |s| s.border_l_1())
                    .when(!tiling.right, |s| s.border_r_1())
                    .when(!(tiling.top || tiling.left), |s| s.rounded_tl(radius))
                    .when(!(tiling.top || tiling.right), |s| s.rounded_tr(radius))
                    .when(!(tiling.bottom || tiling.left), |s| s.rounded_bl(radius))
                    .when(!(tiling.bottom || tiling.right), |s| s.rounded_br(radius))
                    .when(!tiling.is_tiled(), |s| {
                        s.shadow(vec![BoxShadow {
                            color: hsla(0., 0., 0., 0.45),
                            offset: point(px(0.), px(2.)),
                            blur_radius: px(WINDOW_INSET - 2.),
                            spread_radius: px(0.),
                        }])
                    })
            })
            .text_color(rgb(TEXT))
            .font_family("sans-serif")
            .text_size(px(14.))
            .p(px(16.))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(960.))
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .flex_shrink_0()
                            .gap(px(12.))
                            .justify_between()
                            .items_center()
                            .child(
                                // Title area: drag to move (never maximizes).
                                div()
                                    .id("window-drag")
                                    .flex_1()
                                    .min_w(px(160.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .when(client_side, |area| {
                                        area.on_mouse_down(MouseButton::Left, |_, window, _| {
                                            window.start_window_move();
                                        })
                                    })
                                    .child(img(self.logo.clone()).size(px(32.)))
                                    .child(
                                        div()
                                            .text_size(px(19.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Janus"),
                                    )
                                    .children(
                                        self.design_preview.then(|| caption("Design preview")),
                                    ),
                            )
                            .child(navigation)
                            .children(controls),
                    )
                    .child(content),
            );
        div()
            .id("janus-window")
            .size_full()
            .relative()
            .when(client_side, |shell| {
                shell
                    .when(!tiling.top, |s| s.pt(px(WINDOW_INSET)))
                    .when(!tiling.bottom, |s| s.pb(px(WINDOW_INSET)))
                    .when(!tiling.left, |s| s.pl(px(WINDOW_INSET)))
                    .when(!tiling.right, |s| s.pr(px(WINDOW_INSET)))
            })
            .child(surface)
            .children(edges)
    }
}

impl Janus {
    /// Minimize and close for client-side decorations. There is no
    /// maximize: Janus keeps its compact window (user decision).
    fn window_controls(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div().flex().items_center().gap(px(8.));
        if window.window_controls().minimize {
            row = row.child(
                self.window_button("window-minimize", "–", false, cx, |window| {
                    window.minimize_window()
                }),
            );
        }
        // The same path as the compositor's close: casting stops.
        row.child(self.window_button("window-close", "✕", true, cx, |window| {
            window.remove_window()
        }))
    }
}

/// Invisible edge and corner handles; client-side windows resize themselves.
fn resize_edges() -> Vec<gpui::Stateful<gpui::Div>> {
    // The transparent shadow margin is the grab area; corners reach a little
    // into the rounded corner so they are easy to hit.
    const EDGE: f32 = WINDOW_INSET;
    const CORNER: f32 = WINDOW_INSET + WINDOW_RADIUS / 2.;
    let handle = |id: &'static str, edge: ResizeEdge| {
        div()
            .id(id)
            .absolute()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
    };
    vec![
        handle("resize-top", ResizeEdge::Top)
            .top_0()
            .left(px(EDGE))
            .right(px(EDGE))
            .h(px(EDGE))
            .cursor_ns_resize(),
        handle("resize-bottom", ResizeEdge::Bottom)
            .bottom_0()
            .left(px(EDGE))
            .right(px(EDGE))
            .h(px(EDGE))
            .cursor_ns_resize(),
        handle("resize-left", ResizeEdge::Left)
            .left_0()
            .top(px(EDGE))
            .bottom(px(EDGE))
            .w(px(EDGE))
            .cursor_ew_resize(),
        handle("resize-right", ResizeEdge::Right)
            .right_0()
            .top(px(EDGE))
            .bottom(px(EDGE))
            .w(px(EDGE))
            .cursor_ew_resize(),
        handle("resize-top-left", ResizeEdge::TopLeft)
            .top_0()
            .left_0()
            .size(px(CORNER))
            .cursor_nwse_resize(),
        handle("resize-bottom-right", ResizeEdge::BottomRight)
            .bottom_0()
            .right_0()
            .size(px(CORNER))
            .cursor_nwse_resize(),
        handle("resize-top-right", ResizeEdge::TopRight)
            .top_0()
            .right_0()
            .size(px(CORNER))
            .cursor_nesw_resize(),
        handle("resize-bottom-left", ResizeEdge::BottomLeft)
            .bottom_0()
            .left_0()
            .size(px(CORNER))
            .cursor_nesw_resize(),
    ]
}

fn main() {
    if std::env::args().any(|argument| argument == "--version" || argument == "-V") {
        println!("janus {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let design_preview = std::env::args().any(|argument| argument == "--design-preview");
    let smoke_test = std::env::args().any(|argument| argument == "--smoke-test");
    if smoke_test {
        // A missing renderer/frame callback must fail CI rather than hang it.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(20));
            eprintln!("Janus smoke test timed out before rendering");
            std::process::exit(1);
        });
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("start Janus discovery runtime");
    let handle = runtime.handle().clone();
    Application::new().run(move |app: &mut App| {
        app.bind_keys([
            KeyBinding::new("escape", Stop, None),
            KeyBinding::new("tab", FocusNext, None),
            KeyBinding::new("shift-tab", FocusPrevious, None),
            KeyBinding::new("pageup", PageUp, None),
            KeyBinding::new("pagedown", PageDown, None),
        ]);
        app.on_window_closed(|app| {
            if app.windows().is_empty() {
                app.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(840.), px(520.)), app);
        app.open_window(
            WindowOptions {
                app_id: Some("janus".into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(420.))),
                // GNOME/Wayland draws no title bar for this window, so Janus
                // draws its own controls; a server-decorated desktop keeps its own.
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Janus::new(design_preview, smoke_test, &handle, cx));
                view.update(cx, |view, _| window.focus(&view.focus));
                view
            },
        )
        .expect("open Janus window");
        app.activate(true);
    });
}
