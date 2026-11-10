//! Desktop audio from the default output's monitor (what the user hears),
//! converted by PipeWire to 48 kHz interleaved stereo f32. Chunks go to a
//! bounded queue; when the consumer falls behind, the oldest audio is
//! dropped rather than growing latency.
use crate::{AudioChunk, CaptureError};
use pipewire as pw;
use pw::spa;
use std::{
    sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    thread::JoinHandle,
    time::Instant,
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// ~640 ms of PipeWire quanta before the oldest are dropped.
const QUEUE_CHUNKS: usize = 64;

pub struct AudioStream {
    chunks: Receiver<AudioChunk>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream {
    /// Connects to the default output's monitor on the user's PipeWire.
    pub fn start() -> Result<Self, CaptureError> {
        Self::start_at_rate(SAMPLE_RATE)
    }

    /// Lets PipeWire perform native rate conversion for the selected protocol.
    /// Cast uses 48 kHz; legacy AirPlay ALAC uses 44.1 kHz.
    pub fn start_at_rate(sample_rate: u32) -> Result<Self, CaptureError> {
        if !matches!(sample_rate, 44_100 | 48_000) {
            return Err(CaptureError::Unavailable(
                "unsupported audio sample rate".into(),
            ));
        }
        let (sender, chunks) = sync_channel(QUEUE_CHUNKS);
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("janus-pw-audio".into())
            .spawn(move || {
                if let Err(failure) = run(sender, quit_rx, &ready_tx, sample_rate) {
                    let _ = ready_tx.send(Err(failure));
                }
            })
            .map_err(|error| CaptureError::PipeWire(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                chunks,
                quit,
                thread: Some(thread),
            }),
            Ok(Err(failure)) => {
                let _ = thread.join();
                Err(failure)
            }
            Err(_) => Err(CaptureError::PipeWire("audio thread ended".into())),
        }
    }

    /// Captured audio in arrival order; non-blocking.
    pub fn try_recv(&self) -> Option<AudioChunk> {
        self.chunks.try_recv().ok()
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

impl Drop for AudioStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    sender: SyncSender<AudioChunk>,
    quit: pw::channel::Receiver<()>,
    ready: &SyncSender<Result<(), CaptureError>>,
    sample_rate: u32,
) -> Result<(), CaptureError> {
    let failed = |error: pw::Error| CaptureError::PipeWire(error.to_string());
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(failed)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(failed)?;
    let core = context.connect_rc(None).map_err(failed)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "janus-desktop-audio",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            // Capture the default sink's monitor: the desktop's output.
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            *pw::keys::NODE_LATENCY => "480/48000",
        },
    )
    .map_err(failed)?;
    let _listener = stream
        .add_local_listener_with_user_data(sender)
        .process(move |stream, sender| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let length = data.chunk().size() as usize;
            let offset = data.chunk().offset() as usize;
            let Some(bytes) = data.data() else { return };
            let Some(bytes) = bytes.get(offset..offset + length) else {
                return;
            };
            let samples: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            if samples.is_empty() {
                return;
            }
            let captured = Instant::now()
                .checked_sub(std::time::Duration::from_secs_f64(
                    samples.len() as f64 / f64::from(CHANNELS * sample_rate),
                ))
                .unwrap_or_else(Instant::now);
            let chunk = AudioChunk { samples, captured };
            if let Err(TrySendError::Full(_)) = sender.try_send(chunk) {
                // Consumer is behind; drop this chunk instead of queueing latency.
            }
        })
        .register()
        .map_err(failed)?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(sample_rate);
    info.set_channels(CHANNELS);
    let format = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
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
            None,
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
