# Native AirPlay source and licensing

Janus implementation: 9 November 2026. This crate is a native Rust port, not a wrapper around another sender. Neither Go, GStreamer, FFmpeg, UxPlay nor an external sender process is required at runtime.

## Sources inspected before implementation

- [Doubletake](https://github.com/omarroth/doubletake/tree/5e2f1cbe3e1070bcefec8e549c7baf884727bc00), Omar Roth, commit `5e2f1cbe3e1070bcefec8e549c7baf884727bc00`: `internal/airplay/client.go`, `pairing.go`, `mirror.go`, `audio.go`, `fpsap.go`, `fpsap_tables.go`, `fairplay_crypto.go`, `fairplay_md5.go`, `fairplay_message.go`, `fairplay_sap.go` and `fpsap_test.go`.
- [Rust SAP core](https://github.com/objevovat/fairplay-sap-core-airplay2-sender-authentication-handshake/blob/v1.1.0/ports/rust/fairplay_sapcore.rs), objevovat, tag `v1.1.0`: byte helpers, SAP hash and nonlinear circuit adapted into `src/sap.rs`. This source itself derives from Doubletake `8ccea5f`. Its original licensing/provenance document is preserved as [NOTICE.upstream.md](NOTICE.upstream.md); links within that copied document refer to its original upstream repository.
- [UxPlay raop_handlers.h](https://github.com/FDH2/UxPlay/blob/4e95a9ec013c52d8555d88dbc55abe346e94d56e/lib/raop_handlers.h) and `lib/mirror_buffer.c`: independent receiver-side checks of wrapped keys, stream-ID key derivation and continuous AES-CTR state. No UxPlay code was incorporated. No UxPlay executable was run in this session.
- [Apple ALACEncoder.cpp](https://github.com/macosforge/alac/blob/c38887c5c5e64a4b31108733bd79ca9b2496d987/codec/ALACEncoder.cpp), `EncodeStereoEscape` and element framing: protocol reference for the small ALAC escape encoder. No Apple encoder implementation or library is bundled.

## What was ported

`fairplay.rs` ports the stateful m1/m2/m3/m4 exchange, modified MD5, message transform, descriptor, white-box networks and random-key wrapping. `tables.rs` preserves Doubletake's numeric data, converting Go array literals to Rust; dimensions are checked during conversion. `sap.rs` retains explicit wrapping arithmetic, including the intentional unsigned underflow in ring indices.

Session and media code follow the inspected protocol flows, with Janus-owned bounds, cancellation and task ownership. Legacy control carries the wrapped `ekey`, never a clear raw key in `shk`. No debug output includes keys, SAPs, credentials, payloads or session identifiers.

The board-startup follow-up aligned video SETUP's `timestampInfo` array with the same pinned Doubletake `mirror.go::videoStreamDesc`: `SubSu`, `BePxT`, `AfPxT`, `BefEn`, `EmEnc`. The [legacy mirroring request trace](https://air-display.github.io/airplay-internal/mirroring_service.html) independently shows those names. Tests check the request descriptor; the board's previous video-SETUP timeout is not yet proven to be caused by their omission.

## HomeKit-style pairing and paired mirroring (10 November 2026)

Ported from the same pinned Doubletake commit `5e2f1cb`: `internal/airplay/pairing.go` (TLV8, SRP-6a 3072/SHA-512, PIN pair-setup M1–M6, pair-verify V1–V4, HKP pairing types), `client.go` (`encrypt`, `readEncryptedFrame`: HAP channel framing), `event_channel.go` (reversed Events keys, RTSP 200 replies), `mirror.go` (`setupMirrorSession` SETUP order and descriptors, `sendFrame` ChaCha20-Poly1305 with the header as AAD, `deriveChaChaKey`), `fairplay.go` (`deriveStreamKeys` shk/shiv without FairPlay), `discovery.go`/`compatibility.go` (feature predicates; NTP rather than PTP for sourceVersion 377.40.x) and `latency.go` (Wi-Fi playout target).

Deliberate differences: Janus verifies the receiver's long-term Ed25519 signature in pair-setup M6 and in every pair-verify V2 against the key stored at pairing, requires the SRP server proof in M4, and never falls back to an unpaired or unencrypted path. The SRP vector in `testdata/srp_vector.txt` was produced by a separate Python implementation that computes both the client and the server side and asserts they derive the same premaster secret; `tests/test_hap.rs` also runs the client against an independently written accessory (server) implementation.

## Licenses and redistribution

The crate as a whole uses **LGPL-3.0-only**, matching the conservative reading of Doubletake's LGPL-3.0 license. The adapted SAP file retains its upstream **LGPL-3.0-or-later** notice; that does not broaden the rest of this crate. [LICENSE](LICENSE) and [COPYING.GPL](COPYING.GPL) preserve the applicable texts. Changes include Rust translation, bounds, state validation and integration into Janus. This does not relicense the rest of Janus or third-party dependencies.

A Rust translation remains derived code. A binary distribution containing this crate must retain notices and meet LGPLv3's corresponding-source and relinking requirements, including the application material needed for the chosen static linking/LTO arrangement. Preparing that source/relink material is a release gate; local RPMs now include the adapter's license/provenance texts, while complete source/relink material and external distribution qualification remain open. There is no external helper-binary exception or permissive-license claim for this port.

## Reproducible reference vectors

`tests/test_fairplay.rs` checks Doubletake's expanded 90,128-byte white-box table digest and six public exchange vectors, plus descriptor and wrapping vectors. The local SAP fixture is public upstream test material, not a user's receiver certificate or identifier.

The new wrapping vector was produced by separately executing the pinned Go implementation with its standard library only:

1. `newFPSAPSession`: 126 entropy bytes all `0x41` (SAP starts `00 01`).
2. m2: FPLY version 3/type 2, payload length 130, payload marker 2, mode 3, remaining 128 bytes zero. Call `exchangeM3`.
3. Raw key: bytes `00` through `0f`; 16 key-mask entropy bytes all `0x5a`. Call `wrapKey`; compare the full m3 and 72-byte wrapped-key record with Rust.

The normalized source-text snapshots used for that Go execution had these SHA-256 hashes (line-number markup removed from the web tool, trailing newline normalized; these are not claims about Git blob hashes):

| File | SHA-256 |
| --- | --- |
| `fpsap.go` | `a9e37932db6d363d2b29cc61e661ca6c83884b6d20c808188f56b183df3336e9` |
| `fairplay_crypto.go` | `b2c827ffec241aae163ed71679803dc0a7cf4076457f9c2d202a1902df5a257e` |
| `fairplay_md5.go` | `64e690fed6392f5317d67f44065708f24ce8bcdc65db29315fbd6ad213c68ef6` |
| `fairplay_message.go` | `ae5544835fba99c619e3e2fa2d13ec62885dcc783ff3dd6a4e76fb7db1b77cff` |
| `fairplay_sap.go` | `3b409bab1d1f5b9df2dcce312c2f9eda1d64eb5394112bd9d472b60a8995903a` |
| `fpsap_tables.go` | `6dd283ae50635f3313ea204fc9b21a15f5975df2de37ffecbb3665e5e158e5a4` |

Go is a development cross-check only. Normal builds/tests use committed Rust source and vectors. Passing these tests does not establish physical Hisense, Apple TV, UxPlay, Wi-Fi, capture, audio-quality or efficiency qualification.
