# Standalone native receiver compatibility

## Accepted behavior

Janus runs on the laptop and speaks supported receiving protocols directly. It owns the device/source picker and session management in GPUI. No Chrome, Google SDK/account/cloud dependency, external sender process or OS casting-panel handoff. Native OS capture/audio/graphics APIs and mandatory privacy prompts remain necessary. AzubaTV is the first real test target; production behavior never matches that name.

Keep the ordinary flow to screen → source → Cast. Protocol, address, capability and permission detail belongs in Help. Optional Janus-to-Janus receiving uses Janus's own control/MoQ path and does not determine support for existing TVs.

## Discovery implemented now

- Native Rust mDNS browses `_googlecast._tcp.local.`, `_airplay._tcp.local.` and `_janus-cast._udp.local.`. An experimental native legacy AirPlay sender now shares the CLI/GPUI flow; physical qualification is pending. See [AirPlay control](airplay-control.md).
- Shared DNS framing/cache/browser ownership is separate from protocol-specific TXT decoding. There is no Google SDK or browser integration.
- Each candidate carries a browse key, protocol, untrusted advertised identity/name/model, at most eight IPv4 LAN endpoints, readiness hint and optional audio/video output flags. DNS identifiers never authenticate ownership.
- Google Cast `id`/`fn` are required; `md` is optional. Decimal `ca` is interpreted only for video-output and audio-output bits. These are hardware advertisements, not successful mirroring/codec/authentication negotiation.
- Candidate keys include protocol and service instance, not friendly name or array position. Duplicate names stay distinct; addresses for the same instance are merged. Different protocols are not merged based on name/address alone.
- Packet/record/TXT/device limits apply before display. Split PTR/SRV/TXT/A replies trigger bounded resolution requests; TTL, goodbye and cache-flush grace prevent indefinite stale routes.
- The browser publishes only the latest bounded snapshot, coalesces changes, pauses outside the Cast page and cancels on drop. Default UI uses real results; `--design-preview` enables fixtures without networking.

The browser uses IPv4 and (best effort) IPv6 default-interface multicast; a named target is rescanned up to three times. Scoped IPv6, interface churn and native permission/firewall/coexistence qualification remain pending. AzubaTV is cast to natively over Google Cast and AirPlay 2 (user-observed, 2026-11-10; see validation.md). The sandbox still denies socket creation. The user supplied a native Janus screenshot showing **AzubaTV / POLYTRON4K**, providing host discovery and prior native-rendering evidence. Exact retail model/firmware, authentication and mirroring remain unverified. Synthetic fixtures separately exercise parsing only.

## Next complete path

1. AzubaTV discovery is observed in the user-provided native screenshot; retain repeatability/expiry/churn and additional-device qualification as open checks.
2. Help → Check connection now probes native TCP/TLS/auth inputs without trusting the receiver. Complete device certificate/path/signature/revocation validation, then query actual state/capabilities and establish a supported live-mirroring session. Preserve any current app/session until the user explicitly starts a cast.
3. Connect native screen/application-window capture, compatible codec/transport, audio timing and Stop/recovery. Media-URL playback alone does not qualify live screen sharing.
4. Render/test the native UI, then qualify source OS × device/protocol/firmware profiles. Add AirPlay and Miracast paths independently, with their own authentication, transport and hardware constraints.

Do not apply Janus's open receive TLS verifier to another protocol. Do not send MoQ to a TV without verified MoQ receiving support. Do not label a receiver “ready to mirror” merely because discovery reports video output.

## Primary references checked

- [Google Cast discovery](https://developers.google.com/cast/docs/discovery).
- [Chromium receiver metadata parsing](https://raw.githubusercontent.com/chromium/chromium/main/chrome/browser/media/router/discovery/mdns/media_sink_util.cc).
- [Chromium Cast output capability bits](https://raw.githubusercontent.com/chromium/chromium/main/components/media_router/common/providers/cast/channel/cast_device_capability.h).
- [Chromium mirroring versus media routing](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/media/media_router.md).
- [GPUI context and asynchronous entity updates](https://docs.rs/gpui/0.2.2/gpui/struct.Context.html).

These references establish protocol/API behavior. They are not proof of a successful connection to the actual TV or a compiled GPUI build.

## Hisense EShare update — 10 November 2026

The earlier legacy AirPlay failure below is historical. A native EShare adapter (always on; the former Settings switch was removed) now owns the separate RTSP 51040 and negotiated video/audio ports. The user confirmed screen video, selected-window video, clean sound and Stop on the first board. No switch is needed and no vendor client is loaded. Sound matches the native AAC-ELD contract (AOT 39, 44100 Hz stereo, 192 kbit/s, 480-sample frames); the initial AAC-LC attempt caused distorted sound and was corrected. No per-frame or audio-playback ACK is inferred from UDP/TCP writes. Other firmware, protected/moderated boards, dynamic control-port negotiation and sustained smooth 1080p remain unqualified. Sources and physical results: [research](hisense_protocol_analysis.md), [validation](validation.md).

## Field result: Hisense 65WR6CE interactive display ("Azuba-Board"), 9 Nov 2026

Generic receiver testing found a second screen on the LAN; it is recorded here so any future attempt starts from facts.

**Latest AirPlay host attempt:** Janus selects the live-qualified legacy HTTP mirroring service on port 7100. Authentication and local stream setup pass, but mirroring fails with `Broken pipe (os error 32)`: 4 frames queued, 3 transmitted, 0 timing requests/replies. No displayed test pattern or physical Stop is confirmed. The earlier RTSP video SETUP timeout remains unresolved for that separate profile. The board exposes an AirPlay receiver; successful Janus screen mirroring remains unverified. See the [HTTP implementation](airplay-http.md) and [handoff](airplay-handoff.md).

- **Advertises:** AirPlay receiver (`_airplay._tcp`, AppleTV3,2 emulation, `srcvers=220.68`), AirPlay audio, ADB. No Google Cast, no DLNA, no Miracast-over-infrastructure (`_display._tcp`/7250). User-supplied native Janus output now confirms live screen/ALAC capability. The first cast attempt failed on an empty successful control-SETUP reply; Janus's parser was corrected and regression-tested. Rendering remains unverified.
- **Board firmware:** Settings → Network → Projection Display opens "ScreenShare" (v5.03.000.038), its Wi-Fi Display sink. While it is on, the firmware disconnects the board's Wi-Fi ("wireless network is not available… disconnect the wireless display first"); only Ethernet would stay up.
- **Miracast test (laptop as source via NetworkManager Wi-Fi Direct):** the board is discovered as a WFD primary sink (session available, port 7236), the P2P group forms every time (board always chooses client role, even with the laptop's GO intent at 0), DHCP works, and Janus's WFD IE (source, available, 7236) was confirmed active on the supplicant during the link. The board then **never opens the RTSP control connection to the source and refuses connections on its own 7236**, and leaves after 6–15 s showing "phone closed". Tried with 10.42.0.1 and 192.168.49.1 addressing, inbound and immediate outbound session start.
- **Conclusion of those tests:** ScreenShare did not start a Wi-Fi Display session with Janus. This is a reproducible failure, not proof that every standard sender must fail. AirPlay is the preferred next implementation path; see the [OSS source investigation](hisense-airplay-research.md).

### Follow-up probe (board online, Projection Display off), 9 Nov 2026

- **Full TCP sweep of 192.168.200.163:** open 4321, 5555 (ADB), 7100 (AirPlay mirroring), 8000, 8121, 9091, 51010 (AirPlay), 51020, 51030, 51040 (RAOP), 51060, 51070, 52020, 52025, 52030, 52333, 53000, 56789, 57395. Closed: 80, 443, 8080, 8091, 36669 (no VIDAA MQTT), 1883.
- **Port 8000 = "EShare Http Server/1.0"**, page "Share Application" offering EShare clients: `/remote/Share.apk`, `/remote/Share.exe`, `/remote/Share.dmg`, iOS App Store id1232526658, FlyPlay.apk. **No Linux client on that page.** The board's ScreenShare (v5.03) is EShare-based. The current [official EShare download page](https://eshare.app/) does offer an Ubuntu/Debian client; this was not tested against the board.
- **Port 51060** answers RTSP with JSON `{"Framerate":30,"casting_win_height":1080,"casting_win_width":1920,"command":-1}` (EShare cast control). **Port 52333** serves `application/x-mpegURL` (HLS, empty). Other ports gave no greeting/HTTP reply. EShare's proprietary sender protocol was not implemented. The earlier claim that AGENTS.md prohibits reverse engineering was incorrect; no such prohibition appears there.
- **Hisense REST API doc** (`docs/HCD_DS_REST API_DM66D_ V1.0.1.pdf`, 136 pages): `http://<ip>:8091/hisense/<service>/<call>`, JSON, optional `Auth-PSK` header. ~70 endpoints for network, input source (`setCurrentInputSource`: HDMI1 1281, HDMI2 1282, DP 1283, Android 2562), picture, volume, power, `shotScreen`, `injectKey`, `installSilentApp`, OTA. **No casting/streaming/open-URL endpoint.** Documented for the DM66D signage model; **port 8091 is closed on this WR6CE board**, so the API is either absent or disabled (enable method not found in the doc yet). `POST /hisense/common/getDeviceInfo` on port 8000 → 404.
- **Hisense downloads page** (hisense-b2b.com/en/downloads): ScreenShare/HiScreenShare/Bytello Share/HiScreenCast clients for Windows, macOS, Android, iOS only; no Linux, no SDK, no protocol docs.
- **Next physical exit:** test the implemented native Rust legacy AirPlay path ([host recipe](airplay-control.md)), using [Doubletake](https://github.com/omarroth/doubletake) as a sender reference and UxPlay as an independent test receiver. Research and implementation/acceptance boundaries are in [hisense-airplay-research.md](hisense-airplay-research.md). No OSS sender was tested on the 65WR6CE in this research session. The official Linux EShare client is a separate possible diagnostic, not a native Janus implementation. Receiver-app installation remains outside the accepted path (previously declined).

