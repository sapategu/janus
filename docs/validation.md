# Verification record — 9 November 2026

## Native UI and RPM updates — 0.1.1 through 0.1.3

User screenshots confirm the supplied header logo and the first inline
receiver-control layout, including the Window/system-wide Sound notice. They
also show the same TV advertised through separate AirPlay and Google Cast
services; Janus keeps their browse identities separate.

Implemented afterward: compact protocol pills; Screen/Window, Sound and Cast
owned by each receiver card; independent choices keyed by browse identity;
44 px keyboard buttons, focus outline, Tab/Shift+Tab, Enter/Space and Page Up/Down;
one-shot 140 ms hover fill easing. Refresh is now top-right in the fixed Screens
heading. The receiver list scrolls within the card while the logo/navigation
remain fixed. Latest scrolling/pill appearance and screen-reader behavior are
not verified by a native screenshot yet; the sandbox still has no compositor.

| Check | Observed result |
| --- | --- |
| Core/app version and both lockfiles | 0.1.3. Parsed before/after lock entries confirm only local Janus versions changed; dependency graph/pins unchanged. |
| `./scripts/build.sh rpm` | Built 0.1.1-1, 0.1.2-1 and current `target/rpm/RPMS/x86_64/janus-0.1.3-1.fc44.x86_64.rpm`; each returned exit 0 and `digests OK`. |
| `./target/desktop/release/janus --version` | `janus 0.1.3`, exit 0, without opening a window. |
| `rpm -qp --qf '%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}\n' target/rpm/RPMS/x86_64/janus-0.1.3-1.fc44.x86_64.rpm` | `janus-0.1.3-1.fc44.x86_64`, exit 0. |
| Package inventory (`rpm -qpl`, first notice-bearing 0.1.0-5 and 0.1.1-1 builds) | Native `/usr/bin/janus`, desktop launcher, PNG, README and all four AirPlay license/provenance files present. |
| `bash -n scripts/build.sh`; `desktop-file-validate packaging/rpm/janus.desktop` | Passed. |
| Core/app fmt; core Clippy with `--locked` and warnings denied | Passed at current source state. |
| Desktop Clippy with locked deps / warnings denied after scrolling change | Passed at 0.1.2 source state immediately before the version-only bump. |

Native app Help now shows the compiled package version; desktop `--version` / `-V`
exits before compositor/runtime startup. RPM metadata includes the unresolved
project license reference and the adapter's LGPL terms; shipped notices do not
complete external corresponding-source/relink distribution obligations. No RPM
installation or system upgrade was performed by the agent.

## Board profile diagnosis — latest host evidence

Adding the five timestamp descriptors did **not** resolve the board's RTSP
video-SETUP timeout. A further user host attempt reported:
`AirPlay request timed out during video SETUP (timing: 0 requests received,
0 replies sent)`. The counters count valid timing requests from the selected
peer and successful replies; zero does not distinguish absence from filtering.
Added public-source timing packet vectors: >=32-byte valid requests preserve
reserved header bytes, echo the sender timestamp and use the monotonic NTP
clock; malformed packets and responses never provoke replies.
`cargo test --locked -p janus-airplay` passed **25 tests**; the most recent full
pre-version-bump run reported **125 passed, 8 failed, 7 ignored**, all failures
the known socket-creation `PermissionDenied` cases.

User firewalld output identifies the active Wi-Fi FedoraWorkstation zone with
TCP and UDP 1025–65535 permitted, no rich rules shown. No firewall change was
made. Sandbox queries failed at D-Bus connection / `/etc/firewalld` permissions.

The user's read-only `curl --max-time 4 --silent --show-error
http://192.168.200.163:7100/stream.xml` returned XML properties: 1920×1080,
overscanned false, refreshRate 1/60, version **200.54**. This confirms the
separate older HTTP mirroring service. At the time of this probe HTTP sending
was not implemented; the later native implementation and hardware failure are
recorded in the latest HTTP acceptance entry below. Existing RTSP synthetic
tests and RPM builds do not qualify it. See [the handoff](airplay-handoff.md).

## Current continuation — timeout after control-SETUP parser fix

After the empty-control fix, the user reran
`diagnostics check-receivers --name "Azuba-Board" --seconds 12 --hold 0 --mirror 10`:
discovery and live capability passed; session failed with
`AirPlay request timed out`. That host build had no timeout-stage label, so
the stalled operation was initially unknown. No successful rendering is reported.

Added fixed timeout-stage labels at the owning RTSP/transport boundaries.
A paused-clock scripted video-SETUP stall verifies the label survives cleanup
and the poisoned connection is closed without RECORD/TEARDOWN reuse.
`cargo test --locked -p janus-airplay` now reports **23 passed, 0 failed**.
The next user-supplied labelled run reported **`AirPlay request timed out during
video SETUP`**. Compared the request with pinned Doubletake
`mirror.go::videoStreamDesc` and the legacy mirroring trace: Janus omitted the
five `timestampInfo` dictionaries. Added those exact names/order. Running
`cargo test --locked -p janus-airplay --test test_session test_legacy_negotiation_authenticates_and_stops`
with the new independent descriptor check before the implementation change
reported 0 passed, 1 failed (missing key); afterward all 23 AirPlay tests pass.
The descriptor change awaits a host retest; do not describe the timeout itself
as fixed. [Complete handoff](airplay-handoff.md).

Reread `docs/65WR6CE_Datasheet_v2.pdf` using `pdftotext -layout`; both pages
extracted, with the same invalid-font-weight warning. SHA-256 remains
`6442a8b2b25c32aa8eb093b58a897b3c863e75c02bad9cc757751e5853b7a538`.
The datasheet specifies Android 13, 3840×2160/60 Hz, Wi-Fi and wireless screen
transmission; it supplies no sender wire contract. Handoff/status/protocol doc
relative links were checked and all target files exist.

## Board startup regression — user host output and restricted-workspace fix

User-supplied Fedora output (not a socket run in this sandbox) establishes:

- Read-only `diagnostics check-receivers --name "Azuba-Board" --seconds 3 --hold 0`
  passed discovery and live screen/ALAC capability, with a canvas up to 1920×1080.
- The first no-audio test-pattern cast failed with `invalid AirPlay response`.
  Added fixed stage/shape labels, without logging response payloads or credentials.
- `diagnostics check-receivers --name "Azuba-Board" --seconds 12 --hold 0 --mirror 10`
  narrowed the failure to `control SETUP: empty plist response body`. This means
  authentication reached control SETUP, but does not prove media negotiation.
- One intervening four-second discovery scan found no matching screen. A longer
  scan then found it; no discovery-code change or persistent-reachability claim.

The regression was reproduced with a scripted peer: before the fix,
`cargo test --locked -p janus-airplay --test test_session test_empty` reported
1 passed and 1 failed, with the same control-SETUP error. The fix accepts an
empty body only after successful control SETUP; nonempty malformed replies and
empty media replies still fail, with TEARDOWN of accepted state.
After the fix, `cargo test --locked -p janus-airplay` reports **22 passed**.
The adjacent pipeline/selection suites report **3 passed**. Core fmt and Clippy
pass; `./scripts/build.sh desktop` rebuilt the native GUI successfully.
Desktop Clippy also passes. Final `cargo test --workspace --locked --no-fail-fast`
reports **122 passed, 8 failed, 7 ignored** (exit 101); all eight failures remain
the sandbox socket-creation errors listed below.
Board rendering, sound, repeated starts and Stop remain pending a host retest.

## Native AirPlay implementation and supplied logo — restricted workspace

This section supersedes the research-only implementation status below. Native
legacy AirPlay is implemented, with boundaries in [airplay-control.md](airplay-control.md).
These are synthetic/build results; no new physical receiver result is claimed.

| Check / reproducible command | Observed result |
| --- | --- |
| `cargo test --locked -p janus-airplay` | 20 passed, 0 failed: public crypto vectors, independently executed pinned Go wrapping vector, scripted duplex negotiation, hostile RTSP/plist input, media framing/encryption, retransmission limits and cancellation. |
| `cargo test --locked -p janus-mirror --test test_airplay -p janus-cli --test test_cast_selection` | 3 passed, 0 failed: actual OpenH264 test pattern independently decrypted/unframed and decoded; receiver selection rejects ambiguous names. |
| `cargo fmt --all -- --check` | Passed, exit 0. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed, exit 0. |
| `cargo test --workspace --locked --no-fail-fast` | 120 passed, 8 failed, 7 ignored; exit 101. All eight failures hit socket creation with `PermissionDenied: Operation not permitted`: five in `test_cast_session`, two in `test_streaming`, one in `test_mirror`. Log: `target/airplay-workspace-tests.log`. |
| `cargo fmt --manifest-path crates/janus-app/Cargo.toml -- --check` | Passed, exit 0. |
| `cargo clippy --manifest-path crates/janus-app/Cargo.toml --target-dir target/desktop --locked -- -D warnings` | Passed, exit 0, including final logo change. |
| `./scripts/build.sh deps --check` | All required native dependencies installed. No new OS package required. |
| `./scripts/build.sh desktop` | Passed; final native executable at `target/desktop/debug/janus`. |
| `./target/desktop/debug/janus --smoke-test` | Exit 101: GPUI Wayland initialization failed with `NoCompositor`; no frame or logo rendering observed. |
| `cargo run --locked --bin janus -- diagnostics check-receivers --seconds 1 --hold 0` | Exit 1: `Operation not permitted (os error 1)`; no receivers observed. |
| `bash -n scripts/build.sh`; `desktop-file-validate packaging/rpm/janus.desktop` | Both passed. |
| `rpmspec -q --qf '%{NAME}-%{VERSION}-%{RELEASE}\n' --define 'janus_version 0.1.0' --define 'janus_glibc_version 2.43' packaging/rpm/janus.spec` | Passed; `janus-0.1.0-5.fc44`. Spec parsing only; no RPM built or installed. |

The supplied `docs/janus-logo.png` is the single source for the embedded 36 px
header image and packaged launcher icon. The original PNG is preserved; the RPM
installs it in `/usr/share/pixmaps/janus.png`, and `janus.desktop` explicitly uses
that installed path, replacing the old SVG. The window's
`app_id` is `janus`, matching `janus.desktop`. Visual appearance and desktop-shell
icon lookup still require a host desktop check. The installed release-4 app is
unchanged by this debug build.

Physical exit: run the [host test](airplay-control.md#acceptance-and-runnable-host-test)
with the board in normal LAN/AirPlay mode; observe picture, sound, Stop and
repeated screen/window starts. AirPlay transmission and TEARDOWN acknowledgment
do not prove rendered frames. PIN/password/HAP, AAC-only audio, macOS/Windows
capture and performance qualification remain outside this implemented slice.
LGPL source/relink obligations are recorded in the crate's
[provenance](../crates/janus-airplay/PROVENANCE.md) and remain a binary-release gate.

## Hisense OSS source research — 9 November 2026 (restricted workspace)

Documentation-only investigation: [research report](hisense-airplay-research.md).
Read the supplied datasheet, traced discovery → CLI/GPUI Cast-only session →
capture/encoder → Cast Streaming, and inspected primary upstream source and
license notices. Corrected the historical blanket “no EShare Linux client” claim:
the current official download page lists Ubuntu/Debian. The download link returned
binary content to the web tool; no package was installed or executed.

| Check / reproducible command | Observed result in this session |
| --- | --- |
| `pdftotext -layout 65WR6CE_Datasheet_v2.pdf -` | Read both pages; wireless presentation described, no wire protocol/SDK contract. Poppler emitted an invalid-font-weight warning but extracted text. |
| `sha256sum 65WR6CE_Datasheet_v2.pdf` | `6442a8b2b25c32aa8eb093b58a897b3c863e75c02bad9cc757751e5853b7a538` |
| `cargo fmt --all -- --check` | Passed, exit 0. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed, exit 0. |
| `cargo test --workspace --locked` | Exit 101; stopped at `janus-cast/tests/test_cast_session.rs`: 0 passed, 5 failed. Each failure was `PermissionDenied: Operation not permitted` at socket creation on line 134. Earlier reported targets included 5 passing tests and 2 ignored tests. This is not a complete workspace pass/count. |
| Local Markdown relative-link target check | Passed for research report, compatibility, control, todo and plan documents. |

Shell HTTPS fetch of GitHub failed DNS resolution; the connected web-search tool
supplied upstream documents and source instead. No upstream sender or receiver
was built/run; no new LAN scan, pairing, media transmission, board rendering,
latency or efficiency test occurred. Historical physical evidence below remains
separate. The research does not add an implemented acceptance stage or enable
AirPlay Cast in Janus. Future ports must pin exact upstream revisions and retain
the applicable license/provenance notices.

## Verified in this workspace

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --offline --locked -- -D warnings` | Passed |
| `cargo test --workspace --locked` | 104 passed, 0 failed, 7 opt-in native tests ignored (run separately below). All tests are in `crates/<crate>/tests/test_*.rs`. |
| Desktop fmt / Clippy with warnings denied | Passed |
| Native desktop build with locked published libc 0.2.189 | Passed; target/desktop/debug/janus. Target-local xkbcommon-X11 runtime link used; see foundation prerequisites. |
| Fixture generation and independent Y4M/PCM parsing | Passed: 180 frames, 144,000 stereo samples, two intended flash/impulse events |
| Instrument correlation synthetic sample | Passed: one video miss retained, signed skew −2,000 µs; physical provenance unverified |
| `cargo run --offline --locked --bin janus -- diagnostics simulate --frames 3600` | Ran successfully; 32,768-byte peak synthetic queue and approximately +200 ppm fitted drift. No physical latency measurement. |

The discovery suite has 15 passing tests, including generic Google Cast/Janus advertisements, AzubaTV-shaped synthetic metadata, audio-only output filtering, duplicate/renamed devices, merged addresses, split response resolution, cache-flush grace, bounded/hostile TXT input and browser pause/drop.

The passing tests include actual Quinn QUIC/TLS 1.3 over bounded in-memory datagrams, certificate-key possession, control request framing/ownership, busy rejection, Stop, lease expiry, and one pinned MoQ IETF draft-21 object through the raw-QUIC wrapper. Other tests cover malformed wire lengths, DNS-SD expiry/cache limits, clock drift, quality hysteresis and predictive-frame recovery. New P0 tests pass real QUIC control through bounded 8 Mb/s packet serialization, 2–5 ms delay/jitter and every-eleventh-datagram loss; another stalls the consumer to verify overflow drops and buffer cleanup. Two fixture tests verify intended flash/PCM timing and correlation with missed events, signed skew and invalid instrument clocks. See [fixture procedures and synthetic baseline](fixtures/README.md).

The in-memory datagram link is test-only. It does not open network sockets or prove firewall, Wi-Fi, discovery reachability, native capture, codec quality, physical latency, GPU residency or cross-platform support.

The direct native Cast protocol suite has 18 unit tests for framing, read-only status/heartbeat exchange and bounded nonce/TLS transcript inputs, plus 10 integration tests in `crates/janus-cast/tests/test_trust.rs` for offline device trust (path, revocation, signature) against real public Cast vectors and Open Screen's 32-case CRL conformance suite. See [cast-control.md](cast-control.md).

## Native host checks — 9 November 2026 (this Fedora laptop, same LAN as AzubaTV)

Sockets, multicast and the compositor were available in this session, so the
previously blocked native checks were run directly:

| Check | Result |
| --- | --- |
| `janus discover --seconds 4` | AzubaTV / POLYTRON4K at 192.168.200.233:8009, video+audio outputs advertised |
| Native LAN discovery test (then named for AzubaTV; now generic) | Passed |
| `cargo test -p janus-session -- --ignored` (NET03 native UDP/QUIC) | 3 passed |
| `janus diagnostics self-test` | Passed: QUIC/TLS 1.3 + Protobuf synthetic start/heartbeat/stop, no lease remains |
| `./target/desktop/debug/janus --smoke-test` | `JANUS_SMOKE_OK: native window rendered` (Fedora; other OS rows still pending) |
| `janus diagnostics cast-check 192.168.200.233:8009` (3 runs) | `device_verified: true`, `policy: unrestricted`, `revocation: device-crl`, no control/media sent |
| `janus diagnostics check-receivers --hold 12` (generic LAN sweep) | AzubaTV: discovery, device-trust, capability (screen mirroring app `0F5096E8` available), session (12 s, 2 heartbeat replies) pass; mirroring pending. Exit 0 |
| `janus diagnostics check-receivers --hold 60` | session held 60 s, 11 heartbeat replies, no reconnect |
| `cargo test -p janus-cli --test test_native_receivers -- --ignored` | Passed (every discovered receiver passes implemented stages) |
| `cargo test -p janus-discovery --test test_discovery -- --ignored` (generic: any receiver) | Passed |

The real receiver's chain is device → *Hisense TV lushan Mediatek MT9676 Cast
ICA* → *Widevine Cast Subroot* → embedded *Cast Root CA*. It supplied its own
CRL (validity ≈7.5 days), which verified against the embedded Cast CRL Root CA
and permitted the path. The first real run exposed a porting bug (fallback CRL
expiry was capped by its 2023 signer expiry, so every device looked revoked);
it was fixed to match Chromium and a regression test was added. Device trust is
not mirroring: receiver control, Cast Streaming negotiation, capture and
encoding remain unimplemented.

## Casting pipeline — 9 November 2026

| Check | Result |
| --- | --- |
| `cargo test -p janus-cast --test test_streaming` | 8 passed (NIST CTR vector, receiver-rule RTP parse, RTCP feedback, loopback NACK/ACK/PLI) |
| `cargo test -p janus-cast --test test_cast_session` | 5 passed (scripted LAUNCH → OFFER → ANSWER → UDP RTP → STOP) |
| `cargo test -p janus-encode` | 6 passed (IDR with SPS/PPS, forced IDR, OpenH264 decode round trip, scaling) |
| `cargo test -p janus-mirror` | 1 passed: test pattern → H.264 → encrypted RTP → scripted receiver decrypts, ACKs and decodes ≥20 frames; Stop reaches it |
| `cargo test -p janus-capture --test test_native_capture test_native_desktop_audio -- --ignored` | Passed on this Fedora/GNOME host (≥0.5 s of 48 kHz stereo in 1.25 s) |
| Read-only `janus diagnostics check-receivers` after the cast changes | AzubaTV: all implemented stages pass; mirroring opt-in |
| Desktop app build, Clippy, `--smoke-test` | Passed; Cast button wired to the pipeline |
| Real-TV cast, desktop app (RPM 0.1.0-4), screen + sound → AzubaTV | **Running, user-started.** Host trace at ~4 min: TLS control to `192.168.200.233:8009` up (RTT 5–7 ms, heartbeats ~5 s); UDP media socket connected to the TV's answered port `:52724` (OFFER accepted); pipeline threads `janus-pw-video`/`janus-video-enc`/`janus-pw-audio`/`janus-audio-enc` alive. Changing screen: ~396 UDP pkt/s out, ~3.6 Mbit/s, encoder ~97 % of one core. Static screen: ~106 pkt/s (≈ Opus 100/s), ~100 kbit/s, encoder ~1 %. TV feedback ~150 RTCP pkt/s back. Media socket 0 drops; RSS 251 MB steady over 30 s. A concurrent read-only `check-receivers` passed without disturbing the cast. Picture/sound quality and latency: user observation pending. |

## Previously blocked checks (sandbox history)

- `janus discover --seconds 3` was attempted after adding existing-receiver discovery and failed at socket creation; the real AzubaTV was not observed inside this sandbox. The user subsequently supplied a native Janus screenshot showing AzubaTV/POLYTRON4K; this is separate host evidence, not a sandbox socket result. Native UDP tests also previously attempted: `Operation not permitted (os error 1)` when opening sockets. They remain explicit `#[ignore]` tests, run by CI with `cargo test -p janus-session -- --ignored`.
- Native build succeeds. Actual `./target/desktop/debug/janus --smoke-test` was attempted and fails inside GPUI with `NoCompositor`; sandbox environment variables do not provide compositor/GPU access. The user screenshot verifies prior Fedora native rendering/discovery; compact rendering, backend details and accessibility remain unverified. Debian/Fedora/macOS/Windows native workflows are configured but not executed here.
- PipeWire, VA-API and ALSA development packages were absent from pkg-config. Native media adapters have not been implemented or tested. Opus metadata being installed does not imply an implemented audio pipeline.
- Second physical endpoint, Apple Silicon Mac, Windows PC, signing credentials and calibrated latency/audio fixtures are unavailable in this workspace.

## First hardware acceptance and existing receivers

| Sender | Receiver | Protocol | Status |
| --- | --- | --- | --- |
| This Linux laptop | Any discovered Google Cast receiver (first unit: AzubaTV, POLYTRON4K, Hisense/MediaTek MT9676 Cast ICA) | Google Cast wire protocol, native | Discovery, device trust, capability and live session verified on host; mirroring pending |
| Debian/Fedora/macOS/Windows | Additional devices/brands/firmwares | Qualified Google Cast, then AirPlay/Miracast paths | Pending per source and protocol |

Install Janus only on the sender for the existing-TV path. All discovery, screen/app-window selection, connection controls and Stop must be exercised in Janus's native UI. Required OS privacy prompts and receiver authentication are allowed; launching Chrome or the OS casting panel is not acceptance.

`janus diagnostics check-receivers` and `cargo test -p janus-cli --test test_native_receivers -- --ignored` validate every receiver found on the LAN, not a named unit. Synthetic AzubaTV DNS fixtures exercise parsing only and never prove hardware discovery, authentication, codec support or screen mirroring. An advertised video-output flag does not prove mirroring readiness.

## Additional optional Janus-native desktop matrix

Every cell remains **pending**. This matrix validates optional Janus-native receiving; it is not a prerequisite for the first laptop-to-AzubaTV test. Compilation is not end-to-end validation.

| Sender ↓ / Receiver → | Debian | Fedora | macOS | Windows |
| --- | --- | --- | --- | --- |
| Debian | Pending | Pending | Pending | Pending |
| Fedora | Pending | Pending | Pending | Pending |
| macOS | Pending | Pending | Pending | Pending |
| Windows | Pending | Pending | Pending | Pending |

For each direction, install actual signed/trusted artifacts on clean user profiles, record OS/GPU/driver/network/display/audio assets and run:

1. Offline discovery, receive-off/on, duplicate names, IPv4/IPv6, blocked multicast/direct reachability and installed firewall/local-network permission behavior.
2. One-click receiver admission with native screen/window/audio-only source consent, audio-off default and busy rejection. No synthetic source qualifies this check.
3. Source/window closure, permission revocation, lock/suspend, device removal, receive-off and local/remote Stop. Physically confirm capture ends and receiver blanks/mutes within the PRD limits.
4. One-second interruption, network-family/interface change, fresh generation/IDR and no buffered private-frame replay. Validate both successful recovery and timeout cleanup.
5. Ethernet/Wi-Fi latency, physical lip sync, continuity and 720p fallback against PRD §3; record percentiles and unsuccessful samples. Report CPU/private/GPU memory separately.
6. Loss/jitter/bandwidth steps, competing traffic, malformed peers, 100 start/stop cycles and 24-hour soak. Retain bounded diagnostics and failure denominators.
7. Keyboard/200% scale/screen reader, preview/fullscreen/HiDPI, update/failed-update/rollback/uninstall and privacy-redacted support export.

Platform-specific gates: macOS notarized bundle/TCC/Local Network/ScreenCaptureKit/Core Audio; Windows signed installer/private firewall/WGC/WASAPI/multi-GPU; Debian and Fedora independently qualify portal/PipeWire/VA-API on Intel and AMD. Other desktop/GPU combinations require separate evidence.


## Build compatibility evidence

`gpui 0.2.2 → gpui_http_client 0.2.2 → zed-async-tar 0.5.0-zed → xattr 0.2.3`
requires libc's historical Linux ENOATTR alias. Inspection of published cached
releases found it in libc 0.2.189 and absent from 0.2.190/0.2.191. The desktop
manifest pins 0.2.189 and its committed lockfile retains registry checksums.
`cargo build --offline --locked` compiled the unmodified published xattr and
linked Janus successfully. No vendoring, registry modifications or ENOSTR
substitution. Core retains its own dependency resolution.

The Fedora host lacks the unversioned xkbcommon-X11 development link. This
validation linked `/usr/lib64/libxkbcommon-x11.so.0` via a symlink beneath
`target/desktop/native` and `LIBRARY_PATH`; install `libxkbcommon-x11-devel` for
normal builds. See [foundation.md](foundation.md) for native prerequisites,
assets and the remaining A01/A02/A05 gates.

## Native RPM — local Fedora 44 build

- `cargo build --release --locked --offline --manifest-path crates/janus-app/Cargo.toml --target-dir target/desktop` passed, using the previously documented target-local X11 linker search path.
- Built `target/rpm/RPMS/x86_64/janus-0.1.0-1.fc44.x86_64.rpm` (approximately 5.2 MiB). `rpm -K` reports `digests OK`; signature is absent.
- Extracted the payload under target/rpm/verify. The installed `/usr/bin/janus` matches the stripped release GPUI executable; launcher, icon and README match staged inputs. Executable permission is 0755; desktop-file-validate and SVG parsing passed. Declared host package requirements and ELF shared-library resolution passed, including glibc 2.43.
- Build spec and repeatable RPM build script added (now `scripts/build.sh rpm`); script syntax passed. This environment lacks rpmbuild and cannot download it because Fedora mirror DNS fails. The local package used the installed RPM 6.0.2 native build API with the same spec; missing buildroot/unpackaged-file helper checks were replaced by independent extracted-payload verification. The normal helper requires the rpm-build package.
- Package contents, dependency list, byte count and SHA-256 are recorded under target/rpm; no host installation or configuration changes performed. Native desktop rendering remains blocked by compositor access. Install/upgrade/uninstall, signing, license/notices and release qualification are not claimed.


## Compact UI and receiver connection probe

The user-supplied native Fedora screenshot shows the earlier wide layout with
AzubaTV/POLYTRON4K selected. This verifies prior native rendering and host discovery;
it does not verify mirroring or the new layout. Compact geometry is now 840×520
(default), 640×420 minimum, content capped at 960 px, 16 px card padding and 12 px
gaps. Navigation is inline; cards stack below 700 px and retain 44 px actions.

The native Help connection check uses bounded TCP/TLS and sends only a fresh auth
challenge. Four new tests cover actual TLS 1.2/1.3 on in-memory duplex streams,
certificate dates/DER limits, transcript binding, unrelated-message flood bounds,
stalled handshake and cancellation/closure. All results stay unverified; no
receiver control or media is sent. This is not a real-TV authentication result.
See [cast-control.md](cast-control.md) for the remaining device trust gate.

Both debug and release desktop builds and warning-free desktop Clippy passed.
The compact release smoke was attempted and still fails at `NoCompositor` in
this sandbox. Updated unsigned RPM:
`target/rpm/RPMS/x86_64/janus-0.1.0-2.fc44.x86_64.rpm` (6,130,497 bytes).
Digest and extracted-payload checks pass; the stripped release executable,
launcher/icon/README, executable permissions, host runtime requirements and
shared-library resolution match. No install script was added. Release 2 upgrades
release 1; the RPM build (now `scripts/build.sh rpm`) queries its filename from the spec to avoid a stale
hard-coded release. Its syntax passed; the same installed RPM 6 build API was
used here because rpmbuild remains unavailable. Detailed local proof is retained
in target/rpm/verification-2.json. No system installation or compact visual QA
is claimed.


## Latest user-run Hisense HTTP acceptance — 9 November 2026

After the HTTP response half-close correction, the user ran:

```sh
cargo run --locked --bin janus -- diagnostics check-receivers \
  --name "Azuba-Board" \
  --seconds 12 --hold 0 \
  --mirror 10
```

Discovery, live legacy HTTP capability/profile, key-possession proof/FairPlay and
local HTTP stream setup pass. Mirroring fails with `Broken pipe (os error 32)`:
4 frames queued, 3 transmitted, 0 timing requests and 0 replies. Timing and
teardown remain Pending; command exits with acceptance failure. No rendered
pattern or physical Stop is confirmed. This is user-supplied hardware evidence,
not a sandbox-run test. TCP writes are not decode/render acknowledgments.

The native HTTP sender is implemented in development source; older entries
stating it was not implemented describe an earlier state. RPM 0.1.3-1 predates
that sender. See [current implementation](airplay-http.md) and
[handoff](airplay-handoff.md).

## Agent-run Hisense board diagnosis with packet capture — 9 November 2026 (evening)

This session ran on the user's Fedora host itself (wlp3s0, same LAN as the
board), so it could reach the board and capture with `dumpcap` (user is in the
`wireshark` group). Earlier sandbox limits (no sockets, multicast or capture)
did not apply. The user confirmed the board was free before any cast.

**Gates after the changes below:** `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets --locked -- -D warnings` clean;
`cargo test --workspace --locked` **145 passed, 0 failed, 7 ignored** (no
sandbox socket failures on this host). Desktop Clippy
(`--manifest-path crates/janus-app/Cargo.toml --target-dir target/desktop`) clean.

**Read-only facts (no pairing/media):** `/stream.xml` on 7100 → 1920×1080,
version 200.54. The AirPlay service is on **port 51010** (mDNS), not 7000.
TXT `srcvers=220.68`, `features=0x527FFFF7,0x1E`, `am=AndroidTV3,1`. Live
`GET /info` → `sourceVersion 220.68`, `features 0x1E5A7FFFF7` (bit 27 legacy
pairing **set**), `statusFlags 68`, `audioFormats` types 100/101, `displays[]`
with `uuid`/`maxFPS 30`, `keepAliveLowPower`: the UxPlay/RPiPlay `/info`
template. RTSP replies carry `Session: DEADBEEF`, `Audio-Jack-Status` and
`Server: AirTunes/211.3` (shairplay/RPiPlay lineage).

**HTTP profile, Janus, after codec-first + same-connection `/stream.xml`:**
`check-receivers --name "Azuba-Board" --seconds 8 --hold 0 --mirror 5` →
authentication/session pass; mirroring fails: `sharing ended early 410 ms after
stream start: Broken pipe; 3 frames queued / 2 transmitted; timing 0 requests /
0 replies / 0 other datagrams ignored; receiver: receiver closed the stream
connection (FIN) at 156 ms`. Capture: `/stream.xml`, both `/fp-setup` (200,
142/32 bytes, `Server: AirTunes/200.54`) and `POST /stream` are accepted; the
board stays silent for 145 ms, then sends **FIN 2 ms after receiving the first
codec (157 B) + encrypted IDR (3,667 B) packets**; later writes get RST. No
HTTP reply, and **no UDP of any kind** from the board (capture is pre-firewall).

**HTTP profile, scratch diagnostic without `param1`/`param2`** (unencrypted test
pattern from a throwaway script, not Janus code): board **RSTs 11 ms after
`/stream`**, before media. So the board requires the encrypted form; with
Janus's `param1` it accepts `/stream` and only drops when it must decrypt media.

**RTSP profile, forced for diagnosis (temporary knob, since removed):** capture
shows pair-setup/verify and both fp-setup succeed; first `SETUP` (ekey/eiv,
timingPort, et 32) → `200 OK`, `Content-Type: application/x-apple-binary-plist`,
**no Content-Length and no body**; video `SETUP` → **no reply** (5 s timeout);
zero UDP from the board. In the receiver lineage's handler (UxPlay
`raop_handlers.h` @ 4e95a9ec, `raop_handler_setup`), every admitted first
SETUP returns a non-empty `{timingPort, eventPort}` plist and starts NTP; an
empty body means the handler returned early. Variants tried, each unchanged:
User-Agent `AirPlay/935.7.1` (Doubletake); forced ECDH key mixing; `model`
`Linux` (Doubletake).

**Key-unwrap hypothesis disproven (user-approved offline check).** UxPlay's
`playfair_decrypt` (lib/playfair @ 4e95a9ec, compiled only in the scratchpad as
a test oracle, never bundled) was applied to the *real* `m3` and `param1` that
Janus sent the board in the capture above. With `param2` as IV, the recovered
key decrypts the captured IDR packet to `00 00 0d cf 65 …`: a correct 3,535-byte
NAL length and an IDR header. So Janus's FairPlay exchange, key wrap and
continuous AES-CTR are correct for this receiver family. (The golden vector,
built from a synthetic all-zero `m2`, does not decrypt to its raw key; that
vector cannot test receiver interoperability.)

**Further RTSP variant, unchanged:** macOS-like `model`/`osName`/`osVersion`/
`osBuildVersion`/`sourceVersion`, `X-Apple-ProtocolVersion`, `DACP-ID`,
`Active-Remote`, `X-Apple-Device-ID` and receiver-rule key mixing: first SETUP
still `200` with no body. UxPlay's own `report_client_request` admits everyone
unless an allow/block list is set, so the board's app adds its own admission
rule (on-screen approval, allow-list, busy state or a field not yet tried).
All experiment knobs were removed; gates rerun: 145 passed, 0 failed.

No picture was displayed; no physical Stop is claimed.

**Retry, 10 November 2026 (agent-run, capture on):** normal flow (HTTP) →
unchanged, FIN 155 ms after stream start, no UDP. RTSP (temporary force knob)
→ unchanged empty first-SETUP reply. Combined keys+video SETUP (media-first
form, no control SETUP) → **no reply at all**. Keys-only SETUP then video SETUP
with a 45 s wait → video SETUP still **unanswered after 45 s**; zero UDP from
the board throughout. Any SETUP requesting the type-110 stream hangs on the
board. All knobs removed afterwards; gates: 145 passed, 0 failed.

**After board power cycle, 10 November 2026 (agent-run, capture on):** the user
saw no allow/deny prompt on the board in earlier attempts. Board-side QR link
for phone sharing points at `<board>:8000` with `LANCast=true` and no code/PIN
field. A read-only `GET /` on port 8000 answers `Server: EShare Http Server/1.0`
(EShare sender downloads for Android, iOS, Windows, macOS), so the board's
sharing stack, and likely its AirPlay receiver, is EShare. Open TCP ports seen:
8000, 7100, 51010. Fresh-boot retry of
`diagnostics check-receivers --name "Azuba-Board" --mirror 10`: profile,
authentication, session pass; mirroring **fail**: board FIN 549 ms after stream
start, broken pipe at 795 ms, 6 frames queued / 5 sent, 0 NTP requests.
Unchanged by the reboot, so no stuck receiver state was involved.
Second retry the same day: FIN 417 ms after stream start; the capture shows
the board accepted codec data, the first IDR and two P-frames over ~270 ms
before FIN, then RST. Board `/stream.xml` advertises 1920x1080 while the test
pattern is 1280x720, so a real screen cast was run to rule out resolution:
`janus cast --name "Azuba-Board" --source screen --no-audio --seconds 30`
(portal picker, cast at 1920x1080) → board FIN ~190 ms after the first ~100 KB
IDR, then RST; `AirPlay connection failed: Broken pipe`; zero NTP/UDP from the
board. Resolution mismatch is ruled out. The user watched the board during
this cast: nothing appeared on screen (no picture, prompt or sender name).

## AirPlay PIN pairing and paired mirroring on AzubaTV — 10 November 2026 (agent-run, user-observed)

AzubaTV's AirPlay side (licensed AirPlay SDK, sourceVersion 377.40.00,
`statusFlags` bit 9 = PIN pairing, features without 27 and with 43/46/48/51,
no FairPlay bit 14) is a HomeKit-paired receiver. Implemented natively in
`janus-airplay` (`hap`, `secure`, `pairings`, `session::negotiate_hap`,
`Video::paired`); provenance in `crates/janus-airplay/PROVENANCE.md`.

| Step | Result |
| --- | --- |
| `janus pair --name AzubaTV` | First try: `pair-pin-start` timed out at the 5 s request deadline; capture shows the TV answered `200 OK` after 6.5 s (code on screen). Pairing deadlines raised (30 s pin-start, 15 s per step). Second run: user typed the TV's code; SRP M1–M6 with the receiver's M6 signature verified, then pair-verify; pairing stored owner-only (`~/.config/janus/airplay-pairings.plist`, 0600). |
| Discovery | AzubaTV's `_airplay._tcp` record was intermittently missing from IPv4 browsing (0/3 runs once); avahi showed it over IPv6 only at that time (SRV target `localhost.local`). Janus now also browses IPv6 mDNS (best effort) and rescans a named target up to three times: 4/4 scans found it afterwards. |
| Test pattern, 20 s (`janus cast --protocol airplay --source test-pattern`) | Ran to completion; 153 frames; encrypted control, event and video connections up for 20 s; TV exchanged NTP (23 packets). **User: picture shown.** |
| Desktop, 1920×1080, 60 s | Ran to completion; 221 frames, ~1 MB; exit 0. **User: shown; laggy and soft. Auto-stop after 60 s returned the TV to its normal screen.** |
| Quality/lag changes | OpenH264 frame skipping disabled (synthetic benchmark: after a large frame the encoder skipped the next frames, including changed ones); paired AirPlay playout target 400 → 100 ms (Doubletake Wi-Fi value); 60 fps default; resolution presets 720p/1080p/2K/4K for both protocols; paired AirPlay capped by the receiver's advertised maximum (AzubaTV 3840×2160). |
| AirPlay 720p test pattern, 12 s, release build | 720 frames: steady 60 fps. |
| AirPlay 2K desktop (external 2560×1440 monitor), 40 s, release build | Ran to completion; ~15–19 fps under motion (software H.264 limit). |
| Google Cast 2K desktop, 30 s, release build | TV accepted 2560×1440 OFFER; 437 frames sent, 433 acknowledged, RTT 7–9 ms; ~15 fps under motion. |

Encoder measurements (synthetic desktop frame, release build, this Ryzen/Renoir
laptop): OpenH264 key frame 21/48/84/183 ms and changed P frames 5–18 /
12–49 / 22–95 / 62–234 ms at 720p/1080p/1440p/2160p; extra threads, lower
complexity and disabling scene-change detection did not change speed. After
the user installed RPM Fusion's `mesa-va-drivers-freeworld`, VA-API reports
H.264 Constrained Baseline/Main and HEVC encode on the Renoir GPU (stock Fedora
Mesa reported none). GStreamer `vah264enc` (CBR, `target-usage=7`) encoded
300 uploaded NV12 frames at ~79 fps (1080p), ~49 fps (1440p) and ~13–19 fps
(2160p) using 5–16 % CPU. A hardware encoder backend in Janus is not yet
implemented.

Gates after these changes: `cargo fmt --all -- --check` clean; Clippy clean
(core and desktop workspaces); `cargo test --workspace`: **159 passed, 0
failed, 7 ignored** (14 new in `tests/test_hap.rs`; the two receiver-signature
tests were confirmed to fail when signature checks are removed).

## Paired sound, app pairing, GPU encoding and adaptive output — 10 November 2026 (agent-run, user-observed where noted)

| Check | Result |
| --- | --- |
| Paired AirPlay with sound (test pattern, 15 s) | First attempts: `RECORD` took 4.7 s (> 5 s deadline once) and the receiver's `POST /command` event without `CSeq` was rejected, ending the session. Fixed (15 s session-start deadline; CSeq optional; failure reason no longer masked by the sound thread). Then: 840 video frames, 1,746 audio packets (125/s), ran to completion. |
| Volume | **User: the TV volume jumped to 100 % after casting with sound.** Cause: `SET_PARAMETER volume: 0.000000` (0 dB = AirPlay full scale, from the reference sender). Removed; Janus no longer changes the receiver volume. |
| Picture "cracked" at 100 ms playout | **User: text and video choppy/cracked** (software encoder, 1080p): 30–43 ms encode, ~16 fps achieved, 17–38 OpenH264 scene-change IDRs per 30 s. Scene-change detection disabled. |
| GPU encoder (after RPM Fusion `mesa-va-drivers-freeworld` and GStreamer dev packages) | `test_native_gpu_encoder_sustains_1080p60`: 300 frames, **70 fps**, Constrained Baseline, decodable by OpenH264 (pipeline test). Test pattern, 720p60: AirPlay 599 frames/10 s, Cast 599 sent / 592 acknowledged, RTT 7 ms. |
| Real desktop, GPU, 2560×1440 at 60 fps (Auto, first version) | ~27–35 fps achieved, 80–105 ms capture→encoded, 6 dropped, 8 late: not sustainable. Auto now caps at the largest size the encoder keeps smooth. |
| Real desktop, GPU, Auto → 1920×1080 at 60 fps, 12.4 Mbit/s, AirPlay | 35–38 ms capture→encoded, 0 dropped, 1 late, 1 key frame in 25 s. User picture judgement pending. |
| Desktop app | Pairing card (keyboard code entry), minimize/close (no maximize, per user), drag/resize; smoke test renders. User in-app pairing run pending. |

Gates: fmt clean; Clippy clean (core and desktop, `--locked`); `cargo test --workspace`: **165 passed, 0 failed, 8 ignored**.

## Full-quality Auto, area downscaling and size-based delay — 10 November 2026

User report: 2560×1440 looked cleaner than 1080p, whose text looked jagged
("chapped"). Cause: the CPU scaler was nearest-neighbour (2560→1920 skips every
fourth column/row). Replaced by separable, threaded area averaging: 2K→1080p
8.8 ms/frame, 2K copy 4.1 ms, 2K→720p 5.1 ms (release, this laptop); exact
coverage weights tested (4→3 stripes = 64/128/191). Auto now means the source's
own size up to the receiver maximum, at the highest rate the encoder sustains
(GPU: 1080p60, 2K30, 4K20); paired AirPlay delay 150/200/300 ms by size.

| Check | Result |
| --- | --- |
| Real desktop, Auto, AirPlay, GPU | 2560×1440 30 fps, 13.3 Mbit/s; 42–46 ms capture→encoded; 0 dropped, 0 late, 2 key frames in 25 s |

Gates: fmt clean; Clippy clean (core and desktop, `--locked`); `cargo test
--workspace --locked`: **167 passed, 0 failed, 8 ignored**.


## Hisense EShare endpoint correction — 10 November 2026

This run used the already-downloaded vendor artifacts in `research/`, not new
vendor downloads. Focused macOS native disassembly identifies an EShare RTSP
service separate from the mDNS AirPlay service. Windows PE headers, mapped
literals and candidate instruction references corroborate the EShare vocabulary;
no full Windows call graph or vendor-client runtime session is claimed. Sources,
artifact hashes, genuine REA Evidence IDs and unavailable-provider limits are in
[the research report](hisense_protocol_analysis.md).

**Reproduced baseline:**

```sh
JANUS_HISENSE=1 cargo run --locked --bin janus -- cast \
  --name Azuba-Board --source test-pattern --no-audio --seconds 3
```

Before the route correction: discovery and selection succeeded; startup failed
with `AirPlay request timed out during video SETUP`. The adapter reused the
advertised TCP 51010 connection and retried that same endpoint. The inspected
native macOS `startMrror` uses `getDynamicPort_51040` instead.

**Physical results after routing correction:** the same three-second command
exited 0 and reported 40 transmitted video frames. The user explicitly confirmed
that the moving test pattern rendered and that Stop returned the board to its
previous screen. These observations establish video feasibility on this board,
not every product in the vendor download-page series.

After the explicit EShare HTTP identity check was added, independent acceptance
runs transmitted 50/50 and 54/54 queued frames. They initially exited 1 only
because acceptance demanded an RTSP teardown acknowledgement. A targeted
capture (`/tmp/janus-hisense-stop2.pcapng`, 23 packets, 0 dropped) showed Janus
TEARDOWN CSeq 5 in frame 18, body in frame 19, receiver FIN in frame 21, and no
RTSP reply. Native `stopMirror` sends teardown then `disconnectAfterWriting`.
Consequently the EShare path now records local shutdown with acknowledgement
Pending; FIN is never counted as an RTSP ACK.

**Final generic sweep:**

```sh
JANUS_HISENSE=1 cargo run --locked --bin janus -- diagnostics check-receivers \
  --name Azuba-Board --seconds 3 --hold 0 --mirror 3
```

Exit 0; one receiver, seven stages: discovery, capability, profile and session
Pass; authentication, mirroring and teardown Pending. It transmitted **15/15**
queued frames at 1280×720. The lower frame count coincided with compilation and
regression work; no throughput or frame-rate qualification is inferred.
Authentication remains Pending because EShare does not authenticate receiver
identity or encrypt video. Mirroring has no per-frame ACK and needs physical
confirmation; the earlier user observation supplies that separately. Teardown
has no observed RTSP acknowledgement and needs physical confirmation, also
supplied separately. Passing this command is not an all-stages-Verified claim.
Temporary full output: `/tmp/janus-hisense-acceptance-final.json`.

**Real source capture:**

```sh
JANUS_HISENSE=1 cargo run --locked --bin janus -- cast \
  --name Azuba-Board --source screen --no-audio --seconds 5
```

The system portal picker completed; GPU H.264 screen casting started at
1920×1080, planned 30 fps / 7.5 Mbit/s, and exited 0 after Stop. CLI checkpoints
reported 11 transmitted frames, all 11 late, with encode figures around 702–777
ms while other builds/tests were running. Smooth 1080p30 is not qualified by
this run. The user explicitly confirmed the desktop appeared on the board and
sharing stopped. Window-source capture and sound were not tested.

**Final code/build gates:**

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo test --workspace --locked`: **176 passed, 0 failed, 8 ignored**.
- New Hisense tests cover preferred 51040 routing, advertised-connection reuse,
  primary-connect failure fallback and final RTSP rejection, plus explicit
  EShare identity; existing native-format framing/SETUP tests remain passing.
- Two CLI regressions cover truthful authentication and EShare Stop evidence.
- Desktop formatting and Clippy with the desktop manifest/target directory:
  passed; `./scripts/build.sh desktop`: passed.
- Rebuilt `./target/desktop/debug/janus --smoke-test`: exit 0,
  `JANUS_SMOKE_OK: native window rendered; media not tested`.

The initial unprivileged AirPlay suite failed one socket-creating HTTP test with
sandbox EPERM. It was rerun with permitted local sockets as part of the complete
passing workspace run. No ignored hardware test is counted as passed. The final
GPUI application was opened for a separate Settings-switch interaction check.
The user explicitly confirmed the complete rebuilt-app flow: Settings → Hisense
interactive display (Alpha) On → select the board → Sound off → Cast → Stop.
This is user-observed physical acceptance of the requested desktop integration,
in addition to the independent CLI screen cast. No RPM was rebuilt or installed.


## Hisense audio quality failure and codec diagnosis — 10 November 2026

The user requested right-aligned inline Cast and continued audio/testing. The
receiver card now keeps Screen/Window/Sound/resolution in a wrapping left group
and Cast/Cancel/Stop separately at the right of the same row. Native desktop
build, formatting, Clippy and rendered smoke passed.

Initial audio implementation used AAC-LC/1024, 44.1 kHz stereo at 128 kbit/s.
An eight-second pattern cast plus alternating desktop-monitor tones exited 0;
the user heard picture/sound and Stop worked. The later 20-second window test
revealed **distorted, crackling or choppy board audio**, explicitly distinguished
by the user from A/V synchronization. Therefore initial audible tones are not a
quality pass, and the LC implementation is incorrect for this native profile.

Pre-correction synthetic gates passed (181 tests, 0 failed, 8 ignored), including
independent LC decoding. They establish codec output, not conformance to the
receiver's expected format. Three normal five-second-discovery acceptance runs
exited 0 with 78/77/79 transmitted video frames and 161/161/160 actual audio
packets. An earlier two-second scan missed the receiver once; it remains a
recorded discovery limitation, not an audio playback result.

A targeted capture of the 20-second window test recorded 856 audio datagrams
over 19.86 s, no sequence discontinuity, no timestamp reversal, SSRC bytes
`01000000`, median clock step 23 ms and UDP payloads 268–887 bytes. Capture
reported 909 total packets, 0 dropped. Network arrival gaps were 15.80–31.79 ms,
with no >46 ms gap. This did not show sender loss or severe packet bursting;
it did not prove receiver playback quality. Raw capture stays in temporary
local storage (`/tmp/janus-hisense-audio-packets.pcapng`), not a support export.

Fresh native decoding found the root format mismatch. At VA `0x1dbf80`, the
macOS SDK contains `0200000044ac000000ee020027000000`, four LE32 values:
channels 2, sample rate 44100, bitrate 192000, Audio Object Type **39 (AAC-ELD)**.
`FdkAACEncoder initEncoder` also sets granule length **480** at `0x42ddb`.
Sources: `research/extracted/overseas_mac/audio_20261110.txt` and the SDK hash in
the research report. The installed GStreamer FDK `profile=ld` yields AOT 23,
not ELD 39, and exposes no 480-sample granule parameter. The correction must
match the native codec contract; changing timestamps cannot fix that mismatch.

## Hisense native (no switch) and video-quality pass — 10 November 2026

The user reported that the board's picture from this desktop was "not quite
clear and sometimes blurry", and asked for the Hisense switch to be removed.
The earlier "Sound isn't available … turn Sound off" UI error was the switch
being off: the read-only sweep then reported `live legacy HTTP mirroring:
screen=true supported audio=false`. With EShare enabled it reported `live Hisense
interactive display: screen=true supported audio=true`. The `JANUS_HISENSE=1`
commands above are historical; the variable and `--hisense` no longer exist.

**Board encoder wishes (physical capture):** `dumpcap` on TCP 51040 during a
6 s test-pattern cast (47 packets, 0 dropped). Every OPTIONS reply carried
`{"Framerate":"15","idr_req":"0","casting_win_height":"2160","feature":"1",
"Castnum":"1","casting_win_width":"3840","exclusive_screen":"0","bitrate":"0",
"i-interval":"0"}` and `Video-Audio: 3114-25621` → `3165-25805` →
`3225-25989` (2 s apart). The sender desktop is a 1920×1080 eDP panel at scale
1.0, so Auto already casts 1:1 at 1080p and resolution was not the cause.

**Static analysis (EShareSDK, overseas macOS):** `initDynamicBitrateControl`
bitrate constants 0x666666/0x333333/0x199999/0xccccc/0x999999/0x4ccccc (bit/s),
selected on screen height vs 1081; `checkNetworkQuality` reads
`socketWriteQueueCount` (≥6 degrades, ≤2 recovers) and
`updateBitrateBasedOnNetworkQuality` sets `AverageBitRate`; profile
`kVTProfileLevel_H264_Baseline_AutoLevel`; `MaxFrameDelayCount` 1.

**After the change (no flag, no environment variable):**

```sh
cargo run --locked --bin janus -- diagnostics check-receivers --seconds 5 --hold 0
cargo run --locked --bin janus -- cast --name Azuba-Board --source test-pattern --seconds 10
```

- Sweep: Azuba-Board → `live Hisense interactive display … audio=true`;
  AzubaTV stays `paired RTSP`, a MacBook stays `RTSP` (no misclassification).
- 10 s pattern cast with sound: 99 video frames (1 key), 712 audio packets,
  `GPU encoder` with the new VBR/max-qp pipeline (driver warns only about
  unsupported `VAConfigAttribEncQuantization`); 30 fps planned, ~25 fps observed
  in a debug build. No visual quality claim from the test pattern.
- Gates: fmt and Clippy passed; `cargo test --workspace --locked` **182 passed,
  0 failed, 8 ignored** (new board-body `idr_req` parser test); desktop fmt,
  Clippy and `./scripts/build.sh desktop` passed; smoke test
  `JANUS_SMOKE_OK`.
- Pending: user visual confirmation of desktop text sharpness on the board.

**Board canvas (same day):** a single raw `OPTIONS` to TCP 51040 with no
SETUP returned `CastingNumber: 0`, `CastingStatus: 1` and the same body with
`"casting_win_width":"3840","casting_win_height":"2160"`. With the probe reading
it, `diagnostics check-receivers --name Azuba-Board` reports `live Hisense
interactive display: screen=true supported audio=true; canvas ≤3840×2160`
(previously ≤1920×1080). Workspace tests: 182 passed, 0 failed, 8 ignored
(canvas parser cases added to the board-body test). RPM
`janus-0.1.5-2.fc44.x86_64.rpm` built, digests OK (0.1.5-1 was already
installed, so the canvas change took a new Release). Visual 4K quality is not
claimed: the sender panel is 1920×1080.


## Ubuntu .deb and sender name — 10 November 2026

**Package:** `./scripts/build.sh deb --container` (ubuntu:24.04, Docker 29.9.3)
built `target/deb/janus_0.1.5-1_amd64.deb`. The first attempt failed because
`libfreetype6-dev` is only a virtual package on Ubuntu 24.04; the Debian list now
uses `libfreetype-dev`. The release build took 6 min 26 s cold and 49 s
incremental. Depends were computed by `dpkg-shlibdeps`
(`libc6 (>= 2.39)`, `libpipewire-0.3-0t64`, `libgstreamer1.0-0`, …) plus
`libvulkan1`, `libwayland-client0`, `fontconfig`, `pipewire`,
`xdg-desktop-portal` and `gstreamer1.0-plugins-base`.

**Clean install (fresh ubuntu:24.04 container):** `apt-get install
./janus_0.1.5-1_amd64.deb` succeeded (946 packages set up, including the
recommended `gstreamer1.0-plugins-bad`, `mesa-va-drivers` and
`xdg-desktop-portal-gnome`). `janus --version` printed `janus 0.1.5`, and `ldd`
reported 0 missing libraries.

**Rendering:** under Xvfb with llvmpipe, the window mapped at 840×520 but
never presented a frame. CPU stayed idle, and the smoke test timed out with or
without a session D-Bus. Inside the same Ubuntu userspace with this laptop's
`/dev/dri` and desktop session, `janus --smoke-test` printed `JANUS_SMOKE_OK`
on **Wayland** and on **X11 via XWayland** (exit 0). In the same container,
Ubuntu's `mesa-va-drivers` 25.2.8 reported H.264 ConstrainedBaseline/Main/High
`EncSlice` on radeonsi (Renoir), and `vah264enc` was available.

**lintian (first build):** E `no-changelog` and E
`copyright-not-using-common-license-for-lgpl` (both fixed: Debian changelog
and an LGPL-3 common-licenses reference); W `no-manual-page` and W
`copyright-without-copyright-notice` (the notice was added).

**Sender name on the wire:** a 4 s Hisense test-pattern cast with `dumpcap` on
the board's address showed `janus@fedora` in the master-control
`reportClientInfo`/`clientName`, the RTSP `DeviceName` header, the SETUP plist
`machine_name` and the `disconnect` line. Cast and AirPlay carry the same name
inside TLS or encrypted channels and are covered by the shared code. Workspace
tests: 184 passed, 0 failed, 8 ignored (2 new identity tests).

**Final package (rebuilt with the sender name):** in a fresh ubuntu:24.04
container with this laptop's GPU and session, `apt install` succeeded,
`janus --version` printed 0.1.5 with 0 missing libraries, and the smoke test
printed `JANUS_SMOKE_OK` on Wayland and X11. lintian reported no errors; two
warnings remain: `no-manual-page`, and `initial-upload-closes-no-bugs`
(expected for a local package). `apt remove janus` removed the binary and the
desktop entry, leaving no dpkg status. RPM `janus-0.1.5-4.fc44.x86_64.rpm` was
also built; its release binary passed the smoke test.
