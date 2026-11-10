//! Pure scheduling and timing policies. No capture/codec/transport is simulated
//! behind a production interface; diagnostics drive these policies explicitly.

pub mod clock;
pub mod identity;
pub mod quality;
pub mod video;
