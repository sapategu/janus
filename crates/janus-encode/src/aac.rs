//! EShare AAC-ELD matching the native sender: AOT 39, 480 samples,
//! 44.1 kHz stereo, 192 kbit/s, raw access units. AAC-LC is a different
//! bitstream and cannot be substituted for the negotiated EShare format.
use fdk_aac_rust::{Encoder, sys};
use std::collections::VecDeque;
use thiserror::Error;

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 480;
const MAX_PENDING: usize = FRAME_SAMPLES * CHANNELS * 16;
const MAX_PACKET_BYTES: usize = 4096;

#[derive(Debug, Error)]
pub enum Error {
    #[error("AAC input must be bounded interleaved stereo PCM")]
    Input,
    #[error("AAC-ELD encoder failed: {0}")]
    Encoder(String),
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::Encoder(error.to_string())
}

/// Checks that the exact native codec can initialize, without retaining a
/// thread-affine FDK handle or substituting a different AAC profile.
pub fn available() -> bool {
    AacEncoder::new().is_ok()
}

/// Create, use and drop on the dedicated audio thread. The safe FDK wrapper
/// deliberately does not make its native encoder handle transferable.
pub struct AacEncoder {
    encoder: Encoder,
    pending: VecDeque<i16>,
    output: Vec<u8>,
    emitted: u64,
    codec_data: Vec<u8>,
}

impl std::fmt::Debug for AacEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AacEncoder").finish_non_exhaustive()
    }
}

impl AacEncoder {
    pub fn new() -> Result<Self, Error> {
        let mut encoder = Encoder::open(CHANNELS as u32).map_err(failed)?;
        // Values read from the shipped macOS FdkAACEncoder::initEncoder:
        // AOT=39, granule=480, sample rate=44100, stereo/WAVE order, 192000,
        // and transmux=0. No LC/LD or alternate-frame-size fallback.
        for (parameter, value) in [
            (sys::AACENC_AOT, 39),
            (sys::AACENC_GRANULE_LENGTH, FRAME_SAMPLES as u32),
            (sys::AACENC_SAMPLERATE, SAMPLE_RATE),
            (sys::AACENC_CHANNELMODE, CHANNELS as u32),
            (sys::AACENC_CHANNELORDER, 1),
            (sys::AACENC_BITRATE, 192_000),
            (sys::AACENC_TRANSMUX, 0),
        ] {
            encoder.set_param(parameter, value).map_err(failed)?;
        }
        encoder.initialize().map_err(failed)?;
        let info = encoder.info().map_err(failed)?;
        if info.frame_length != FRAME_SAMPLES as u32
            || info.input_channels != CHANNELS as u32
            || info.max_output_bytes == 0
            || info.max_output_bytes as usize > MAX_PACKET_BYTES
        {
            return Err(failed("unexpected AAC-ELD frame geometry"));
        }
        let codec_data = encoder.audio_specific_config().map_err(failed)?;
        // AOT39 is escaped in AudioSpecificConfig. FrameLengthFlag=1 selects
        // 480 rather than 512; the remaining ELD flags are clear.
        if codec_data != [0xf8, 0xe8, 0x50, 0x00] {
            return Err(failed("unexpected AAC-ELD AudioSpecificConfig"));
        }
        Ok(Self {
            encoder,
            pending: VecDeque::with_capacity(MAX_PENDING),
            output: vec![0; info.max_output_bytes as usize],
            emitted: 0,
            codec_data,
        })
    }

    /// Submit finite/clamped f32 stereo samples. Returns raw AAC-ELD units
    /// with per-channel source sample positions (0, 480, ...). Anchor these
    /// positions to the first capture timestamp, not packet arrival time.
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<(u64, Vec<u8>)>, Error> {
        if !samples.len().is_multiple_of(CHANNELS)
            || samples.len() > MAX_PENDING.saturating_sub(self.pending.len())
        {
            return Err(Error::Input);
        }
        self.pending.extend(samples.iter().map(|sample| {
            if sample.is_finite() {
                (sample.clamp(-1., 1.) * 32767.).round() as i16
            } else {
                0
            }
        }));
        let mut packets = Vec::new();
        while self.pending.len() >= FRAME_SAMPLES * CHANNELS {
            let frame: Vec<i16> = self.pending.drain(..FRAME_SAMPLES * CHANNELS).collect();
            let length = self
                .encoder
                .encode_interleaved_i16(&frame, &mut self.output)
                .map_err(failed)?;
            if length != 0 {
                packets.push((self.emitted, self.output[..length].to_vec()));
                self.emitted = self
                    .emitted
                    .checked_add(FRAME_SAMPLES as u64)
                    .ok_or(Error::Input)?;
            }
        }
        Ok(packets)
    }

    /// Encoding is synchronous on the dedicated audio thread; no background
    /// units remain to collect. Partial PCM waits for the next complete frame.
    pub fn poll(&mut self) -> Result<Vec<(u64, Vec<u8>)>, Error> {
        Ok(Vec::new())
    }

    /// Codec declaration used by independent decoder regressions.
    #[doc(hidden)]
    pub fn audio_specific_config(&self) -> &[u8] {
        &self.codec_data
    }
}
