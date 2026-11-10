//! H.264 encoding with the bundled OpenH264 library: real-time screen-content
//! mode, Constrained Baseline (no B-frames, widest receiver support), target
//! bitrate rate control, IDR on demand plus a periodic safety IDR. Each call
//! yields one Annex-B access unit; key frames carry SPS/PPS.
use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile,
        RateControlMode, UsageType,
    },
    formats::{BgraSliceU8, YUVBuffer},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoConfig {
    /// Even, ≥ 320×240 (Open Screen minimum).
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub bit_rate: u32,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid video size or rate")]
    Config,
    #[error("frame size does not match the encoder")]
    FrameSize,
    #[error("H.264 encoder failed: {0}")]
    Encoder(String),
}

/// One encoded access unit.
#[derive(Clone, Debug)]
pub struct EncodedVideo {
    pub key_frame: bool,
    pub data: Vec<u8>,
}

pub struct H264Encoder {
    encoder: Encoder,
    config: VideoConfig,
    yuv: YUVBuffer,
}

impl H264Encoder {
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
        let encoder_config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .bitrate(BitRate::from_bps(config.bit_rate))
            .max_frame_rate(FrameRate::from_hz(config.frame_rate as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            // Skipping after a large frame froze screen updates for up to a
            // second and left a starved key frame unrefined on a still screen.
            .skip_frames(false)
            // Scene-change detection turned every large screen change into a
            // multi-hundred-KB IDR (38 in 30 s measured), bursting the link.
            .scene_change_detect(false)
            .background_detection(false)
            .adaptive_quantization(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(config.frame_rate * 10));
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), encoder_config)
            .map_err(|error| Error::Encoder(error.to_string()))?;
        Ok(Self {
            encoder,
            config,
            yuv: YUVBuffer::new(config.width as usize, config.height as usize),
        })
    }

    pub fn config(&self) -> VideoConfig {
        self.config
    }

    /// Encodes one tight BGRA frame of exactly the configured size.
    /// Returns `None` if the encoder chose to skip the frame.
    pub fn encode(
        &mut self,
        bgra: &[u8],
        force_key_frame: bool,
    ) -> Result<Option<EncodedVideo>, Error> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        if bgra.len() != w * h * 4 {
            return Err(Error::FrameSize);
        }
        self.yuv.read_bgra8(BgraSliceU8::new(bgra, (w, h)));
        if force_key_frame {
            self.encoder.force_intra_frame();
        }
        let stream = self
            .encoder
            .encode(&self.yuv)
            .map_err(|error| Error::Encoder(error.to_string()))?;
        let key_frame = match stream.frame_type() {
            FrameType::IDR => true,
            FrameType::I | FrameType::P | FrameType::IPMixed => false,
            FrameType::Skip | FrameType::Invalid => return Ok(None),
        };
        let data = stream.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(EncodedVideo { key_frame, data }))
    }
}
