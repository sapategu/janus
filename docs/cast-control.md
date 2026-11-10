# Native Cast control implementation

AirPlay uses the separate `janus-airplay` legacy adapter and the shared mirror pipeline. Its implementation, source/license provenance and generic acceptance stages are documented in [airplay-control.md](airplay-control.md). It does not reuse Cast device trust, session types, media framing or frame-ACK criteria.

`janus-cast` implements wire/control/authentication processing, offline device trust, owned live sessions and Cast Streaming directly in Rust, with no browser, vendor SDK, account or cloud calls. The sections below describe the implemented path and its evidence boundaries.

## Implemented and tested

- Cast V2 `u32be length | Protobuf` framing, capped at 65,536 bytes including envelope overhead. Oversized prefixes fail before body allocation.
- Explicit presence checks for required proto2 fields, bounded endpoint/namespace identifiers, exact text/binary payload kind, and rejection of unsupported wire revisions/chunking.
- Read-only receiver status requests, correlation, bounded application/namespace lists, valid volume range, heartbeat Ping/Pong and virtual connection Close interpretation.
- An active receiver application is observed only. No automatic launch, stop-other-app, URL load or volume mutation is implemented.
- Fresh 16-byte CSPRNG authentication challenge per attempt, SHA-256 requested, three-second deadline, response routing, exact nonce echo and no silent SHA-1 downgrade.
- Bounded response credentials and signature transcript `sender_nonce || actual_TLS_leaf_DER`. Challenge consumption prevents processing it twice through the same owner.
- Default Debug output redacts message payloads, application/session identifiers and authentication inputs.

The parsed type is deliberately `UnverifiedCredentials`. Structurally valid bytes, a matching nonce, a TLS connection or DNS discovery never produce an authenticated state; only `trust::verify` does.

## Offline device trust (`janus_cast::trust`)

Implemented and verified against the real AzubaTV (see validation.md). No account, cloud or SDK call; trust data is embedded and its provenance/hashes recorded in `crates/janus-cast/trust/README.md`.

- **Anchors:** Cast Root CA and Eureka Root CA for devices; Cast CRL Root CA for revocation lists. Extracted byte-for-byte from Open Screen.
- **Path rules (Open Screen `BoringSSLTrustStore`):** depth-first issuer search over at most five supplied intermediates, anchors first; canonical name matching; target time-valid, RSA ≥2048, matching outer/inner signature algorithm and keyUsage digitalSignature. Each issuer must be time-valid (anchor exempt), keyCertSign if keyUsage present, CA basic constraint, pathlen ≤255 enforced including the anchor's, RSA ≥2048, no unhandled critical extension and no anyPolicy mapping. Chain links accept RSA PKCS#1 v1.5 SHA-1/256/384/512 (existing roots are SHA-1 signed). Directory-name constraints are enforced; any other name-constraint form fails closed.
- **Revocation (Chromium `CRL_REQUIRED_WITH_FALLBACK`):** the receiver's `CrlBundle` is verified (version 0, signer path to the CRL root, RSA SHA-256 signature, validity window, expiry capped by signer). If absent/invalid, the embedded fallback CRL is used. Its signer is checked at its published time and its validity window is Janus's own 20 weeks from `FALLBACK_CRL_REFRESHED_UNIX` (expires 2027-03-29). Revoked SPKI SHA-256 hashes and per-issuer serial ranges are checked from anchor to device. If neither CRL is usable, verification fails closed (`RevocationUnavailable`).
- **Signature:** RSASSA-PKCS1-v1_5 SHA-256 by the device key over `sender_nonce || TLS leaf DER` of this exact channel.
- **Policy:** the audio-only OID 1.3.6.1.4.1.11129.2.5.2 anywhere in the path yields `DevicePolicy::AudioOnly`; such receivers must never be offered a screen.
- `VerifiedDevice` can only be produced by `trust::verify`. The hidden `trust::testing` module used by integration tests returns plain facts and cannot mint it.

`connection::probe_authentication` now returns a `VerifiedDevice` and still closes the channel. Help → **Check connection** shows "Genuine Cast device verified", an audio-only notice, or a specific failure (revoked, revocation data out of date, clock wrong, not genuine). `janus diagnostics cast-check <ip>:8009` runs the same read-only check from the CLI.

## Live receiver session (`janus_cast::session`)

`Session::start(endpoints, SessionConfig, runtime)` connects, verifies device trust (`connection::connect_verified`, crate-only), then keeps the channel as an app-owned, read-only session:

- Opens the platform virtual connection (`CONNECT` to `receiver-0`, strong), then `GET_STATUS` and `GET_APP_AVAILABILITY` for the Cast Streaming mirroring apps `0F5096E8` (screen) and `85CDB22F` (audio). Formats follow Chromium `cast_message_util.cc`.
- Sends `PING` every 5 s, answers receiver `PING`, and treats 10 s without any message as lost (Chromium defaults).
- Publishes a latest-value `Snapshot`: phase, generation, device policy, whether a receiver app is active, volume, screen/audio mirroring capability, last loss and heartbeat count. An audio-only certificate forces screen mirroring to unavailable. `ready_for_screen()` requires a connected, unrestricted device whose receiver reports the screen mirroring app.
- Losses (timeout, receiver CLOSE, disconnect, malformed-message flood) reconnect with bounded backoff and a fresh trust check. The retry budget refills only after a generation receives a heartbeat reply, so a receiver that accepts and then goes silent cannot flap forever. Address, permission, TLS, authentication and trust failures are final.
- `close()` sends `CLOSE` and shuts the stream; dropping the handle aborts and closes.
- Never launches, stops or controls a receiver app, and changes no volume: the TV keeps its current app until the user explicitly casts.

Help → **Check connection** now starts a live session for the selected TV and shows its state; the button becomes **Disconnect**. Changing target, leaving Help, Escape or Stop closes it.

## Generic acceptance

`janus diagnostics check-receivers [--seconds 5] [--hold 12] [--name <exact>]` discovers every receiver and runs, per Google Cast receiver: discovery → device-trust → capability → session (held with heartbeats) → mirroring (pending). It exits non-zero if no receiver is found or any implemented stage fails. `crates/janus-cli/tests/test_native_receivers.rs` runs the same code as an opt-in test. Janus-native receivers report their session stage as pending until that adapter exists.

## Casting (`Session::cast`) and Cast Streaming

Only an explicit user Cast changes the TV. `Session::cast(streams)`:

1. Sends `LAUNCH` for `0F5096E8` (screen; `85CDB22F` audio-only) and waits (≤ `cast_timeout`, default 20 s) for a `RECEIVER_STATUS` listing it; `LAUNCH_ERROR`/`INVALID_REQUEST` fail the cast.
2. Opens a virtual connection to the app's `transportId` and sends the OFFER on `urn:x-cast:com.google.cast.webrtc` (`castMode: mirroring`; per-stream random AES key/IV mask; payload types video 96 / audio 127 as Chrome uses for Android/Google TV; H.264 90 kHz, Opus 48 kHz stereo).
3. Accepts only the ANSWER for its `seqNum` whose indexes/SSRCs match the offer, then starts the `Streamer` to the receiver's IP and answered UDP port.
4. `stop_cast()`, a rejected ANSWER, a timeout or closing the session sends `STOP` + transport `CLOSE`; a status without the app (or its CLOSE) reports `EndedByReceiver`.

`streaming::Streamer` (one task, one connected UDP socket): AES-128-CTR per frame (nonce = IV mask ⊕ frame ID), Cast RTP packetization (≤1472-byte packets), 10 ms bursts at ≈24 Mbps, sender report before the first media and every 500 ms, RTT from receiver reports, checkpoint/CST2 ACK release, NACK retransmit skipping packets sent within one RTT, PLI and dropped-frame key-frame requests, kickstart of the newest frame, 120-frame ID span and in-flight duration window. Sources: Open Screen `sender_impl.cc`, `rtp_packetizer.cc`, `frame_crypto.cc`, `rtp_wire.rs`, `offer_messages.cc`, `answer_messages.cc`, `sender_session.cc` at 308a18e2.

`janus-mirror` feeds it: portal capture (or test pattern) → BGRA ≤1080p → OpenH264 Constrained Baseline (IDR on request, every 10 s), and PipeWire desktop monitor → Opus 10 ms.

Still open: the first real-TV cast; VA-API/DMA-BUF; VP8 fallback if a receiver rejects H.264; adaptive bitrate.

`connection::probe_authentication` tries at most eight advertised endpoints with one-second TCP deadlines and an eight-second total bound. TLS and auth each have three-second limits. TLS 1.2/1.3 handshake signatures are verified by rustls; resumption, early data and SNI are disabled. A private Cast-specific verifier bounds certificates, parses the exact DER and checks validity plus at most four days of remaining lifetime (Chromium's temporary TLS policy). Device identity is then established by the signed transcript above, not by the TLS certificate. Janus's optional open-receive verifier is never reused.

Only a fresh challenge is written; at most eight unrelated frames are ignored. The channel is dropped on success, error, timeout or cancellation; no reusable transport handle escapes. No CONNECT/GET_STATUS, heartbeat response, receiver mutation or media is sent.

Changing the selected target/addresses, leaving Help, Escape or window closure cancels the check; stale generations cannot overwrite the new state. Cast remains disabled until the mirroring path exists.

## Proof

```sh
cargo test -p janus-cast --locked
janus diagnostics check-receivers [--mirror 10]
cargo test -p janus-cli --test test_native_receivers -- --ignored --nocapture
janus cast [--name "<screen>"] [--source screen|window|test-pattern] [--no-audio]
```

`tests/test_streaming.rs` (8): OFFER/ANSWER fields and validation, NIST SP 800-38A CTR vector with the frame-ID nonce, packets parsed with the receiver's rules, sender reports, compound feedback (NACK bitvectors, CST2 ACKs, 8-bit expansion, PLI), loopback NACK/ACK/PLI, in-flight drops requesting key frames. `tests/test_cast_session.rs` (5): read-only until Cast, full LAUNCH → OFFER → ANSWER → UDP media → STOP, launch error/rejected answer/timeout, receiver-ended cast, audio-only and single-cast guards. `janus-mirror/tests/test_mirror.rs`: test pattern → decodable H.264 at a scripted receiver.

`tests/test_session.rs` (11 tests) drives the session against a scripted in-memory receiver: read-only message set, capability and audio-only override, receiver PING, liveness loss and bounded reconnect, receiver CLOSE, malformed flood, final trust failure, bounded unreachable retries and drop-close. `tests/test_control.rs` covers CONNECT/CLOSE/PING formats and bounded, correlated app-availability.

`tests/test_trust.rs` (10 tests) checks real public Cast chains (Chromecast Gen1/Gen2, Nexus Player, Vizio, Chromecast Audio), a real device SHA-256 signature, policy/name-constraint/pathlen vectors, the embedded fallback CRL across its window, full verification fail-closed paths and all 32 cases of Open Screen's CRL conformance suite. Eighteen unit tests cover framing/required fields, payload bounds, framed duplex status/heartbeat exchange, correlation/routing, active/idle status, malformed/flooded input, fresh nonce, expired/replayed/wrong-route replies and downgraded authentication algorithms. New tests run actual TLS 1.2/1.3 over an in-memory duplex stream and check exact TLS-leaf transcript binding, expired/future/long-lived certificates, DER bounds/trailing input, unrelated-message flood limits, timeout, cancellation and channel closure without control/media writes. Unit tests use synthetic peers; real receivers are validated by the generic LAN sweep and the recorded runs in validation.md.

## Primary references

- [Open Screen channel schema](https://chromium.googlesource.com/openscreen/+/refs/heads/main/cast/common/channel/proto/cast_channel.proto), inspected blob `a1ecf3471a642a5a639d71cd4feb98e7ec1e0dca`.
- [Chromium authentication transcript and TLS lifetime handling](https://raw.githubusercontent.com/chromium/chromium/main/components/media_router/common/providers/cast/channel/cast_auth_util.cc).
- [Open Screen device certificate, revocation and audio policy](https://raw.githubusercontent.com/chromium/openscreen/main/cast/common/certificate/cast_cert_validator.cc), path builder `boringssl_trust_store.cc` and `cast_crl.cc` at commit `308a18e24e717ce27231c0aaa713364a41416e54`.
- [Chromium CRL fallback policy](https://raw.githubusercontent.com/chromium/chromium/main/components/media_router/common/providers/cast/certificate/cast_crl.cc) at commit `256d14612ecd3ab6b42283197a5d34a3d9f07008`.

Protocol field assignments follow these primary sources; the implementation is native Rust rather than linking their SDK/runtime. Authentication policy and hardware interoperability remain explicit qualification work.


## Hisense / EShare: separate control endpoint

Supported natively since 10 November 2026 (user decision): there is no Settings switch, `--hisense` flag or `JANUS_HISENSE` variable any more. Discovery always listens on the EShare UDP port, and every non-HAP AirPlay screen receiver gets the bounded identity probe below. An explicit EShare server header, including the bounded HTTP HEAD probe on TCP 8000, is required. `AirTunes/211.3`, an Apple sourceVersion or a listening TCP port alone does not select this unencrypted profile. Explicit AirPlay password/pairing requirements and authentication rejections are preserved.

On explicit Cast, the Hisense adapter tries TCP 51040 for EShare RTSP, retaining the advertised connection only if primary TCP establishment fails. RTSP errors on the selected service are final. OPTIONS → single video SETUP (`type=110`, binary plist) → negotiated video dataPort; the existing Annex-B/128-byte-header sender owns media and local-first Stop. No HAP/FairPlay claim is made.

With Sound on, video TCP is connected before a second RTSP SETUP negotiates audio (`streams: [{type:96, audioFormat:0x01000000}]`). Audio uses the returned `dataPort`; one observed response returned 54022 and `controlPort` 54756. These are negotiated session values. Janus does not hardcode an audio UDP port or reuse AirPlay's ALAC/RTCP path. The native contract requires desktop-monitor PCM at 44,100 Hz, stereo **AAC-ELD (AOT 39), 192 kbit/s, 480-sample granules**, sent as raw access units after a 12-byte native header: `80 60`, big-endian sequence, big-endian millisecond timestamp, then constant bytes `01 00 00 00`. No ADTS or AU-length header is added. Native `ESAudioSession RTPPackAndSend:withlen:` at `0x32fb4`, particularly the byte-8 store at `0x3306e`, corrects the earlier research's SSRC byte order. `RTSPClient setupAudio` at `0xa4c43` supplies the SETUP fields; retained [audio disassembly](../research/extracted/overseas_mac/audio_20261110.txt) is static evidence, not an observed vendor-client session.

The initial implementation wrongly used GStreamer's libav AAC-LC encoder at 128 kbit/s with 1024-sample frames. Fresh `FdkAACEncoder` analysis shows the native default bytes at VA `0x1dbf80` are `0200000044ac000000ee020027000000` (little-endian channels 2, rate 44100, bitrate 192000, AOT 39); `initEncoder` sets granule 480 at `0x42ddb`. The exact ELD fix is in progress. The earlier libav dependency/RPM Release 3 changes describe the initial attempt, not a verified compatible encoder or rebuilt RPM. The EShare profile is unencrypted; Help/README carry the trusted-LAN limit.

### EShare video quality (10 November 2026)

The board answers every keep-alive `OPTIONS` (2 s) with its encoder wishes, captured on the physical board: `{"Framerate":"15","idr_req":"0","casting_win_width":"3840","casting_win_height":"2160","bitrate":"0","i-interval":"0",…}` plus `Video-Audio: <video>-<audio>` receive counters. Native `-[RTSPClient checkChangeEncoderAttribute:]` applies these; Janus now honours `idr_req` (bounded parser, `testing::idr_requested`) by forcing an IDR. Native `-[VideoDisplaySession initDynamicBitrateControl]` (EShareSDK, overseas macOS) starts near 6.7 Mbit/s at ≤1080p, with min/max bounds and a 1 s `checkNetworkQuality` that lowers the bitrate when the socket write queue grows. Native uses H.264 Baseline (`kVTProfileLevel_H264_Baseline_AutoLevel`), so Janus keeps Constrained Baseline.

Janus changes, shared by every AirPlay profile:
- GPU encoder VBR (target = plan, peak 2×) with `max-qp=38` instead of CBR; balanced `target-usage=4` at ≤1080p. CBR gave a screen change ~1/30 of a second's budget, so new text stayed blurry.
- When the picture stops changing, the last frame is re-encoded for ~0.5 s so it refines. Before, only the 1 s static resend refined it.
- Back-pressure: encoded units wait (≤8) for room in the 2-frame TCP queue, and no new frame is encoded meanwhile. Before, a full queue dropped the frame and forced an IDR, which was the most starved frame of all.
- Network-following bitrate (GPU): −20 % per second when >10 % of ticks were blocked (floor plan/4, ≥1 Mbit/s), +10 % per clear second up to the plan.

Canvas: the probe sends one `OPTIONS` to TCP 51040 before any SETUP (verified on the board: `CastingNumber: 0`, nothing starts) and uses `casting_win_width/ height` (bounded 320×240–7680×4320, even) as the receiver maximum instead of the AirPlay `/info` 1920×1080. Resolution choices up to 4K now apply; a cast still never upscales beyond the source. The board's `Framerate` (15) is recorded but not applied: its `Video-Audio` counters advanced ~25–30 video frames/s at Janus's 30 fps, so motion stays at 30.

Hisense generic acceptance adds an audio stage: read-only stays Pending; explicit mirroring requests audio when live capability supports it. Zero actual transmitted audio packets fail. Positive counts remain Pending with physical sound confirmation required; queued packets are not playback acknowledgements. Other AirPlay profiles keep their existing acceptance sound behavior.

Native macOS `RTSPClient startMrror` calls `getDynamicPort_51040`; `teardown` sends only `streams: [{type:110}]`; `stopMirror` calls `disconnectAfterWriting`. The real board responds to Janus's TEARDOWN with FIN rather than an RTSP reply. Acceptance therefore records local close with protocol acknowledgement Pending; it never treats FIN as RTSP acknowledgement. Rendering and physical Stop need independent observation; both were confirmed for the three-second pattern run. The later first eight-second AAC run exited 0 with a last checkpoint of 118 video frames and 253 queued audio packets; the user confirmed picture, sound and Stop. The user subsequently reported distorted, crackling, choppy sound in the window test. This is a playback-quality failure, not simply sync error. The earlier independent decoder test established only that the incorrectly chosen LC stream decoded as LC with stereo 440/880-Hz separation; it did not validate the board's ELD requirement. Header bytes, ordering and negotiated ports remain supported evidence. Corrected ELD codec proof and physical sound retest are pending; do not claim verified audio quality before that retest. See [binary evidence and boundaries](hisense_protocol_analysis.md) and [reproducible results](validation.md). No production device-name/address match.

## Sender name (10 November 2026)

Every receiver sees this computer as `janus@<short hostname>` (e.g. `janus@fedora`), from `janus_media::identity::sender_name()`. The name is used in the Google Cast CONNECT `userAgent`, the AirPlay SETUP `name` (legacy and paired) and the HomeKit pairing `X-Apple-Client-Name`, and in Hisense/EShare's SETUP `machine_name`, `DeviceName` header, `sayHello`, master-control `clientName`/`reportClientInfo` and `disconnect` lines. Only ASCII letters, digits, `-` and `_` (at most 48) are kept, so the name is safe in JSON, plists and RTSP headers. It is a display name only; AirPlay pairing identities are stored separately, so existing pairings stay valid.
