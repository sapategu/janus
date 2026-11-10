//! PipeWire video consumer on a dedicated thread. Negotiates packed 32-bit
//! RGB in mapped (SHM/memfd) buffers, copies each frame out and publishes only
//! the latest one; the PipeWire buffer is returned immediately.
use crate::{CaptureError, Frame, PixelLayout};
use pipewire as pw;
use pw::spa;
use std::{
    os::fd::OwnedFd,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Instant,
};
use tokio::sync::watch;

/// Largest frame accepted (8K × 8K × 4).
const MAX_FRAME_BYTES: usize = 8192 * 8192 * 4;

pub struct VideoStream {
    latest: watch::Receiver<Option<Arc<Frame>>>,
    error: Arc<Mutex<Option<CaptureError>>>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl VideoStream {
    /// Consumes PipeWire node `node_id` on the remote behind `fd` (from the
    /// portal). Returns once the stream is connected.
    pub fn start(fd: OwnedFd, node_id: u32) -> Result<Self, CaptureError> {
        let (publish, latest) = watch::channel(None);
        let error = Arc::new(Mutex::new(None));
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread_error = error.clone();
        let thread = std::thread::Builder::new()
            .name("janus-pw-video".into())
            .spawn(move || {
                let result = run(fd, node_id, publish, quit_rx, &ready_tx);
                if let Err(failure) = result {
                    *thread_error.lock().expect("error lock") = Some(failure.clone());
                    let _ = ready_tx.send(Err(failure));
                }
            })
            .map_err(|error| CaptureError::PipeWire(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                latest,
                error,
                quit,
                thread: Some(thread),
            }),
            Ok(Err(failure)) => {
                let _ = thread.join();
                Err(failure)
            }
            Err(_) => Err(CaptureError::PipeWire("capture thread ended".into())),
        }
    }

    /// Latest-value frames; `None` until the first frame arrives.
    pub fn frames(&self) -> watch::Receiver<Option<Arc<Frame>>> {
        self.latest.clone()
    }

    pub fn error(&self) -> Option<CaptureError> {
        self.error.lock().expect("error lock").clone()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn layout(format: spa::param::video::VideoFormat) -> Option<PixelLayout> {
    use spa::param::video::VideoFormat as F;
    Some(match format {
        F::BGRx | F::BGRA => PixelLayout::Bgrx,
        F::RGBx | F::RGBA => PixelLayout::Rgbx,
        F::xRGB | F::ARGB => PixelLayout::Xrgb,
        F::xBGR | F::ABGR => PixelLayout::Xbgr,
        _ => return None,
    })
}

struct State {
    format: spa::param::video::VideoInfoRaw,
    publish: watch::Sender<Option<Arc<Frame>>>,
    sequence: u64,
}

fn run(
    fd: OwnedFd,
    node_id: u32,
    publish: watch::Sender<Option<Arc<Frame>>>,
    quit: pw::channel::Receiver<()>,
    ready: &std::sync::mpsc::SyncSender<Result<(), CaptureError>>,
) -> Result<(), CaptureError> {
    let failed = |error: pw::Error| CaptureError::PipeWire(error.to_string());
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failed)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failed)?;
    let core = context.connect_fd_rc(fd, None).map_err(failed)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "janus-screen",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(failed)?;

    let state = State {
        format: Default::default(),
        publish,
        sequence: 0,
    };
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|_, state, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Ok((media, sub)) = spa::param::format_utils::parse_format(param)
                && media == spa::param::format::MediaType::Video
                && sub == spa::param::format::MediaSubtype::Raw
            {
                let _ = state.format.parse(param);
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(layout) = layout(state.format.format()) else {
                return;
            };
            let size = state.format.size();
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let chunk = data.chunk();
            let (offset, length, stride) = (
                chunk.offset() as usize,
                chunk.size() as usize,
                chunk.stride(),
            );
            if length == 0 || stride <= 0 || length > MAX_FRAME_BYTES {
                return;
            }
            let Some(bytes) = data.data() else { return };
            let Some(pixels) = bytes.get(offset..offset + length) else {
                return;
            };
            state.sequence += 1;
            let frame = Frame {
                width: size.width,
                height: size.height,
                stride: stride as usize,
                layout,
                data: pixels.to_vec(),
                captured: Instant::now(),
                sequence: state.sequence,
            };
            state.publish.send_replace(Some(Arc::new(frame)));
        })
        .register()
        .map_err(failed)?;

    use spa::param::video::VideoFormat as F;
    let format = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            F::BGRx,
            F::BGRx,
            F::RGBx,
            F::BGRA,
            F::RGBA,
            F::xRGB,
            F::xBGR
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 120, denom: 1 }
        ),
    );
    let bytes: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(format),
    )
    .map_err(|error| CaptureError::PipeWire(format!("{error:?}")))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&bytes)
        .ok_or_else(|| CaptureError::PipeWire("format pod".into()))?];
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(failed)?;

    let mainloop_quit = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| mainloop_quit.quit());
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}
