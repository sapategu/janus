# Legacy HTTP mirroring profile

Implemented in the development source on 9 November 2026. The installed/local RPM 0.1.4-2 contains this adapter. Do not describe the board as casting successfully until the physical media test passes.

## Evidence and profile selection

The Hisense board's advertised AirPlay endpoint passes `/info`, raw pairing and FairPlay but its RTSP type-110 SETUP times out. Correcting the empty control reply and timestamp descriptors did not resolve that timeout. Timing diagnostics showed zero valid 32-byte requests. The user's active FedoraWorkstation firewall already permits TCP/UDP 1025–65535; no firewall settings were changed.

A read-only GET on the standard older mirroring service, port 7100, returned `/stream.xml`: 1920×1080, refreshRate 1/60, overscanned false, version 200.54. Janus now probes that endpoint on the selected private IP before authenticating or sending media. A valid bounded capability plist selects `Profile::LegacyHttp`; an absent service leaves the existing RTSP profile. This is protocol/capability selection, never a model/name/address match or a retry after failed authentication. No HTTP redirect is followed. PIN/password indications still fail explicitly.

Sender order now follows OpenAirPlay `http_requests`: on the mirroring connection Janus sends `GET /stream.xml` (re-validated), both `/fp-setup`, then `POST /stream`. Codec data (type 1) is always the first media packet; heartbeats start only after the first video frame (OpenAirPlay `stream_packets`). The earlier engine sent heartbeats first; a loopback engine regression (`test_http_engine_sends_codec_data_before_any_heartbeat`) fails on that order (`[2, 2, 1]`) and passes now (`[1, 0, 2]`).

Diagnostics: the response monitor now records, without payloads, whether the receiver replied (status), closed (FIN), reset or sent non-HTTP bytes, with the time since stream start. A ready reply wins over local Stop (bounded 200 ms linger), and an explicit receiver rejection replaces a later local Broken pipe. Timing counts every datagram on the timing socket that is not a valid request from the receiver's address (`timing_ignored`). Acceptance prints all three.

Latest agent-run result (this host, packet capture): `/stream` accepted; board closes (FIN) 2 ms after the first codec + encrypted IDR packets; no reply, no UDP. See validation.md and the handoff for the current key-unwrap hypothesis.

Earlier user-run hardware result after the half-close correction: discovery, HTTP profile, raw key-possession proof, fresh FairPlay and local stream setup pass. Mirroring then fails with `Broken pipe (os error 32)`: 4 frames queued, 3 fully transmitted, 0 timing requests and 0 replies. Timing and physical teardown remain Pending. No displayed pixels were confirmed. Completed TCP writes establish transport progress, not receiver decryption, decoding or rendering.

## Native implementation and ownership

| Owner | Behavior |
| --- | --- |
| `janus-airplay/src/http.rs` | Bounded XML capability schema; legacy stream descriptor; fresh HTTP FairPlay/start; optional HTTP response reader; 48-byte NTP packet construction. |
| `wire.rs` | HTTP/1.1 Host/header framing alongside RTSP, shared bounded response parser, bounded plist event decoding. `into_mirroring` consumes the HTTP connection after sending `/stream`, preventing control reuse. |
| `session.rs` | Live profile qualification, advertised-control raw pairing proof and reusable fresh wrapped-key exchange. Existing RTSP shared-secret key derivation is preserved. |
| `video.rs` | Both profiles use the same bounded Annex-B/AVCC conversion. HTTP uses direct raw key/IV continuous AES-CTR, legacy type/flag values and no RTSP codec footer. |
| `streaming.rs` | Shared media engine, optional audio transport, protocol-owned lifecycle, response read owner, heartbeat/writer cancellation and fixed-port timing task ownership. |
| `janus-mirror` | Same portal capture, source consent, H.264 encoder and Stop. Reports actual profile, queued versus transmitted frames, timing counters and protocol-specific Stop acknowledgment availability. |
| `janus-cli/src/acceptance.rs` | Live capability/profile stages, explicit authentication/session/media/timing/Stop checks; unsupported acknowledgments stay Pending. |

The main advertised control endpoint still proves its raw Ed25519/X25519 key possession. The mirroring connection then completes its own fresh FairPlay SAP and wraps a fresh AES key on that same HTTP connection. The older profile uses the direct unwrapped key plus `param2` IV, without RTSP's stream-ID derivation. The control proof and FairPlay do not establish manufacturer identity; do not claim a Google Cast trust credential or a signed identity for the HTTP service. There is no clear-video path and no fallback after a failed proof/confirmation.

`POST /stream` carries a bounded binary plist: fresh locally administered sender device ID, signed 32-bit session ID, sender protocol version, wrapped `param1`, IV `param2`, latency and the published fps/timestamp descriptor names. The matching `X-Apple-Device-ID` header is generated locally, not from a user's physical MAC. The stream immediately follows; the historical protocol does not require an HTTP start response. Codec configuration remains clear as the protocol specifies; VCL access units are encrypted continuously across frame boundaries.

## Timing, Stop and errors

The HTTP profile uses standard 48-byte NTP on client port **7010**, with a session-relative clock shared with the video headers. This is distinct from the RTSP profile's 32-byte timing packets and NTP-epoch adjustment. Bind only the local interface address selected by the control connection. A conflicting port owner returns a concrete error; no guessed replacement port or shared listener is used. Startup owns the socket until upload begins, so cancellation releases it. The live engine aborts and joins its timing task before reporting completion, releasing the fixed port for another cast.

An independent reader handles optional HTTP replies. It bounds headers/bodies, allows a bounded number of success/interim replies, enforces a deadline once a partial reply starts, and stops on permission/status/framing failure. Video frame arrivals cannot cancel a partial reply. A clean read EOF after upload starts only ends the optional response direction: TCP permits half-close, so the media writer remains responsible for upload failure. A reproducing test failed with `Some(ReceiverEnded)` before this correction and passes afterward. One-second heartbeats also run before the first HTTP video frame, so write failure detection does not depend on changing capture content.

Stop invalidates local sends and interrupts a blocked writer first. The stream and authentication sockets close and timing ownership ends. This profile has no receiver TEARDOWN ACK: `receiver_stop_confirmed` is `None`, and acceptance's Stop result remains Pending physical observation. TCP writes and NTP responses are not evidence of rendered frames. Failure reports now include actual complete frames written and valid clock requests/replies, alongside queued encoder frames.

**Audio is not implemented for this HTTP profile.** Requested Sound returns an explicit error before capture; it is never silently omitted. Use Sound off for the current test. Existing RTSP ALAC and Google Cast Opus paths remain separate.

## Proof and host command

`janus-airplay/tests/test_http.rs` covers the published capability schema and hostile XML, wrapped-key-only preamble, direct continuous CTR/legacy headers, 48-byte clock fields, full scripted proof/FairPlay/upload without a start ACK, failed raw proof/confirmation, permission rejection, response timeout/cancellation and TCP half-close. The mirror integration test independently decrypts and decodes actual OpenH264 test-pattern output for both profiles.

Latest focused run: 34 AirPlay tests plus 3 mirror/selection tests pass. See [validation.md](validation.md) for final gates and the exact current host result.

```sh
cargo run --locked --bin janus -- diagnostics check-receivers \
  --name "Azuba-Board" \
  --seconds 12 --hold 0 \
  --mirror 10
```

Keep the board in normal LAN/AirPlay mode. Physical exit: observe the test pattern, clean Stop, repeatability and then real Screen/Window with Sound off.

## Sources

- [OpenAirPlay HTTP requests](https://openairplay.github.io/airplay-spec/screen_mirroring/http_requests.html), [stream packets](https://openairplay.github.io/airplay-spec/screen_mirroring/stream_packets.html), [time synchronization](https://openairplay.github.io/airplay-spec/screen_mirroring/time_synchronization.html).
- Historical independent receiver [espes/Slave-in-the-Magic-Mirror](https://github.com/espes/Slave-in-the-Magic-Mirror/tree/5a80c28824e804c6b526b65bbb84089f3b9d5391), commit `5a80c28824e804c6b526b65bbb84089f3b9d5391`, `airplay.py` and `drm.py`, inspected before implementation. This is a wire/crypto behavior reference; no code was copied from its GPL-2.0-or-later files, and its Apple binary, interpreter, Python runtime and VLC are not used by Janus.
- Existing native FairPlay port and independent Go vectors retain the sources and LGPL notices in [PROVENANCE.md](../crates/janus-airplay/PROVENANCE.md).
