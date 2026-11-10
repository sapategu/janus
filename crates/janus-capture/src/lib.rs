//! Native screen/window capture for casting.
//!
//! Linux: the xdg-desktop-portal ScreenCast picker (the user chooses a whole
//! monitor or one application window; Janus never bypasses it) feeds a
//! PipeWire video stream consumed on a dedicated thread (`video`). Desktop
//! audio comes from the default output's monitor (`audio`). Frames and
//! audio are handed over latest-value / bounded, so a slow encoder never
//! builds an unbounded queue.

#[cfg(target_os = "linux")]
pub mod audio;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod video;

use std::time::Instant;
use thiserror::Error;

/// What the user is asked to pick in the system dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A whole monitor.
    Screen,
    /// One application window.
    Window,
}

/// Byte order of a packed 32-bit pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelLayout {
    /// B, G, R, X/A
    Bgrx,
    /// R, G, B, X/A
    Rgbx,
    /// X/A, R, G, B
    Xrgb,
    /// X/A, B, G, R
    Xbgr,
}

/// One captured video frame (owned copy of the PipeWire buffer).
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: usize,
    pub layout: PixelLayout,
    pub data: Vec<u8>,
    pub captured: Instant,
    /// Increments per delivered frame.
    pub sequence: u64,
}

/// Interleaved stereo f32 samples at the capture stream's requested rate.
#[derive(Clone, Debug)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub captured: Instant,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CaptureError {
    #[error("screen capture was cancelled")]
    Cancelled,
    #[error("screen capture isn't available: {0}")]
    Unavailable(String),
    #[error("PipeWire failed: {0}")]
    PipeWire(String),
    #[error("screen capture isn't supported on this platform yet")]
    Unsupported,
}
