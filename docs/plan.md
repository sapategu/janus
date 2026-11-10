# Janus implementation plan

## Product contract

Janus is a standalone Rust/GPUI sender application. Install it on the laptop, discover compatible existing TVs/receivers, choose a screen or an application window, and Cast. **AzubaTV is the first physical acceptance target, not a brand/name restriction.** Qualify devices by protocol, capabilities and firmware. No second Janus installation is required for a receiver that already supports a qualified protocol.

Janus owns destination/source selection, connection status, audio controls, Stop and recovery in its native UI. Do not launch Chrome, depend on Google SDK/account/cloud services, or hand control to the OS casting panel. Supported OS capture/audio/graphics APIs and mandatory permission/portal prompts remain valid implementation mechanisms. Application sharing initially means the selected application window; never imply that window selection restricts system-wide loopback audio.

**Independence decision confirmed by the user:** standalone Janus with direct protocol compatibility. Use native protocol implementations for existing receivers and Janus's own MoQ path for Janus-compatible receivers. A TV's firmware determines what it can decode; MoQ is not sent to a receiver that does not implement it. Google Cast is the first interoperability path, followed by independently qualified AirPlay and Miracast paths. No third-party sender app is required. Avoid promising universal support before the device/profile matrix is verified.

The primary flow is **choose destination → choose source → Cast**. Keep cards and labels short, one dominant action, and persistent Stop during sharing. Put protocol names, addresses, codec/timing counters and troubleshooting detail in Help. Discovery is not proof of reachability, identity, idle state, or screen-mirroring support. A reported video output is not an approved mirroring capability. Audio-only receivers cannot be offered as screen/window targets.

Optional Janus receiving starts off. Its open LAN mode allows requests without pairing; receiver Stop disables that mode. This policy applies only to Janus receivers. Existing TVs retain their own authentication, approval/PIN and busy rules; never bypass them or use Janus's open TLS verifier for another protocol. Source consent always remains local.

## Architecture and invariants

Rust 2024, pinned toolchain and dependencies. `janus-app` owns GPUI views; `janus-session` owns lifecycle and grants; `janus-wire` owns Janus-native Protobuf; `janus-cast` owns direct Cast framing/control/authentication-input processing; `janus-discovery` owns DNS-SD; `janus-media` owns deadlines, adaptation and clocks; `janus-cli` produces the `janus` executable; checks live under `janus diagnostics`. The reusable QUIC service belongs to `janus-session`. Keep DNS-SD caching/browser ownership separate from native protocol metadata decoders. Add transport/native adapter crates when they have real implementations; do not create an empty provider framework.

UI uses immutable snapshots and bounded commands; media never flows through a view. Native adapters retain OS thread/fence ownership. Audio callbacks use preallocated rings without locks, allocation or logging. Cancellation is generation-scoped. Stale generations, revoked leases and Stop never reopen capture. Stop invalidates locally before network acknowledgement. Clock discontinuity and source replacement cannot silently retain consent.

Discovery: browse existing Google Cast `_googlecast._tcp.local.` and optional Janus `_janus-cast._udp.local.` receivers. Return typed protocol, untrusted browse identity, friendly name/model, bounded endpoint list, advertised outputs and unknown/readiness state. Bound records, devices, metadata and updates; handle TTL/goodbye/cache flush, split record resolution and duplicate names. Keep selection attached to a browse key, never list position. Pause the browser when leaving Cast. IPv6 scope/interface churn remains a separate qualification task. Later protocol discovery must fit this model without treating every receiver as Google Cast.

Janus-native control only: separate QUIC ALPN `janus-ctrl/1`, 0-RTT disabled, `u32be length | Protobuf Envelope`, 16 KiB bound, typed commands/errors, strict major version and role/state validation. Random connection/session identifiers, exact generations and request sequence. A fresh grant binds media to control exporter context, peer connection identity, namespace and allowed tracks. Open mode grants admission, not identity trust.

The standalone compatibility decision and current behavior are in [docs/receiver-compatibility.md](receiver-compatibility.md).

MoQ evidence is tracked in [docs/moq-qualification.md](moq-qualification.md). Visual geometry and acceptance are in [docs/ui-spec.md](ui-spec.md).

MoQ is the Janus-native transport, not a universal existing-TV wire protocol. Native receiver compatibility must implement that receiver’s session/security/streaming rules. Do not substitute media-URL playback for live screen mirroring. Qualify Rust implementations first; evaluate a contained native library only where it materially lowers correctness/maintenance cost.

MoQ: qualify Cloudflare and moq-dev independently using exact commits, implemented draft, ALPN, native QUIC, credentials/exporter access, pre-media authorization, bounded queues, cancellation and peer vectors. Select a passing standards-compatible implementation; if neither passes, block production media rather than mislabel raw QUIC. Inspect cached implementations as evidence, not as proof of conformance. Record LOC/object mapping and revision separately. Relay E2EE is a later security boundary.

Janus-native media profile: H.264 SDR, no reordering, one access unit/object, IDR groups, live edge, bounded subgroup reset and dependency recovery. Audio: 48 kHz stereo Opus, 10 ms, datagrams only when complete objects fit; bounded stream fallback. Scheduling drops stale work; no media history. Start with two raw staging frames, 4 MiB encoded queues, 2 MiB access-unit limit, bounded codec surfaces and six open subgroups. Measure transport/GPU allocation separately.

Janus-native clocks: NTP-style probes, low-delay affine fit, monotonic domains, uncertainty and drift bounds. Device-clock audio master, bounded ASRC, video scheduled to the same timeline. ±250 ppm correction; reset on device/path discontinuities. Physical tests establish lip sync. No zero-delay or acoustic-echo elimination claims.

## Visual contract

Use the supplied image's charcoal/mint palette and rounded cards, translated into casting views. Initial tokens: background `#171818`, cards `#222323`, raised controls `#454747`, text `#F5F6F5`, muted `#A0A5A3`, accent `#C6F8ED`. Refine through native screenshots; maintain accessible contrast. Compact tokens: 16 px card padding/radius, 12 px gaps and 12/14/18/20 px type hierarchy; pill actions remain at least 44 px high; consistent outlined icons. Bundle a redistributable font before screenshot baselines.

Views: Cast, active session, Settings and Help; expose optional Receive only when its backend works. Cast emphasizes target selection and source/audio controls, with minimal copy. Default UI uses real results; visual fixtures require `--design-preview`. Active view emphasizes status, destination and Stop. Receive provides display choice and persistent incoming state. Network internals stay in Help. Prototype fixtures cover empty/searching/available/busy/permission denied/starting/active/recovering/stopped/error.

Default 840×520, minimum 640×420; inline navigation replaces the dashboard sidebar. Cap content width at 960 px and keep cards at content height. Stack destination/source cards below 700 px. Check 100/150/200% scaling, focus restoration and keyboard-only flows. No claim of pixel fidelity until rendered comparison. Prototype states are clearly labeled; no fake production devices.

## Ordered phases and runnable exits

| Tier | Phase | Depends on | Demonstrable exit |
| --- | --- | --- | --- |
| P0 | Foundation, docs, pins and CI | — | Headless workspace builds and documented commands run |
| P1 | GPUI interface | P0 | Native destination/source flow with minimal copy, screenshots and accessibility checks |
| COMPAT-01 | Standalone receiver discovery | P0; wire UI after P1 | Native Rust DNS-SD finds AzubaTV and other compatible receivers; no Chrome/SDK/cloud/OS casting panel |
| P2 | Connection/session integration | COMPAT-01 | Qualified existing-receiver control/authentication, busy/Stop/reconnect, app-owned UI |
| P3 | Media/timing | P2 | Real native capture/encode → receiver protocol → display; Janus MoQ qualified independently |
| P4 | First Linux casting product | P1–P3 | This Linux laptop → AzubaTV screen/window/audio with Stop/recovery; DEB/RPM candidate |
| P5 | macOS sender | P4 | Native capture/audio and existing-TV end-to-end tests on Apple Silicon |
| P6 | Windows sender | P5 | Native capture/audio/device-loss and existing-TV end-to-end tests |
| COMPAT-02 | AirPlay mirroring | P4–P6 | Direct native compatibility and device/profile matrix, with receiver authentication |
| COMPAT-03 | Miracast | COMPAT-02 | Native in-app flow on supported Wi-Fi/driver/OS rows; no OS casting-panel handoff |
| P7 | Direct v1 qualification | P4–P6, qualified compatibility rows | Verified sender-platform × receiver-protocol/device matrix, performance, accessibility, signed packages and rollback |
| P8 | 4K60/codec efficiency | P7 | Qualified complete profiles within each receiver's actual capabilities |
| P9 | Group playback | P8 | Membership/bandwidth control, calibrated sync, join/leave recovery on qualified endpoints |
| P10 | Relay/routed deployment | P9 | Explicit remote enrollment, reviewed E2EE, revocation and deployment |
| P11 | HDR/higher refresh | P10 | Qualified complete profiles and fallback per receiver/transport |
| P12 | Extended displays | P11 | Signed native display components with safe install/remove/recovery/rollback |
| P13 | Remote input | P12 | Separate grants and supported receivers, visible control and emergency revoke |

GPUI now builds here; native smoke launch is blocked by compositor access. Independent headless P2 work can proceed, but that does not close P1 or authorize native rendering claims. COMPAT-01 can be unit-tested while rendering/network permissions are blocked; neither a synthetic AzubaTV advertisement nor a discovered card closes physical mirroring acceptance. Native adapters and later releases need actual evidence.

## Validation and release

Run fmt, Clippy and core tests on every change; use platform runners for actual native behavior. Tests cover malformed lengths/versions, replay/generation, Stop/reconnect races, output expiry, bounded allocations, discovery poisoning/TTL, clock drift/asymmetry and saturation. Headless loopback tests demonstrate protocol only.

PRD §3 Janus-native performance targets remain: 1080p60 glass-to-glass p95 ≤90 ms Ethernet/≤130 ms Wi-Fi; lip-sync p95 ≤25/35 ms; idle private memory ≤100 MiB; active ≤220 MiB; separate GPU budget ≤128 MiB; Stop capture invalidation p95 ≤100 ms, output blank ≤250 ms healthy or 2 s without liveness. These are targets, not measured results. Existing-TV buffering, authentication and codec/clock behavior require separate measured profiles; do not claim the Janus-native targets for those paths without evidence.

First validate this Linux laptop → AzubaTV without receiver installation, then multiple devices/brands/firmwares per supported protocol and all sender OS rows. Retain the 16 desktop pairs as an additional optional Janus-native matrix. Test install/firewall/privacy on signed packages, IPv4/IPv6, loss/jitter/bandwidth steps, 24 h soak and 100 start/stop cycles. Default support export excludes names, titles, paths, addresses, tokens and media.

Current constraints: the Fedora host runs native smoke, sockets, multicast, PipeWire capture and VA-API encoding (RPM Fusion freeworld driver), and AzubaTV is reachable; other OS lab assets, signing credentials and calibrated physical instruments remain unavailable. P0 scope/assets/signing prerequisites are recorded in [docs/foundation.md](foundation.md); fixture and calibration procedures in [docs/fixtures/README.md](fixtures/README.md). Record each as an evidence gate in todo.md. No silent downgrade of security, native functionality or qualification.

## Immediate next exit

**Done (10 November 2026):** this Linux laptop casts its screen (and sound) to AzubaTV over **Google Cast** and **AirPlay 2** — HomeKit PIN pairing in the CLI and the desktop app, encrypted control, GPU H.264 (VA-API via GStreamer) with software fallback, adaptive full-quality output (the screen's own size up to the TV's maximum, highest sustained frame rate) and size-matched playout delay. The user reports the current quality good. RPM `janus-0.1.4-2` declares all runtime dependencies. Evidence: docs/validation.md; statuses: todo.md.

**Receiver acceptance is generic.** `janus diagnostics check-receivers` and its opt-in test discover every LAN receiver and run each protocol's stages (discovery → device trust → capability → session → mirroring with `--mirror <s>`). No device is named; any new TV/screen is validated by rerunning it.

Ordered path (outside the Hisense board):

1. **Source/audio acceptance** — Window source, sound-only casting and repeated start/Stop through CLI and GPUI on both protocols; record results.
2. **Media quality** — DMA-BUF capture to the GPU encoder for 2K60/4K60 (PERF-ZEROCOPY); bitrate adaptation from Cast RTCP feedback and AirPlay lateness.
3. **DISCOVERY-ALL / COMPAT-DEVICES** — DLNA/UPnP and Miracast-capable receivers in the same picker and sweep; more Cast/AirPlay brands by capability.
4. **App completion** — settings (defaults, forget pairing), sharing indicator, screenshots, keyboard/screen-reader checks (UI03/UI04/B05).
5. **Release** — signing, DEB, LGPL source/relink material, executed CI (D03).
6. **Other senders** — macOS (P5) and Windows (P6); optional Janus-native receiving (MoQ) stays separate.

**Hisense exit (10 November 2026; native, switch removed):** Native EShare test-pattern video and physical Stop now work on the first board (user-confirmed), as do a portal-selected desktop source and the rebuilt GPUI alpha-switch → Cast → Stop flow. Selected-window video plus clean AAC-ELD sound and Stop are also user-confirmed after correcting the native codec to AOT 39/480 samples. The responsible adapter opens mirroring TCP 51040 rather than mDNS AirPlay TCP 51010; explicit EShare identity (no switch) selects this profile. EShare has no cryptographic receiver proof; use only on a trusted LAN. Ordinary AirPlay retains its authentication path. Next exit: user-confirm the VBR/refinement/back-pressure sharpness fix on real desktop content (board canvas 3840×2160 now used), improve measured high-resolution video throughput/latency, then sustain clean window/screen audio-video sharing and repeat acceptance on another firmware. The short screen run does not qualify smooth 1080p30 performance. Measured A/V synchronization, protected/moderated connections and dynamic ports stay outside this verified slice. The historical AirPlay failure remains in the [handoff](airplay-handoff.md); it no longer blocks the explicit EShare route.

## Native desktop build

Run `./scripts/build.sh desktop --run` (installs missing OS packages first, one sudo prompt) to build and launch the GPUI binary directly from `target/desktop/debug/janus`. Omit --run for build only. No separate artifact folder/copy is created. The helper uses the committed desktop lockfile. GPUI compiled and linked here after pinning published libc 0.2.189 for upstream xattr 0.2.3 compatibility. The Fedora validation build used a target-local link to the installed xkbcommon-X11 runtime; install libxkbcommon-x11-devel for the standard build. `--smoke-test` passes natively on Fedora 44 (`JANUS_SMOKE_OK`). The user screenshot confirms the prior layout; no compact screenshot comparison has passed. Native CI covers all four candidate OS rows; its jobs still require execution.


## P0 completion gates

Foundation code/documentation is implemented: both lockfiles, native smoke command/CI, product/OS/asset contract and signing prerequisites, deterministic flash/impulse fixtures, bounded packet impairment, clock driver, event correlation and retained synthetic baseline. See todo.md for individual verified deliverables.

Keep A01/A02/A05 open until the missing physical asset inventory is completed, native smoke evidence is collected on all four rows (including Linux Wayland), and the common-clock physical fixture is calibrated with a retained instrument baseline. Compilation, Xvfb software rendering and synthetic timestamps do not replace those acceptance conditions. No physical or multi-OS result is inferred.

## Native RPM

`./scripts/build.sh rpm` builds the locked release GPUI executable, stages the stripped app/launcher/icon under target/rpm, and packages via native rpmbuild. The current local Fedora 44 x86_64 package is `target/rpm/RPMS/x86_64/janus-0.1.4-2.fc44.x86_64.rpm` (digests OK). It requires GStreamer and recommends its VA plugin plus RPM Fusion's freeworld VA driver for GPU encoding; without them Janus encodes on the CPU. This unsigned package is the current app build; signing and distribution qualification stay open. See [RPM instructions](../packaging/rpm/README.md).

The user's Fedora screenshot confirms prior native rendering and AzubaTV discovery. The compact layout is a new build pending native screenshot comparison. Test its read-only connection phase by selecting the TV, opening Help and choosing Check connection. An unverified reply cannot enable Cast. This probe never sends receiver control or media; device trust and the capture/mirroring slice remain the next implementation gates.
