//! Encoder selection: the GPU (VA-API through GStreamer) when this system can
//! encode H.264 in hardware, otherwise the bundled OpenH264 encoder. Output
//! units carry the sequence number of the frame they encode, so callers can
//! keep capture timestamps exact when the GPU returns a frame late.
//! `JANUS_VIDEO_ENCODER=software` forces the software encoder.
use crate::h264::{EncodedVideo, Error, H264Encoder, VideoConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Gpu,
    Software,
}
impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gpu => "GPU",
            Self::Software => "software",
        }
    }
}

enum Inner {
    Software(Box<H264Encoder>),
    #[cfg(target_os = "linux")]
    Gpu(Box<crate::gpu::GpuEncoder>),
}

pub struct VideoEncoder {
    inner: Inner,
    next: u64,
}

impl std::fmt::Debug for VideoEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoEncoder")
            .field("backend", &self.backend())
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
fn software_forced() -> bool {
    std::env::var("JANUS_VIDEO_ENCODER").is_ok_and(|v| v.eq_ignore_ascii_case("software"))
}

/// Whether a GPU encoder will be used for new encoders on this system.
pub fn gpu_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        !software_forced() && crate::gpu::available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

impl VideoEncoder {
    /// GPU when available and it starts; otherwise software.
    pub fn new(config: VideoConfig) -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        if gpu_available()
            && let Ok(gpu) = crate::gpu::GpuEncoder::new(config)
        {
            return Ok(Self {
                inner: Inner::Gpu(Box::new(gpu)),
                next: 0,
            });
        }
        Ok(Self {
            inner: Inner::Software(Box::new(H264Encoder::new(config)?)),
            next: 0,
        })
    }

    pub fn backend(&self) -> Backend {
        match &self.inner {
            Inner::Software(_) => Backend::Software,
            #[cfg(target_os = "linux")]
            Inner::Gpu(_) => Backend::Gpu,
        }
    }

    pub fn config(&self) -> VideoConfig {
        match &self.inner {
            Inner::Software(e) => e.config(),
            #[cfg(target_os = "linux")]
            Inner::Gpu(e) => e.config(),
        }
    }

    /// Retargets the bitrate while encoding. GPU only: the bundled software
    /// encoder keeps its configured rate and this returns `false`.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn set_bit_rate(&mut self, bit_rate: u32) -> bool {
        match &mut self.inner {
            Inner::Software(_) => false,
            #[cfg(target_os = "linux")]
            Inner::Gpu(e) => {
                e.set_bit_rate(bit_rate.max(300_000));
                true
            }
        }
    }

    /// Sequence number the next `encode` call will assign to its frame.
    pub fn next_sequence(&self) -> u64 {
        self.next
    }

    /// Units finished since the last call (GPU only; software is synchronous).
    pub fn poll(&mut self) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        match &mut self.inner {
            Inner::Software(_) => Ok(Vec::new()),
            #[cfg(target_os = "linux")]
            Inner::Gpu(e) => e.poll(),
        }
    }

    /// Encodes one tight BGRA/BGRx frame; returns the ready access units,
    /// oldest first, with the sequence number of the frame each encodes.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        force_key_frame: bool,
    ) -> Result<Vec<(u64, EncodedVideo)>, Error> {
        let sequence = self.next;
        self.next += 1;
        match &mut self.inner {
            Inner::Software(e) => Ok(e
                .encode(bgra, force_key_frame)?
                .map(|unit| (sequence, unit))
                .into_iter()
                .collect()),
            #[cfg(target_os = "linux")]
            Inner::Gpu(e) => e.encode(bgra, force_key_frame),
        }
    }
}
