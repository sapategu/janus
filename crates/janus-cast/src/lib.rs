//! Direct native Cast protocol implementation. No browser, vendor SDK or cloud.
//! Wire/control parsing, nonce/TLS binding and offline device trust (path,
//! revocation and signature against embedded anchors) are implemented. A
//! verified device is still not a mirroring session; control and media remain.

pub mod auth;
pub mod connection;
pub mod control;
pub mod offer;
pub mod rtcp;
pub mod rtp;
pub mod session;
pub mod streaming;
pub mod trust;
pub mod wire;
