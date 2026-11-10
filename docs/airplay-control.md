# Native legacy AirPlay implementation

Implemented 9 November 2026. Protocol tests and native Linux build are verified;
the Hisense 65WR6CE physical cast remains unverified. Prior board observations
are in [receiver-compatibility.md](receiver-compatibility.md).
Current failure, implementation map and next steps: [AirPlay handoff](airplay-handoff.md).
The RTSP path still times out at video SETUP after descriptor fixes. A live
`/stream.xml` probe confirmed a separate older HTTP profile, now implemented;
its latest host run passes authentication/session but fails with Broken pipe
after 3 transmitted frames; no rendering is confirmed.
See [HTTP profile and current media diagnosis](airplay-http.md).

## Supported slice

`janus-airplay` owns bounded RTSP/binary plist, raw transient Ed25519/X25519
pair-setup/verify, FairPlay SAP, NTP timing, H.264 over continuous AES-CTR/TCP,
optional 44.1 kHz stereo ALAC over RTP/AES-CBC, feedback and teardown. This is
raw-paired RTSP AirPlay with no PIN/password prompt. AirPlay 2 receivers (HomeKit
PIN pairing, encrypted control, ChaCha20-Poly1305 video/sound) use the paired
profile: `hap`, `secure`, `pairings`, `session::negotiate_hap` (see AGENTS.md §5).
Unsupported pairing or audio profiles return concrete errors. No security-off
retry, OS casting panel, proprietary client, browser or receiver app is used.

`janus_mirror::start_receiver` is shared by CLI and GPUI. Cast keeps its existing
trust/session/transport. AirPlay uses its own adapter and the same portal
screen/window source, scaling and OpenH264 encoder. PipeWire converts audio to
the protocol's requested rate; Cast continues to use 48 kHz Opus. Screen and
window both expose Sound; window capture still includes system-wide sound when
enabled. macOS/Windows real capture/audio remain unsupported.

ALAC uses the standard uncompressed escape form (352 stereo samples per packet),
about 1.42 Mb/s before packet overhead. This avoids a new native codec dependency;
it is not a claim of compressed-ALAC efficiency. No latency/battery/CPU result is
claimed. Video remains software OpenH264 at at most 1080p; hardware encoding and
physical quality qualification remain separate roadmap work.

## Negotiation and invariants

1. Read-only Help → Check connection calls `GET /info` and closes. Discovery
   output hints cannot authenticate a receiver or authorize media. Live info
   must advertise screen mirroring; explicit malformed security flags fail.
2. Explicit Cast performs raw key exchange and verifies the receiver's Ed25519
   proof, then completes fresh FairPlay m1–m4 and wraps a fresh stream key.
   This proves session key possession, not manufacturer identity. It does not
   create/reuse a Google Cast `VerifiedDevice` or bypass Cast trust checks.
3. SETUP creates NTP/session state, optional ALAC audio, then type-110 video.
   A successful control-only SETUP may have an empty body: its event/timing
   metadata is optional. Nonempty control replies must still be valid bounded
   binary plists; audio/video SETUP must return their required stream ports.
   Video SETUP includes the pinned sender's five named timestamp descriptors.
   Only a narrow explicit SETUP-shape rejection allows one media-first attempt;
   authentication/proof/transport failures never trigger a different security
   protocol. A control `skipRecord` waiver survives later audio responses.
4. Data/event connections use only ports returned by that receiver, on the same
   selected IP. RECORD follows transport connection, unless waived. Legacy
   plaintext control contains a wrapped key, never a clear `shk` key. Identifiers,
   SAP, keys and credentials are not logged or exposed by Debug.
5. Media queues are bounded (two video frames, sixteen audio packets); video
   frames are at most 4 MiB. A dropped video frame forces IDR. Control feedback
   has a separate owner so a slow RTSP response cannot block the media writer.
   Audio retransmission history and response budgets are bounded.
6. Stop invalidates local sends first, signals capture shutdown and interrupts
   blocked TCP media writes; TEARDOWN is attempted with a short deadline. A partial/timed-out
   control exchange poisons its connection, so teardown may be unacknowledged
   and closing the connection is mandatory. Cancel aborts pending source/session
   work; stale UI generations discard and close any completed handle.

RTSP headers are limited to 8 KiB/48 fields and bodies to 64 KiB; duplicate
headers, conflicting framing and mismatched CSeq fail. Binary plists have
limits on depth, collections, event count and aggregate scalar data. `plist`
`=1.10.1` is pinned; its event API is enabled to enforce those bounds before
building values. The crate adds no OS package prerequisite. Both lockfiles
retain the existing dependency pins.

Response errors identify the operation (control/audio/video SETUP, pairing or
capability check) and a fixed validation reason. They never include response
payloads, session URIs or keys. User-supplied board output on 9 November 2026
confirmed a successful but empty control-SETUP reply; this is handled without
changing authentication or retrying a different security profile.
Request timeout errors also identify the operation; TCP transport connection
deadlines distinguish video and event channels. Incomplete control exchanges
remain poisoned after timeout and are never reused.

## Acceptance and runnable host test

```sh
cargo run --locked --bin janus -- diagnostics check-receivers --name "Azuba-Board"
cargo run --locked --bin janus -- cast --name "Azuba-Board" --source test-pattern --no-audio --seconds 10
cargo run --locked --bin janus -- cast --name "Azuba-Board" --source screen --no-audio --seconds 10
cargo run --locked --bin janus -- cast --name "Azuba-Board" --source window --seconds 10
./scripts/build.sh desktop --run
```

The name is optional targeting, never a production device match. Duplicate names
are rejected by CLI selection; the desktop picker uses the candidate's browse
key. Leave the board in its normal LAN/AirPlay sharing mode, rather than the
Projection Display mode that disconnected Wi-Fi in the prior field test.

Generic acceptance now runs AirPlay discovery → live capability; an explicit
`--mirror 10` also runs authentication/session, test-pattern transmission and
receiver-acknowledged teardown. Its mirroring outcome stays **Pending** after
successful transmission: AirPlay has no Cast-style frame ACK, and writing TCP
bytes does not prove displayed pixels. Physical observation must confirm picture,
sound, Stop and repeated screen/window starts. New hardware/protocol failures
remain failures, not synthetic passes.

Sources, full upstream revision and LGPL notices:
[janus-airplay/PROVENANCE.md](../crates/janus-airplay/PROVENANCE.md).
