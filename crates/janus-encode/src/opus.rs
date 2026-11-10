//! Opus audio for Cast Streaming: 48 kHz stereo, 10 ms frames (480 samples
//! per channel), the frame size Chrome's Cast sender uses. Input of any
//! length is buffered until whole frames are available.
use opus::{Application, Bitrate, Channels, Encoder};
use thiserror::Error;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 10 ms frame (also the RTP tick increment).
pub const FRAME_SAMPLES: usize = 480;
const MAX_PACKET_BYTES: usize = 4000;
/// Bound on buffered input (~200 ms); older audio is discarded beyond it.
const MAX_PENDING: usize = FRAME_SAMPLES * CHANNELS * 20;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Opus encoder failed: {0}")]
    Opus(String),
}

pub struct OpusEncoder {
    encoder: Encoder,
    pending: Vec<f32>,
    output: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(bit_rate: i32) -> Result<Self, Error> {
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio)
            .map_err(|error| Error::Opus(error.to_string()))?;
        encoder
            .set_bitrate(Bitrate::Bits(bit_rate))
            .map_err(|error| Error::Opus(error.to_string()))?;
        Ok(Self {
            encoder,
            pending: Vec::with_capacity(MAX_PENDING),
            output: vec![0; MAX_PACKET_BYTES],
        })
    }

    /// Adds interleaved stereo samples and returns every complete 10 ms
    /// packet. Each packet advances the RTP clock by [`FRAME_SAMPLES`].
    pub fn push(&mut self, interleaved: &[f32]) -> Result<Vec<Vec<u8>>, Error> {
        self.pending.extend_from_slice(interleaved);
        if self.pending.len() > MAX_PENDING {
            let excess = self.pending.len() - MAX_PENDING;
            self.pending.drain(..excess - excess % CHANNELS);
        }
        let frame = FRAME_SAMPLES * CHANNELS;
        let mut packets = Vec::new();
        let mut consumed = 0;
        while self.pending.len() - consumed >= frame {
            let length = self
                .encoder
                .encode_float(&self.pending[consumed..consumed + frame], &mut self.output)
                .map_err(|error| Error::Opus(error.to_string()))?;
            packets.push(self.output[..length].to_vec());
            consumed += frame;
        }
        self.pending.drain(..consumed);
        Ok(packets)
    }
}
