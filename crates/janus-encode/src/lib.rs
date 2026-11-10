//! Native media encoding for casting. `video` selects the GPU encoder
//! (`gpu`, VA-API through GStreamer, Linux) or the bundled OpenH264 encoder
//! (`h264`, Constrained Baseline, Annex-B access units), `scale` converts
//! captured 32-bit frames to the encoder's size, `opus` encodes 10 ms
//! desktop-audio frames, `aac` encodes EShare AAC-ELD (safe FDK wrapper, Linux),
//! and `pattern` draws a moving
//! test image for pipeline checks without screen capture.

#[cfg(target_os = "linux")]
pub mod aac;
#[cfg(target_os = "linux")]
pub mod gpu;
pub mod h264;
pub mod opus;
pub mod pattern;
pub mod scale;
pub mod video;
