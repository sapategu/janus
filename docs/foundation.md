# Foundation contract and asset inventory

Recorded 9 November 2026. These are qualification rows, not certified support claims.
The accepted product behavior is in [plan.md](plan.md) and PRD R01–R10.

## Locked scope

Ship a standalone Rust/GPUI sender: destination → screen/application window → Cast.
Audio is off initially; window video never implies process-scoped audio. Janus owns
selection, status, recovery and Stop. Native privacy prompts remain mandatory.
No Chrome, Google SDK/account/cloud or OS casting-panel handoff. AzubaTV is the
first test receiver; protocol/capabilities determine eligibility across brands.
Authenticate using the receiver's protocol before streaming. Optional Janus-native
receiving and MoQ do not change existing-TV security or format requirements.

Direct v1 excludes remote input, extended desktops, relay, group playback, HDR and
4K claims. All R01–R10 are release blockers on every claimed path. PRD §3 numeric
gates apply to qualified Janus-native profiles. Existing-TV profiles need their own
physical measurements; no zero-delay, echo-elimination or universal-support claim.

## OS, packaging and provider rows

| Row | Architecture / desktop | Planned channel | Native media provider | Qualification |
| --- | --- | --- | --- | --- |
| Debian 13 | x86_64, GNOME Wayland | DEB, signed APT repository | Portal/PipeWire, VA-API Intel and AMD, explicit monitor audio | Pending |
| Fedora 44; requalify 45 separately | x86_64, GNOME Wayland | RPM, signed repository | Portal/PipeWire, VA-API Intel and AMD, explicit monitor audio | Build on Fedora 44; native launch/media pending |
| macOS 15+ | Apple Silicon | Developer ID signed/notarized application and DMG | ScreenCaptureKit, VideoToolbox, Metal, Core Audio | Pending |
| Supported Windows 11 | x64, supported baseline and current build | Signed installer and update | WGC, D3D11, hardware MFT, WASAPI | Pending |

Confirm vendor security support and exact OS build at each release; “+” never
certifies untested upgrades. Record new compositor/driver/GPU rows separately.
Initial codec profile is H.264 SDR and explicit audio on qualified paths; Opus is
planned for Janus-native transport. Existing receivers negotiate compatible
formats. Provider availability and redistribution/license approval remain A06
and packaging gates; no codec binaries are silently bundled.

## Observed and missing assets

| Asset | Recorded evidence | Missing before qualification |
| --- | --- | --- |
| Local Linux sender | Fedora 44 Workstation, kernel 7.2.13-200.fc44.x86_64; AMD Ryzen 5 PRO 4650U, 6 cores/12 threads; Renoir Vega 1002:1636, amdgpu; GNOME Wayland environment | Physical capture/graphics device access, refresh/audio path and driver validation |
| AzubaTV | User native Janus screenshot shows AzubaTV / POLYTRON4K | Repeatability/churn, exact retail model/firmware, authenticated compatibility, display/DSP modes |
| Debian Intel and AMD nodes | Required reference driver families | Hardware, exact OS/driver/provider inventory and runs |
| Apple Silicon Mac / Windows PC | Required native sender rows | Hardware, OS build, GPU/audio drivers and runs |
| Network | Ethernet E and controlled Wi-Fi W profiles defined in PRD §3 | AP model/firmware/channel, topology, actual RTT/loss and receiver route |
| Measurement fixture | Deterministic assets, event correlation, clock driver and bounded packet shaper implemented | High-speed camera/photodiodes and common-clock audio acquisition; calibration recording/error |

Rust is pinned to 1.96.0; GPUI to 0.2.2. Both Cargo workspaces retain lockfiles.
The sandbox has no `/dev/dri`, denies native UDP sockets and cannot resolve the
registry. These are environment observations, not proof of host incompatibility.

## Build and signing prerequisites

Linux GPUI needs a C/C++ toolchain, pkg-config, OpenSSL, fontconfig/FreeType,
XCB/X11, xkbcommon **including the X11 development library**, Wayland, Vulkan
loader and a usable renderer. Media capture adds PipeWire, VA-API, Opus, ALSA,
libclang (bindgen) and GStreamer (core, plugins-base headers, plugins-bad's VA
plugin) for GPU H.264 encoding. EShare sound uses the pinned safe FDK AAC
wrapper to produce AAC-ELD with the native 480-sample frame size. Its native
reference build needs Git and the existing C++ toolchain; it fetches upstream
FDK at commit `d8e6b1a3aa606c450241632b64b703f21ea31ce3`, or uses
`FDK_AAC_SOURCE_DIR` for an existing matching source tree. This is a build-time
source fetch, with no network service used by encoding at runtime. GPU encoding also needs a VA driver with H.264
encode enabled; on Fedora that is RPM Fusion's `mesa-va-drivers-freeworld`,
and without it Janus uses its software encoder. The single source of truth for Fedora and Debian
package names is the `dependency_lists` function in
[scripts/build.sh](../scripts/build.sh); `build.sh deps` installs whatever is
missing in one sudo prompt, and its `desktop`/`rpm` commands and native CI all
use it.

Earlier on this Fedora host only the `libxkbcommon-x11.so.0` runtime was
present, without the development link. `build.sh deps` installs
`libxkbcommon-x11-devel`, so a normal build needs no workaround. A local validation build linked that existing runtime via a symlink
under `target/desktop/native` and `LIBRARY_PATH`; no system or registry files were
modified. That workaround does not supply missing GPU/display access.

The desktop pins published `libc = 0.2.189`: GPUI's `gpui_http_client` uses
`zed-async-tar 0.5.0-zed`, which requires `xattr 0.2.3`; libc 0.2.190 removed the
Linux `ENOATTR` alias that version uses. `xattr 1.x` cannot satisfy its `^0.2`
requirement. Remove the compatibility pin when the upstream archive chain
supports current xattr, then rebuild all rows. No vendored xattr or registry patch.

Before distributing: reserve product/package/update namespaces; provision Linux
repository signing, Apple Developer ID/notarization and Windows code signing;
configure protected CI secrets outside this repository; approve notices/SBOM,
entitlements/privacy descriptions, installers, update signatures and rollback.
No signing credentials or public repositories are provisioned here. These are
prerequisites, not completed release operations.

## Reproducible exits

`./scripts/build.sh desktop` produces `target/desktop/debug/janus`.
`./target/desktop/debug/janus --smoke-test` opens the native destination view,
waits until a frame has been submitted, prints `JANUS_SMOKE_OK`, and exits.
Failure to reach a frame within 20 seconds fails the process. It neither starts
capture nor proves physical presentation, accessibility or casting performance.
Normal launch remains `./scripts/build.sh desktop --run`.

Native CI defines Debian/Fedora Xvfb+software-renderer smoke jobs and macOS/Windows
native jobs. The user screenshot confirms prior Fedora native rendering/discovery. Hosted execution,
recorded backend/GPU details, other OS rows and compact screenshot comparison remain pending. Merely adding a workflow does not close A02.


## Legacy AirPlay implementation prerequisites

The native Rust `janus-airplay` adapter adds pinned `plist =1.10.1` (event parsing
for explicit bounds) and uses the existing Rust AES/CTR/ring dependencies. It
adds no OS package group or external media process. PipeWire's native rate
conversion supplies 44.1 kHz for ALAC; Cast retains 48 kHz Opus. Both lockfiles
are updated without changing existing desktop pins. The AirPlay port carries
LGPLv3 notices; corresponding source/relink material is required before a new
binary release. See [source provenance](../crates/janus-airplay/PROVENANCE.md).
