# Janus — agent guide

Instructions for anyone (human or AI agent) changing this repository.
`CLAUDE.md` is a symlink to this file. Project status lives in
[docs/todo.md](docs/todo.md); the roadmap in [docs/plan.md](docs/plan.md); evidence in
[docs/validation.md](docs/validation.md). This file holds the rules and
recipes that stay true across tasks.

## 1. Product contract

Janus is **standalone, native casting software** for the desktop, in the
spirit of AirPlay: it auto-discovers every TV and screen on the same LAN and
casts to it from Janus's own UI.

- **No external services.** No Chrome, Google Cast SDK, Google account or cloud
  call, and no hand-off to the OS casting panel. Janus speaks each receiver's
  *local wire protocol* natively in Rust (that is required, not a dependency).
  OS capture/audio/graphics APIs and mandatory privacy prompts (e.g. the
  xdg-desktop-portal screen picker) are allowed.
- **Generic, never device-specific.** AzubaTV (a Google TV, advertised model
  POLYTRON4K) is only the first physical unit. Production code never matches a
  device by name, model or address. Support is decided by protocol and
  verified capability.
- **User flow:** choose destination → choose source (whole screen **or** one
  application window) → optional **include audio** toggle → Cast; persistent
  Stop while sharing. Minimal copy; protocol/address/codec detail lives in Help.
- **Priority order agreed with the user:**
  1. A real, testable cast to the first Cast receiver (AzubaTV) — trust,
     live session and Cast Streaming media.
  2. Auto-discovery of *all* TV/screen kinds on the LAN (AirPlay, DLNA/UPnP,
     Miracast-capable, Janus receivers) in the same picker and acceptance sweep.
  3. Screen or app-window source with the include-audio option everywhere.
- Prefer work that moves toward a cast the user can actually run and test over
  protocol scaffolding.

## 2. Repository map

Two Cargo workspaces, each with a committed lockfile:

| Path | Role |
| --- | --- |
| `Cargo.toml` | Core (headless) workspace; pinned `=x.y.z` dependencies in `[workspace.dependencies]` |
| `crates/janus-app` | GPUI desktop app, **its own workspace and lockfile** (graphics deps kept out of core) |
| `crates/janus-cast` | Native Google Cast protocol (see §5) |
| `crates/janus-discovery` | DNS-SD/mDNS browser, cache, protocol TXT decoding |
| `crates/janus-session` | Janus-native QUIC control, lifecycle, grants |
| `crates/janus-wire` | Janus-native Protobuf control wire |
| `crates/janus-media` | Deadlines, quality ladder, clocks (pure logic) |
| `crates/janus-capture` | Linux capture: xdg-desktop-portal ScreenCast picker (`portal`), PipeWire video (`video`), desktop-audio monitor (`audio`) |
| `crates/janus-encode` | Encoder selection (`video`): GPU H.264 via GStreamer `vah264enc` (`gpu`, Linux) or bundled OpenH264 (`h264`); Opus 10 ms (`opus`); area-averaging frame conversion/scaling (`scale`); test pattern (`pattern`) |
| `crates/janus-airplay` | Native AirPlay sender: AirPlay 2 HomeKit PIN pairing (`hap`, stored in `pairings`), encrypted channels (`secure`), ChaCha20-Poly1305 video/sound; legacy raw pairing, FairPlay SAP, RTSP and older HTTP (port 7100) profiles; see `docs/airplay-handoff.md` |
| `crates/janus-mirror` | The cast pipeline: source → output `plan` (size/fps/bitrate) → encoders → `Session::cast`/AirPlay `Sender`, Stop, auto-end |
| `crates/janus-cli` | The `janus` executable (`cast`, `discover`, diagnostics, acceptance) + lib (`acceptance`, `cast`, `fixture`) |
| `scripts/build.sh` | The only build script (§3) |
| `.github/workflows` | `ci.yml` (Rust checks) → `build.yml` (packages for every OS) → `release.yml` (master only: version, notes, GitHub release) |
| `packaging/rpm` | RPM spec, desktop entry, icon |
| `packaging/deb` | Debian control template, copyright summary, teammate install guide |
| `docs/` | Contracts and evidence: `cast-control.md`, `receiver-compatibility.md`, `validation.md`, `foundation.md`, `ui-spec.md`, `moq-qualification.md` |
| `docs/janus-prd-techdocs.md` | Original PRD (targets, not results) |

`janus-cast` modules (flat files under `src/`):

| Module | Responsibility |
| --- | --- |
| `wire` | Cast V2 `u32be length | protobuf` framing, 64 KiB bound, presence checks |
| `auth` | Fresh nonce challenge; `UnverifiedCredentials` bound to `nonce || TLS leaf DER` |
| `trust` | Offline device trust: embedded anchors, path rules, CRL, signature, audio-only policy |
| `connection` | TCP/TLS + auth exchange; `probe_authentication` (check and close) and crate-only `connect_verified` |
| `control` | Read-only control JSON: CONNECT/CLOSE, PING/PONG, GET_STATUS, GET_APP_AVAILABILITY |
| `session` | App-owned live session: heartbeats, snapshots, bounded reconnect; explicit `cast()` (LAUNCH → OFFER/ANSWER → `Streamer`) and `stop_cast()` |
| `offer` | Cast Streaming OFFER/ANSWER (`urn:x-cast:com.google.cast.webrtc`) |
| `rtp` | Cast RTP packetizer, AES-128-CTR frame crypto |
| `rtcp` | Sender reports; compound RTCP feedback parser (ACK/NACK/PLI/RR) |
| `streaming` | UDP sender engine: pacing, retransmit, reports, key-frame requests |

## 3. Build and dependencies

Use **`scripts/build.sh`** for everything native; do not add other scripts.

```sh
./scripts/build.sh deps [--check] [--yes] [--ci]   # install missing OS packages
./scripts/build.sh desktop [--run]                 # GPUI app → target/desktop/debug/janus
./scripts/build.sh rpm                             # release RPM → target/rpm/RPMS/<arch>/
./scripts/build.sh deb [--container [IMAGE]]       # .deb → target/deb/ (Ubuntu 24.04 container off Debian)
./scripts/build.sh version X.Y.Z                   # CI: stamp a release version into the checkout
```

- One function per job: `install_deps`, `build_desktop`, `build_rpm`,
  `build_deb` (+ `build_deb_container`). `desktop`, `rpm` and `deb` call `ensure_deps`, which installs **every** missing
  package in one package-manager transaction (a single sudo prompt) and is a
  no-op when all are present.
- Package names live **only** in `dependency_lists` (Fedora and Debian groups:
  build, desktop, media, package, ci). CI calls `./scripts/build.sh deps --yes --ci`.
- **Adding a native dependency:** add it to the right group for both families
  in `dependency_lists`, then mention it in `docs/foundation.md` if it changes
  prerequisites. Never tell users to `dnf install` by hand.
- **Adding a build job:** a new `build_<job>` function plus a dispatch case and
  a usage line at the top of `build.sh`.
- GPU encoding needs GStreamer (core + plugins-base headers, plugins-bad VA
  plugin; all in `dependency_lists`) and a VA driver with H.264 encode (on
  Fedora: RPM Fusion `mesa-va-drivers-freeworld`, recommended by the RPM).
  Without it Janus uses OpenH264. `JANUS_VIDEO_ENCODER=software` forces it.
- An agent cannot type the user's sudo password. Ask the user to run sudo
  steps in their own terminal (e.g. `./scripts/build.sh rpm`), then continue.
- Rust toolchain is pinned in `rust-toolchain.toml` (1.96). Build and test
  with `--locked`. When adding a crate: pin `=x.y.z` in
  `[workspace.dependencies]`, prefer versions already in a lockfile, and check
  that only the intended lockfile entries changed. The desktop lockfile
  (`crates/janus-app/Cargo.lock`) must be updated too when a core crate it uses
  gains a dependency.
- RPM releases: bump `Release:` and add a `%changelog` entry in
  `packaging/rpm/janus.spec`; update the install command in
  `packaging/rpm/README.md`.
- DEB releases: the Debian revision is the `-N` in `packaging/deb/control`
  (`Version: @VERSION@-N`); dependencies come from `dpkg-shlibdeps` plus the
  dlopened/runtime list there. Never build the `.deb` on Fedora directly
  (glibc mismatch); use `deb --container`. See `packaging/deb/README.md`.
- **Releases are automatic** from `master` (`release.yml`): CI → next version
  from the commits since the last `vX.Y.Z` tag → `build.yml` with that version
  stamped (`build.sh version`) → GitHub release with generated notes. Write
  commit subjects as `[crate]: <verb> …` (e.g. `[airplay]: improve …`) or
  Conventional Commits; `add/implement/introduce/support/enable` or `feat` →
  minor, `fix`/other verbs → patch, `!`/`BREAKING CHANGE:` → major,
  `[docs]`/`[ci]` alone → no release. No co-author trailers in commits.

## 4. Code and test layout

- **Flat modules.** Every module is a file directly in the crate's `src/`
  (e.g. `crates/janus-cast/src/rtp.rs`). No module subdirectories.
- **Tests live in `crates/<crate>/tests/test_<topic>.rs`.** No `#[cfg(test)]`
  modules or `*_tests.rs` files in `src/`.
  - Shared test-only helpers: `tests/support/mod.rs` (with
    `#![allow(dead_code)]`), included via `mod support;`. Not a test target.
  - Test vectors: `crates/<crate>/testdata/` with provenance recorded nearby.
  - Binary-only crates expose testable code through a small lib target
    (`janus-cli/src/lib.rs`).
- **Reaching internals from tests:** expose the narrowest item possible, marked
  `#[doc(hidden)]`, or a `#[doc(hidden)] pub mod testing` facade. A testing
  facade must never be able to mint a production trust type (e.g. it returns
  plain facts, not `trust::VerifiedDevice`).
- `unsafe_code` is forbidden workspace-wide; Clippy runs with `-D warnings`.
- Debug output of protocol types redacts payloads, identifiers, keys and
  credentials.

## 5. Receiver protocol rules

### Discovery (`janus-discovery`)
Browse `_googlecast._tcp.local.` and `_janus-cast._udp.local.`; results carry a
browse key, protocol, untrusted name/model, ≤8 private endpoints and advertised
outputs. Discovery proves nothing about identity, idle state or mirroring.
Selection is keyed by browse key, never list position. New protocols must fit
the same `Candidate` model and join the acceptance sweep (§6).

### Google Cast (`janus-cast`)
- **Trust before control.** No control message beyond the auth challenge is
  sent until `trust::verify` succeeds for *this* TLS channel. Trust data is
  embedded (`crates/janus-cast/trust/`, hashes and sources in its README):
  Cast Root CA and Eureka Root CA for devices, Cast CRL Root CA for CRLs.
  Revocation follows Chromium `CRL_REQUIRED_WITH_FALLBACK`: device CRL, else
  the embedded fallback CRL (window = `FALLBACK_CRL_REFRESHED_UNIX` + 20 weeks;
  its signer is checked at its published time and does **not** cap the window).
  No usable CRL → `RevocationUnavailable`. Never add an open-verifier fallback
  or reuse Janus's own open-receive TLS verifier for Cast.
- **Refreshing trust data** (each release, before 2027-03-29): re-extract the
  four files from the upstream sources listed in `trust/README.md`, update the
  hashes, bump `FALLBACK_CRL_REFRESHED_UNIX`, rerun `test_trust.rs`.
- **Read-only until the user casts.** CONNECT, GET_STATUS, GET_APP_AVAILABILITY
  and heartbeats never disturb the TV. LAUNCH/STOP of the mirroring app
  (`0F5096E8` screen, `85CDB22F` audio) happen only on an explicit Cast/Stop.
- **Audio-only policy** (certificate OID 1.3.6.1.4.1.11129.2.5.2 anywhere in the
  path) forces screen mirroring off.
- **Session** (`session`): 5 s heartbeat, 10 s liveness; retry budget refills
  only after a heartbeat reply; trust/auth/TLS/permission failures are final.
- **Cast Streaming** (`offer`, `rtp`, `rtcp`, `streaming`): Android/Google TV
  payload types video 96 / audio 127; video timebase 90 kHz, audio 48 kHz;
  per-stream random AES key and IV mask; SSRC ranges audio 1–50000, video
  50001–100000. Frames: key frames reference themselves, predicted frames the
  previous ID. Dropped frames and receiver picture loss raise
  `key_frame_requests`, which the encoder must honour.
- **Casting flow** (`Session::cast`): LAUNCH `0F5096E8` (or `85CDB22F` for
  audio-only) → wait for its RECEIVER_STATUS entry → CONNECT to its
  `transportId` → OFFER on `urn:x-cast:com.google.cast.webrtc` → ANSWER →
  `Streamer` to `peer_ip:udpPort`. Transport messages are routed before
  receiver-control parsing. Failures after launch STOP the app again; closing
  the session while casting STOPs it so the TV returns to its previous screen.
- **Pipeline** (`janus-mirror`): video thread takes the latest captured frame
  (or test pattern), converts to BGRA at ≤1080p, encodes H.264 and sends; a
  static screen is resent once per second; key-frame requests force IDR.
  Audio thread: PipeWire monitor → Opus 10 ms (RTP +480 per packet). The
  pipeline ends itself when the session reports the cast ended or failed.
- **Primary sources.** Port behaviour from Open Screen / Chromium sources and
  record the commit (current: Open Screen `308a18e24e717ce27231c0aaa713364a41416e54`).
  Fetch and read the source before implementing a protocol detail; don't
  write wire formats from memory.

### Other receivers
AirPlay, DLNA/UPnP and Miracast get their own adapters with their own
authentication rules. Never send MoQ or Janus control to a receiver that does
not implement it. Media-URL playback does not count as live screen mirroring.

### AirPlay (`janus-airplay`)
- Profile by capability only: feature 27 clear and 43/46/48 set → HomeKit
  (HAP) pairing (HKP type 3 for MFi SDK receivers, features 26/51; else 5);
  otherwise the legacy raw-pairing/FairPlay profiles. Reference: Doubletake
  `5e2f1cb` (see `crates/janus-airplay/PROVENANCE.md`).
- PIN pairing is an explicit user step (`janus pair`, or the app's pairing card
  when a cast reports "not paired"). The pairing is stored owner-only in
  `~/.config/janus/airplay-pairings.plist`, keyed by the receiver's deviceID.
- Always verify the receiver's Ed25519 signatures (pair-setup M6, every
  pair-verify V2) and the SRP server proof; never fall back to an unpaired or
  unencrypted path.
- Never change the receiver's volume (`volume: 0.000000` is full scale).
- Receivers may answer slowly (pair-pin-start 6.5 s, RECORD with sound 4.7 s);
  only those steps get longer deadlines.

## 6. Testing and acceptance

Run on every change (core workspace):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Desktop app: `cargo fmt --manifest-path crates/janus-app/Cargo.toml`, Clippy
with `--manifest-path crates/janus-app/Cargo.toml --target-dir target/desktop`,
then `./scripts/build.sh desktop` and `./target/desktop/debug/janus --smoke-test`.

**Generic receiver acceptance** — the definition of "works on a TV":

```sh
cargo run --bin janus -- diagnostics check-receivers [--seconds 5] [--hold 12] [--name "<exact>"]
cargo test -p janus-cli --test test_native_receivers -- --ignored --nocapture
```

Both run `janus_cli::acceptance::check_receivers`: discover every receiver,
then run its protocol's stages (Google Cast: discovery → device-trust →
capability → session → mirroring). Any failing implemented stage fails the
run. The mirroring stage is opt-in (`--mirror <seconds>` /
`JANUS_TEST_MIRROR_SECONDS`): it casts the test pattern and requires the TV to
acknowledge at least half the frames; without it the run is read-only.
Casting by hand: `janus cast [--name X] [--source screen|window|test-pattern]
[--no-audio] [--seconds N]`. Rules:

- Validate by protocol and capability; plugging in another TV and rerunning
  must validate it with no code change. Name filters are optional targeting
  only.
- **Every new capability adds a stage** to that sweep (e.g. the mirroring
  stage when Cast Streaming media works; each new protocol's stages).
- Hardware/network tests are opt-in `#[ignore]` tests named `test_native_*`
  and must be generic (`JANUS_TEST_RECEIVER_NAME`, `JANUS_TEST_MIN_RECEIVERS`,
  `JANUS_TEST_HOLD_SECONDS` as optional knobs).
- Protocol tests use real public vectors where they exist (e.g. Open Screen
  device chains and CRL suite) and the receiver's own parsing rules, not only
  round-trips through Janus's own encoder.
- Assert that suites are not vacuous (e.g. count cases per category).

## 7. Security and privacy invariants

- Stop invalidates locally before any network acknowledgement; stale
  generations never reopen capture.
- Bounded everything: frame sizes, queues, message counts, reconnects.
- Source consent is always local (portal picker); window selection never
  implies that system-wide audio is excluded — say so in the UI.
- Default support exports exclude names, titles, paths, addresses, tokens and
  media. Don't commit a user's device certificates or identifiers as fixtures
  without asking.
- No silent downgrade of security, native functionality or qualification.

## 8. Desktop UI (`crates/janus-app`)

Cast page: pick a discovered screen, Screen/Window source, Sound on/off,
resolution (Auto = the screen's own size up to the TV's maximum at the highest
sustained frame rate; 720p/1080p/2K/4K manual), then Cast. Cast runs
`janus_mirror::start_receiver` on the app's tokio runtime (2 workers); the
button becomes Cancel/Stop · Esc; window close stops the cast. An unpaired
AirPlay PIN receiver opens the pairing card (keyboard code entry). On
client-side-decorated desktops (GNOME/Wayland) Janus draws minimize and close
only (no maximize: the window never maximizes and a compositor maximize is
undone), header drag and edge resize, 14 px rounded corners with a shadow inset. Help → Check
connection is the read-only live session.


Charcoal/mint tokens (background `#171818`, cards `#222323`, raised `#454747`,
text `#F5F6F5`, muted `#A0A5A3`, accent `#C6F8ED`); 840×520 default, 640×420
minimum; 44 px pill actions. Views consume latest-value snapshots (tokio
`watch`); media never flows through a view. Live discovery is the default;
fixtures only with `--design-preview`. Cast stays disabled until the mirroring
path works for the selected receiver. Details: `docs/ui-spec.md`.

## 9. Documentation and evidence

- Update `docs/todo.md` statuses (**Not started / In progress / Blocked /
  Verified**) with what was actually verified, and `docs/plan.md`'s next exit.
- Record reproducible commands and real results in `docs/validation.md`;
  protocol behaviour and sources in `docs/cast-control.md`.
- Distinguish synthetic tests from native/physical evidence. Never claim a
  result that was not run; report failures and counts exactly as observed.

## 10. Recipes

**Add a receiver protocol.** Decoder in `janus-discovery` mapping to
`Candidate` (+ unit tests in `tests/test_discovery.rs`); adapter crate or flat
module with its own auth/session; stages in `janus-cli/src/acceptance.rs`;
UI labels by verified capability; docs in `receiver-compatibility.md`.

**Add an acceptance stage.** Extend the protocol branch in
`acceptance.rs` with a `Check { stage, outcome }`; `Pending` until implemented,
`Fail` with a concrete reason on failure; mention it in `docs/todo.md`'s acceptance
rule and `docs/cast-control.md`.

**Add a Cast control message.** Builder + bounded parser in `control.rs`
(source format from Chromium `cast_message_util.cc`), wire it into the
`session.rs` state machine, test it in `tests/test_control.rs` and against the
scripted receiver in `tests/test_session.rs`.

**Change Cast Streaming behaviour.** Read the matching Open Screen file
(`sender_impl.cc`, `rtp_packetizer.cc`, `compound_rtcp_parser.cc`/`rtp_wire.rs`,
`offer_messages.cc`, `answer_messages.cc`), port it, and extend
`tests/test_streaming.rs` (receiver-rule parsing and loopback receiver).

**Add a native dependency.** `dependency_lists` in `scripts/build.sh` for
Fedora and Debian; pin the Rust crate; Linux-only crates go under
`[target.'cfg(target_os = "linux")'.dependencies]` so macOS/Windows core CI
still builds.
