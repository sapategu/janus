# AAC-ELD codec provenance

Janus owns EShare RTSP, RTP-like framing, clocks, capture and Stop in Rust. The Linux audio encoder uses the safe FFI API of `fdk-aac-rust =0.2.3`, with `fdk-aac-rust-sys =0.2.3`; both package identities/checksums are in both Cargo lockfiles. Janus introduces no unsafe Rust code.

The sys crate fetches and compiles upstream FDK AAC at default commit `d8e6b1a3aa606c450241632b64b703f21ea31ce3` from https://github.com/mstorsjo/fdk-aac . It applies its packaged test-bridge patch and compiles a packaged QMF bridge. This is a third-party modified version of the Fraunhofer FDK AAC Codec Library for Android; Janus does not edit that source. The exact changes are supplied by fdk-aac-rust-sys 0.2.3 under `build-support/`. Package source: https://crates.io/crates/fdk-aac-rust-sys/0.2.3 .

Cold builds need Git/C++; runtime needs no service or codec process. For an existing offline source checkout, the dependency accepts `FDK_AAC_SOURCE_DIR`; it must correspond to the selected revision and dependency's patch contract. Environment overrides are not new Janus configuration or protocol fallbacks.

Native EShare encoder contract was read from the supplied macOS SDK: AOT 39 (AAC-ELD), 44100 Hz, stereo, 192000 bit/s, 480-sample granules, raw transport. Neither AAC-LC/1024 nor AAC-LD/512 is substituted for it.

The complete upstream license is retained in NOTICE.FDK-AAC. The RPM recipe installs it and this provenance alongside the existing AirPlay notices. Binary redistribution also requires making complete corresponding codec sources and modifications available to recipients, as stated by that license. Release qualification/source distribution remains tracked in docs/todo.md (D03); a local build or physical receiver test is not a public-release qualification.
