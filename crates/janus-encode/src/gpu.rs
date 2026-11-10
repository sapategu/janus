//! GPU H.264 through GStreamer's VA plugin: BGRx frames are uploaded and
//! colour-converted on the GPU (`vapostproc`) and encoded by `vah264enc`
//! (Constrained Baseline like the software encoder, for the widest receiver
//! support; byte-stream access units).
//!
//! Rate control is VBR around the planned bitrate with a 2× peak and a QP
//! ceiling. Constant bitrate starved the one frame a screen change produces
//! (≈1/30 of a second's budget for a whole new page of text), and a static
//! screen then never got refined, so text stayed blurry for seconds.
//! Used only when the VA driver exposes H.264 encoding; otherwise callers
//! keep the bundled software encoder.
use crate::h264::{EncodedVideo, Error, VideoConfig};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use std::{collections::VecDeque, time::Duration};

/// Frames submitted but not yet encoded (about 50 ms at 60 fps).
const MAX_IN_FLIGHT: usize = 3;
/// Worst quantizer allowed: keeps desktop text legible on a screen change
/// at the cost of a larger frame, which callers pace by back-pressure.
pub const MAX_QP: u32 = 38;

/// VBR peak in kbps for a target bitrate: `target-percentage=50` makes the
/// encoder aim at half of `bitrate`, so the peak is twice the target.
fn peak_kbps(bit_rate: u32) -> u32 {
    (bit_rate / 1000).max(300).saturating_mul(2)
}

/// Whether this system can run the GPU pipeline (plugin present; the VA
/// plugin registers `vah264enc` only when the driver can encode H.264).
pub fn available() -> bool {
    gst::init().is_ok()
        && gst::ElementFactory::find("vah264enc").is_some()
        && gst::ElementFactory::find("vapostproc").is_some()
}

pub struct GpuEncoder {
    pipeline: gst::Pipeline,
    source: AppSrc,
    sink: AppSink,
    config: VideoConfig,
    frame: u64,
    pending: VecDeque<u64>,
}

impl std::fmt::Debug for GpuEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuEncoder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::Encoder(format!("GPU encoder: {error}"))
}

impl GpuEncoder {
    pub fn new(config: VideoConfig) -> Result<Self, Error> {
        if config.width < 320
            || config.height < 240
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
            || config.frame_rate == 0
            || config.frame_rate > 120
            || config.bit_rate < 300_000
        {
            return Err(Error::Config);
        }
        gst::init().map_err(failed)?;
        let kbps = peak_kbps(config.bit_rate);
        // Balanced preset where the GPU has headroom (1080p is far below its
        // limit); the fastest preset only for larger frames.
        let usage = if u64::from(config.width) * u64::from(config.height) <= 1920 * 1080 {
            4
        } else {
            7
        };
        // A key frame every 10 s keeps a safety refresh without bursts;
        // receivers request more when they lose one.
        let description = format!(
            "appsrc name=source is-live=true format=time block=false max-buffers=2 \
             caps=video/x-raw,format=BGRx,width={w},height={h},framerate={fps}/1 \
             ! vapostproc ! video/x-raw(memory:VAMemory),format=NV12 \
             ! vah264enc name=encoder rate-control=vbr bitrate={kbps} target-percentage=50 \
               max-qp={qp} key-int-max={key} b-frames=0 ref-frames=1 target-usage={usage} \
             ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au \
             ! appsink name=sink sync=false max-buffers=16 drop=false",
            w = config.width,
            h = config.height,
            fps = config.frame_rate,
            key = config.frame_rate * 10,
            qp = MAX_QP,
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(failed)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| failed("not a pipeline"))?;
        let source = pipeline
            .by_name("source")
            .and_then(|e| e.downcast::<AppSrc>().ok())
            .ok_or_else(|| failed("missing source"))?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<AppSink>().ok())
            .ok_or_else(|| failed("missing sink"))?;
        let encoder = Self {
            pipeline,
            source,
            sink,
            config,
            frame: 0,
            pending: VecDeque::new(),
        };
        encoder
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(failed)?;
        let (result, _, _) = encoder.pipeline.state(gst::ClockTime::from_seconds(5));
        result.map_err(|_| encoder.bus_error("couldn't start"))?;
        Ok(encoder)
    }

    pub fn config(&self) -> VideoConfig {
        self.config
    }

    /// Changes the target bitrate of the running encoder (VA allows this in
    /// PLAYING); used to follow the network like the native EShare sender.
    pub fn set_bit_rate(&mut self, bit_rate: u32) {
        if let Some(encoder) = self.pipeline.by_name("encoder") {
            encoder.set_property("bitrate", peak_kbps(bit_rate));
            self.config.bit_rate = bit_rate;
        }
    }

    fn bus_error(&self, fallback: &str) -> Error {
        let message = self.pipeline.bus().and_then(|bus| {
            bus.pop_filtered(&[gst::MessageType::Error])
                .and_then(|m| match m.view() {
                    gst::MessageView::Error(e) => Some(e.error().to_string()),
                    _ => None,
                })
        });
        failed(message.unwrap_or_else(|| fallback.to_owned()))
    }

    /// Queues one tight BGRA/BGRx frame without waiting for it, then returns
    /// every access unit already finished (see [`Self::poll`]). Upload,
    /// conversion and encoding overlap with the caller's next capture.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        force_key_frame: bool,
    ) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        if bgra.len() != w * h * 4 {
            return Err(Error::FrameSize);
        }
        if self.pending.len() >= MAX_IN_FLIGHT {
            // Bounded: wait for the oldest frame rather than queue without limit.
            let mut out = self.collect(Some(Duration::from_millis(500)))?;
            if self.pending.len() >= MAX_IN_FLIGHT {
                return Err(self.bus_error("encoder stalled"));
            }
            out.extend(self.submit(bgra, force_key_frame)?);
            return Ok(out);
        }
        self.submit(bgra, force_key_frame)
    }

    fn submit(
        &mut self,
        bgra: &[u8],
        force_key_frame: bool,
    ) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        if force_key_frame {
            let event = gstreamer_video::DownstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            self.source.send_event(event);
        }
        let duration = gst::ClockTime::SECOND / u64::from(self.config.frame_rate);
        let mut buffer = gst::Buffer::from_mut_slice(bgra.to_vec());
        {
            let buffer = buffer.get_mut().ok_or_else(|| failed("buffer"))?;
            buffer.set_pts(duration * self.frame);
            buffer.set_duration(duration);
        }
        self.pending.push_back(self.frame);
        self.frame += 1;
        self.source.push_buffer(buffer).map_err(failed)?;
        self.collect(None)
    }

    /// Access units finished so far, oldest first, each with the sequence
    /// number of its frame. Never blocks. With no B-frames the encoder emits
    /// exactly one unit per frame in submission order, so units are matched
    /// first-in-first-out (output timestamps carry an encoder offset).
    pub fn poll(&mut self) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        self.collect(None)
    }

    fn collect(&mut self, wait: Option<Duration>) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        let mut out = Vec::new();
        let mut wait = wait;
        while let Some(sample) = match wait.take() {
            Some(wait) => self
                .sink
                .try_pull_sample(gst::ClockTime::from_nseconds(wait.as_nanos() as u64)),
            None => self.sink.try_pull_sample(gst::ClockTime::ZERO),
        } {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Some(sequence) = self.pending.pop_front() else {
                return Err(failed("unexpected output unit"));
            };
            let map = buffer.map_readable().map_err(failed)?;
            out.push((
                sequence,
                EncodedVideo {
                    key_frame: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                    data: map.as_slice().to_vec(),
                },
            ));
        }
        Ok(out)
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        let _ = self.source.end_of_stream();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
