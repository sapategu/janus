# Janus

Native desktop casting for Linux, in the spirit of AirPlay. Janus finds the TVs and displays on your local network and mirrors your whole screen or one window, with optional desktop sound, speaking each receiver's own protocol natively in Rust. It needs no Chrome, Google SDK or account, cloud service, or the OS casting panel. Receivers need no Janus installation.

## Features

- One picker for every receiver on the LAN: Google Cast, AirPlay and Hisense/EShare interactive displays (DNS-SD/mDNS plus EShare's own UDP discovery).
- Screen or single-window source through the xdg-desktop-portal picker (PipeWire). Desktop sound is optional and covers everything the computer plays.
- **Google Cast**: offline device-trust verification (Cast root/CRL anchors embedded, no account or cloud), read-only live session with heartbeats, and Cast Streaming (AES-CTR RTP, RTCP feedback, retransmission, key-frame requests) ported from Open Screen.
- **AirPlay**: AirPlay 2 HomeKit PIN pairing with stored credentials, ChaCha20-Poly1305 encrypted video and sound, and the legacy FairPlay SAP, RTSP and HTTP profiles. Ed25519 signatures and SRP proofs are always verified, with no fallback to an unpaired path.
- **Hisense / EShare boards**: native RTSP control, the board's own canvas (up to 3840×2160), honoured key-frame requests, and AAC-ELD sound matching the vendor sender.
- H.264 on the GPU through GStreamer VA (`vah264enc`): VBR with a quality ceiling, refinement when the picture settles, and TCP back-pressure with a network-following bitrate. The bundled OpenH264 is used as the fallback.
- Resolution **Auto** casts the screen's own size up to the receiver's maximum, never upscaling; 720p/1080p/2K/4K can be chosen manually.
- Receivers list this computer as `janus@<hostname>` (e.g. `janus@fedora`).
- Compact native GPUI app (Wayland and X11) with rounded client-side window, persistent Stop (Esc), and window close that stops the cast.
- `janus` CLI for scripted casts, pairing, discovery and read-only per-receiver acceptance checks.

## Requirements

- Linux desktop with Wayland or X11, PipeWire and xdg-desktop-portal (default on Fedora 44 and Ubuntu/Kubuntu 24.04).
- A Vulkan-capable graphics driver (Mesa on Intel/AMD).
- GStreamer 1.x with plugins-base. For GPU encoding: plugins-bad (VA plugin) and a VA driver that can encode H.264.

macOS and Windows builds are published as previews: the app runs, but screen capture and casting are implemented for Linux only so far.

### Fedora

```sh
sudo dnf install ./janus-<version>-1.fc44.x86_64.rpm
```

Fedora's own Mesa VA driver has H.264 encoding disabled. For GPU encoding, enable RPM Fusion and install `mesa-va-drivers-freeworld`; without it Janus encodes on the CPU. See [packaging/rpm](packaging/rpm/README.md).

### Ubuntu / Kubuntu / Debian

```sh
sudo apt install ./janus_<version>-1_amd64.deb
```

Use `apt` with the `./` path (not `dpkg -i`) so the dependencies install too. Ubuntu's `mesa-va-drivers` encodes H.264 on AMD; on Intel install `intel-media-va-driver`. See [packaging/deb](packaging/deb/README.md).

Check GPU encoding with `vainfo | grep EncSlice` and `gst-inspect-1.0 vah264enc`.

## Tested devices

Each device was tested from a Fedora 44 laptop (AMD Renoir). Please report others that work or fail.

- **Google TV** (POLYTRON 4K): Google Cast mirroring with sound; AirPlay 2 after one-time PIN pairing.
- **Hisense 65WR6CE interactive display** (EShare): screen and window casting with clean AAC-ELD sound, Stop.
- **Macs as AirPlay receivers** (Mac17,2, Mac17,5, Mac14,2): not supported. Janus targets TVs and displays. Macs ask for a first-time code over a pairing flow Janus doesn't implement, so they report "requires pairing".

Support is decided by protocol and verified capability, never by device name or model: another receiver is checked by running `janus diagnostics check-receivers` with no code change.

## Install a release

Download the package for your system from the [Releases](../../releases) page (`.deb`, `.rpm`, macOS `.tar.gz`, Windows `.zip`, plus `SHA256SUMS`) and install it as shown above. Open **Janus** from the applications menu.

## Build

```sh
./scripts/build.sh deps                     # install missing OS packages (one sudo prompt)
./scripts/build.sh desktop --run            # build and open the app (target/desktop/debug/janus)
./scripts/build.sh rpm                      # Fedora package  -> target/rpm/RPMS/<arch>/
./scripts/build.sh deb                      # Debian/Ubuntu   -> target/deb/
./scripts/build.sh deb --container          # .deb from any Linux host (Ubuntu 24.04 container)
```

The Rust toolchain (1.96) comes from `rust-toolchain.toml` through rustup. `scripts/build.sh` is the only build script; `desktop`, `rpm` and `deb` install missing packages first. The headless core and the GPUI desktop app are separate Cargo workspaces, each with its own lockfile.

Run the checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
./target/desktop/debug/janus --smoke-test     # renders one native frame and exits
```

## Usage

### Desktop app

1. Pick a screen from the list (all receivers on the LAN appear automatically).
2. Choose **Screen** or **Window**, **Sound on/off** and **Resolution**.
3. Press **Cast** and choose the source in the system dialog.
4. Press **Stop** (or Esc) to return the TV to what it showed before.

An AirPlay TV that shows a code opens a pairing card the first time. Help → Check connection tests the selected TV read-only, without sharing anything.

### Command line

The CLI is built from source (`cargo run --bin janus -- …`; the packaged `janus` is the desktop app):

```sh
# Cast the screen with sound to the only receiver found
janus cast

# Choose a receiver and a window, without sound, for 20 seconds
janus cast --name "Living Room TV" --source window --no-audio --seconds 20

# Moving test pattern (no capture permission needed)
janus cast --source test-pattern --seconds 10

# Force a protocol, size or bitrate
janus cast --name "Living Room TV" --protocol airplay --resolution 1080p --mbps 8

# Pair once with an AirPlay TV that shows a code
janus pair --name "Living Room TV"

# List receivers, then check each one read-only (never interrupts the TV)
janus discover --seconds 5
janus diagnostics check-receivers
janus diagnostics check-receivers --name "Living Room TV" --mirror 10   # also casts a test pattern
```

#### `janus cast` flags

| Flag | Default | Description |
|------|---------|-------------|
| `--name` | | Exact receiver name (needed when several are found) |
| `--protocol` | | `airplay` or `cast`, when one TV is listed under both |
| `--source` | `screen` | `screen`, `window` or `test-pattern` |
| `--no-audio` | off | Don't send the computer's sound |
| `--seconds` | | Stop automatically after this many seconds |
| `--resolution` | `auto` | `auto`, `720p`, `1080p`, `2k`/`1440p`, `4k`/`2160p`; never upscales |
| `--mbps` | by resolution | Video bitrate in Mbit/s |
| `--latency-ms` | per protocol | Receiver playout delay |
| `--fps` | 60 | Frames per second; the receiver and encoder may deliver fewer |

`JANUS_VIDEO_ENCODER=software` forces the OpenH264 encoder.

### Hisense / EShare boards

Boards that identify themselves as EShare are detected automatically. EShare casting is **not encrypted** and the board does not prove its identity, so use it only on a trusted network. Ordinary AirPlay receivers never fall back to this profile.

## Releases and CI

| Workflow | Runs | Does |
|----------|------|------|
| [CI](.github/workflows/ci.yml) | pull requests, branch pushes; called by Release | fmt, Clippy and tests of both workspaces on Linux, macOS and Windows |
| [Build](.github/workflows/build.yml) | after CI succeeds; called by Release | `.deb` (Ubuntu 24.04, installed and run), `.rpm` (Fedora 44), macOS and Windows archives (smoke-tested) |
| [Release](.github/workflows/release.yml) | pushes to `master` only | CI → next version and notes → Build → GitHub release with packages and `SHA256SUMS` |
| [Cache cleanup](.github/workflows/cleanup.yml) | after CI, Build or Release completes; when a pull request closes | keeps only the newest cache of each kind per branch; deletes a closed pull request's caches |

Versions follow the commits since the last `vX.Y.Z` tag. Both Conventional Commits (`feat(airplay): …`, `fix: …`) and scoped subjects (`[airplay]: improve …`, `[cast]: feat: …`) are understood:

- **major**: `!` after the type, or a `BREAKING CHANGE:` footer;
- **minor**: `feat`, or a scoped subject starting with add/implement/introduce/support/enable;
- **patch**: `fix`/`perf`, or any other scoped change (improve, update, …);
- **no release**: `[docs]`, `[ci]`, and docs/ci/chore/test/style/refactor types alone.

Each release's notes are generated from those commits (breaking changes, features, fixes, improvements, other), with download and checksum instructions.

## Project layout

| Path | Role |
|------|------|
| `crates/janus-discovery` | DNS-SD/mDNS and EShare discovery, receiver model |
| `crates/janus-cast` | Google Cast: trust, control, session, Cast Streaming |
| `crates/janus-airplay` | AirPlay 2/legacy sender and the Hisense/EShare adapter |
| `crates/janus-capture` | Portal picker, PipeWire video, desktop-audio monitor |
| `crates/janus-encode` | GPU/OpenH264 H.264, Opus, AAC-ELD, scaling |
| `crates/janus-mirror` | The cast pipeline: plan → encode → send, Stop |
| `crates/janus-media` | Timing, quality and sender-identity policies |
| `crates/janus-session`, `janus-wire` | Janus-native QUIC control and wire format |
| `crates/janus-cli` | `janus` CLI and acceptance checks |
| `crates/janus-app` | GPUI desktop app (own workspace) |
| `docs/` | Protocol notes, validation evidence, plan and status |

Contributor and agent rules are in [AGENTS.md](AGENTS.md). Status lives in [docs/todo.md](docs/todo.md), evidence in [docs/validation.md](docs/validation.md).

## References

Janus is a native Rust implementation. These projects and sources were read and ported from, or used to check behaviour. Commits are those recorded in the provenance files.

- [Open Screen](https://chromium.googlesource.com/openscreen/) (The Chromium Authors) at `308a18e24e717ce27231c0aaa713364a41416e54`: Cast Streaming sender, RTP packetizer, compound RTCP parser, OFFER/ANSWER messages, the certificate validator, and the embedded Cast root/Eureka/CRL anchors.
- [Chromium](https://chromium.googlesource.com/chromium/src/): Cast control messages (`cast_message_util.cc`), auth (`cast_auth_util.cc`), CRL handling (`cast_crl.cc`) and the fallback CRL (`cast_fallback_crl.h` at `256d14612ecd3ab6b42283197a5d34a3d9f07008`).
- [Doubletake](https://github.com/omarroth/doubletake/tree/5e2f1cbe3e1070bcefec8e549c7baf884727bc00) (Omar Roth) at `5e2f1cb`: AirPlay pairing, mirroring, audio and FairPlay SAP flows, the primary AirPlay sender reference (LGPL-3.0-or-later).
- [FairPlay SAP core](https://github.com/objevovat/fairplay-sap-core-airplay2-sender-authentication-handshake/tree/v1.1.0) (objevovat) at `v1.1.0`: byte helpers, SAP hash and nonlinear circuit.
- [UxPlay](https://github.com/FDH2/UxPlay/tree/4e95a9ec013c52d8555d88dbc55abe346e94d56e) at `4e95a9e`: receiver-side key and mirror-buffer checks (no code incorporated).
- [AirPlay internals](https://air-display.github.io/airplay-internal/mirroring_service.html): mirroring-service notes.
- [Apple ALAC](https://github.com/macosforge/alac/tree/c38887c5c5e64a4b31108733bd79ca9b2496d987) at `c38887c`: escape-frame element format for the small ALAC encoder.
- [Fraunhofer FDK AAC](https://github.com/mstorsjo/fdk-aac) at `d8e6b1a3aa606c450241632b64b703f21ea31ce3`, via [`fdk-aac-rust-sys`](https://crates.io/crates/fdk-aac-rust-sys/0.2.3): AAC-ELD sound for EShare boards.
- [OpenH264](https://github.com/cisco/openh264) via the [`openh264`](https://crates.io/crates/openh264) crate: software H.264.
- [GStreamer](https://gstreamer.freedesktop.org/) VA plugin: GPU H.264.
- [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (Zed Industries): the desktop UI framework.
- EShare / Hisense: behaviour derived from analysing the vendor's own sender clients; see [docs/hisense_protocol_analysis.md](docs/hisense_protocol_analysis.md). No vendor code is shipped.

## Disclaimer

Much of this project was written with AI coding agents and then reviewed and tested on the devices listed above. Treat it as pre-release software. Don't use it in production or security-sensitive environments. EShare casting is unencrypted by design.

## License

No license has been selected for the Janus project itself yet, so no rights are granted beyond those below. Bundled third-party code keeps its own terms:

- the native AirPlay port in `crates/janus-airplay` is LGPL-3.0-only AND LGPL-3.0-or-later; see its [LICENSE](crates/janus-airplay/LICENSE), [COPYING.GPL](crates/janus-airplay/COPYING.GPL) and [PROVENANCE](crates/janus-airplay/PROVENANCE.md);
- Fraunhofer FDK AAC follows its own license; see [NOTICE.FDK-AAC](crates/janus-encode/NOTICE.FDK-AAC);
- OpenH264 is BSD-2-Clause; the Cast trust anchors are BSD-style (The Chromium Authors).
