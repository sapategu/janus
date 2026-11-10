# AirPlay implementation handoff — 9 November 2026

Start here when continuing the Hisense work. Read [AGENTS.md](../AGENTS.md),
[todo.md](todo.md), [plan.md](plan.md) and the
[wire/profile contract](airplay-control.md). This document records the current
implementation and unresolved failure; it does not qualify the board as working.

## Status update — 10 November 2026

A second AirPlay path now works on real hardware: AzubaTV's licensed AirPlay 2
receiver, through HomeKit PIN pairing, encrypted control and ChaCha20-Poly1305
video and sound (`hap`, `secure`, `pairings`, `session::negotiate_hap`). See
validation.md (10 November entries), todo.md COMPAT-AIRPLAY-PAIRED and AGENTS.md
§5. The Hisense board below is parked: resume only with a MacBook capture.

## Current exit and latest hardware evidence

The target is the Hisense 65WR6CE interactive display, advertised as
**Azuba-Board / AppleTV3,2**. Those labels are documentation/test targeting only;
production code must remain generic. The user can run native Fedora commands
against the board; this agent's sandbox cannot open sockets or access a compositor.

User-supplied host results, in order:

1. `janus cast --name "Azuba-Board" --source test-pattern --no-audio --seconds 10`
   discovered it, then failed with `invalid AirPlay response`.
2. Read-only `diagnostics check-receivers --name "Azuba-Board" --seconds 3 --hold 0`
   passed discovery and live capability: screen=true, ALAC audio=true,
   canvas up to 1920×1080. Authentication/session/media were correctly Pending.
3. Fixed stage/shape errors isolated the first failure to a successful, empty
   **control SETUP** reply. A longer 12-second discovery scan was needed on one
   attempt; another four-second scan found no matching screen.
4. Corrected that parser assumption: successful control-only SETUP may omit its
   optional plist metadata. This is covered by a reproducing scripted regression.
5. Next host `check-receivers --seconds 12 --hold 0 --mirror 10` passes
   discovery/capability, then fails with **`AirPlay request timed out`**.
   That run predates the new timeout-stage labels.
6. The latest labelled host run identifies **`video SETUP`** as the timed-out
   operation. Discovery/live capability still pass. Pairing/FairPlay/control
   precede that step in the implementation. No media transport is established.
7. Compared the descriptor with pinned Doubletake `mirror.go::videoStreamDesc`:
   Janus omitted its five `timestampInfo` dictionaries. Added the names
   `SubSu`, `BePxT`, `AfPxT`, `BefEn`, `EmEnc` in that order. A scripted-peer
   descriptor check fails before the addition and passes after it.
   The user retested: **video SETUP still times out**. The descriptor omission
   was real, but its correction did not resolve this board's startup failure.
8. Added privacy-safe timing counters to negotiation timeout errors. Latest host
   result: **video SETUP timeout; 0 valid timing requests received, 0 replies
   sent**. Counts do not establish whether the board sent nothing or traffic was
   filtered elsewhere. Timing packet response construction now matches the
   pinned sender's >=32-byte request rule and reserved-byte preservation.
9. User checked firewalld: Wi-Fi uses the active FedoraWorkstation zone, which
   permits TCP and UDP **1025–65535**. No rich rules were shown. The displayed
   host zone configuration does not explain blocking ephemeral timing ports;
   no firewall rules were changed. Sandbox D-Bus access is denied.
10. **Confirmed read-only host probe:** `curl --max-time 4 --silent --show-error
    http://192.168.200.163:7100/stream.xml` returned XML properties: width 1920,
    height 1080, overscanned false, refreshRate 1/60, version **200.54**.
    This is evidence for the separately documented older HTTP mirroring
    profile. At that point HTTP sending was not implemented; it is now implemented
    in development source (see the latest result below). Do not claim board
    success or retry authentication failures through a less secure route.
11. Sources read for the next implementation: OpenAirPlay's screen-mirroring
    HTTP requests, stream packets and time-synchronization pages; espes
    Slave-in-the-Magic-Mirror `airplay.py`, commit
    **5a80c28824e804c6b526b65bbb84089f3b9d5391** (commit identified through
    `commit/master.patch`). Its receiver uses HTTP FairPlay on the mirroring
    connection, then `POST /stream` and continuous AES-CTR with the unwrapped
    key and `param2` IV directly. Its Python/Apple-binary interpreter and VLC
    integration must never be bundled or executed by Janus. No code from that
    GPL-2.0-or-later file has been copied into Janus.

12. Native HTTP sending was implemented; see [airplay-http.md](airplay-http.md)
    for ownership, framing, timing, tests and sources. A reproduced response
    half-close bug was fixed. The latest user-run acceptance then passes
    discovery/capability/profile/authentication/session but fails mirroring:
    **Broken pipe (os error 32); 4 frames queued / 3 transmitted; timing
    0 requests / 0 replies**. Timing and teardown remain Pending. No picture or
    clean physical Stop has been confirmed. RPM 0.1.4-2 includes the HTTP code.

13. **Agent-run with packet capture (this host can now reach the board and run
    `dumpcap`; see validation.md, "Agent-run Hisense board diagnosis").**
    The AirPlay service is on port **51010** with `sourceVersion 220.68` and the
    UxPlay/RPiPlay `/info` template (features bit 27 set). HTTP: after
    codec-first and same-connection `/stream.xml`, the board accepts `/stream`,
    then sends **FIN 2 ms after the first codec + encrypted IDR packets**; no
    HTTP reply, no UDP at all. Without `param1` it resets right after `/stream`.
    RTSP (forced for diagnosis): first SETUP → 200 with **no body**; video SETUP
    unanswered. In the lineage's handler an admitted SETUP always returns
    `{timingPort, eventPort}`, so the board returned early. User-Agent, model
    and forced ECDH mixing variants changed nothing.

14. **Key wrap verified correct:** the receiver family's `playfair_decrypt`,
    applied to the real captured `m3`/`param1`, yields a key that decrypts the
    captured IDR to a valid NAL (validation.md). Crypto is not the cause.
    macOS-like SETUP fields/headers did not change the empty RTSP reply.

Current work: find why the board's app does not admit Janus in the first RTSP
SETUP (empty `200`), the receiver's primary path. The HTTP 7100 module is
secondary. Next evidence: whether the board shows an approval prompt or has a
sharing allow-list/code setting, and a packet capture of the user's working
MacBook mirroring to the board (`sudo tcpdump -i en0 -w mac.pcap host
<board-ip>` on the Mac) to compare the first SETUP byte for byte. Note: Janus's key-mixing
rule (`mix` only when feature 27 is clear) follows Doubletake's empirical rule
and is the opposite of UxPlay's receiver rule (mix whenever pair-verify ran);
it only matters once the board admits the session. Completed writes prove neither decode nor display.
Do not infer a crypto, codec or timing cause from these counters alone. Keep
profile selection capability-driven and authentication failures final.

## Implementation map

All adapter modules are flat files under `crates/janus-airplay/src/`:

| File | Implemented responsibility |
| --- | --- |
| `lib.rs` | Public `probe`, `start`, `Sender`, `Stats`, errors; fixed stage/reason labels and timeout stages. |
| `wire.rs` | Sequential RTSP/HTTP response framing, CSeq, session header, bounded binary plist events; incomplete exchanges poison the connection. |
| `session.rs` | Private-LAN endpoints, live `/info`, raw Ed25519/X25519 pairing with verified server proof, fresh FairPlay setup, control/audio/video SETUP and cleanup. |
| `fairplay.rs`, `sap.rs`, `tables.rs` | Native SAP m1–m4, descriptor/key wrapping, SAP hash/circuit and checked upstream tables. |
| `video.rs` | H.264 Annex-B to AVCC configuration, receiver framing and continuous AES-CTR stream encryption. |
| `audio.rs` | ALAC escape frames, RTP/AES-CBC, bounded retransmit history and response budgets. |
| `streaming.rs` | TCP video/event transports, UDP audio/timing, bounded channels, independent RTSP feedback owner, cancellation and TEARDOWN. |
| `testing.rs` | Hidden facts/bytes-only test facade; cannot mint a production live sender or Cast trust credential. |

Integration changes:

| Location | Implemented behavior |
| --- | --- |
| `janus-mirror/src/lib.rs` | Shared capture preparation/H.264 loop, `Transport::{Cast, AirPlay}`, AirPlay ALAC audio, timestamps, cleanup/watchdogs; `MirrorStats::receiver_stop_confirmed`. |
| `janus-mirror/src/receiver.rs` | `supports_receiver`, `start_receiver`, owned `Casting`; shared CLI/GPUI routing while retaining Cast's trust/session. `stop()` returns final stats. |
| `janus-capture/src/audio.rs` | `AudioStream::start_at_rate` permits 44,100/48,000 Hz; PipeWire resamples. Cast's default remains 48 kHz. |
| `janus-cli/src/cast.rs` | Shared entry point, Ctrl+C during startup, exact-name targeting; rejects duplicate names instead of choosing first. Fixed four-second discovery remains unchanged. |
| `janus-cli/src/acceptance.rs` | AirPlay live capability; explicit `--mirror` adds authentication/session/test-pattern transmission and acknowledged teardown. Rendering remains Pending without physical observation. |
| `janus-app/src/main.rs` | Same shared `Casting`; AirPlay Help probe, profile errors, startup AbortHandle, generation cleanup and Stop/window-close handling. |

Existing AirPlay discovery is reused through the typed `Candidate` model. No
EShare, REST, receiver installation, browser, external sender or OS casting-panel
handoff was added. Google Cast authentication/trust has not been replaced.

## Bounds, security and lifecycle

- RTSP: 8 KiB headers, 48 fields, 64 KiB bodies; duplicate headers, conflicting
  framing and incorrect CSeq fail. Requests have a five-second deadline.
- Binary plists: bounded depth, collections, event count and scalar bytes before
  building values. XML plists are currently unsupported and get a clear error.
- Raw pairing verifies the receiver's Ed25519 proof; this is key possession,
  not manufacturer identity or a Google Cast `VerifiedDevice`.
- Fresh session keys/IVs, UUID v4 and locally administered unicast sender ID.
  Plaintext control contains wrapped `ekey`, never clear `shk`.
- One media-first ordering retry is allowed only for explicit control-SETUP
  shape rejection statuses. Never retry a different security scheme after
  authentication, proof or transport failure.
- Empty control-SETUP success is accepted without changing ordering or security.
  Malformed nonempty control and empty audio/video replies still fail. Required
  media ports remain mandatory; no guessed ports or model-specific branches.
- Two queued video frames, sixteen audio packets, video frame bound 4 MiB.
  Audio history retains at most 256 packets; responses are limited per request
  and per second. Retransmitted packet bytes are identical.
- Slow RTSP feedback cannot block the media writer. Stop invalidates local
  sends first and interrupts blocked TCP writes. Failed partial control requests
  are never reused for TEARDOWN; close the connection instead.
- Acknowledged TEARDOWN and TCP writes do not establish rendered video.
  `receiver_checkpoint = -1` indicates unavailable Cast-style frame ACKs.

## Profile and remaining limitations

Implemented slice: legacy raw, no-PIN/password AirPlay, NTP timing, H.264/AES-CTR
over TCP and optional 44.1 kHz stereo ALAC/RTP/AES-CBC. ALAC uses the uncompressed
escape representation (352 stereo samples/packet, about 1.42 Mb/s before overhead).
Video uses software OpenH264 up to 1080p. This does not claim high efficiency,
latency, battery life, 4K casting or general AirPlay 2 compatibility.

PIN/password/HAP/PTP, AAC-only receivers and real macOS/Windows capture/audio
are unsupported. Linux screen/window uses local portal consent; Sound for a
window still means system-wide desktop audio. Physical picture/sound, Stop,
repeated starts and CLI/GPUI screen/window combinations are unverified on this
board. Miracast previously formed a wireless link but never a WFD session; its
Projection Display mode disconnected board Wi-Fi. Keep the board in normal
LAN/AirPlay sharing mode for these tests.

## Tests and verification

Tests live under `tests/test_*.rs`, not source modules:

- AirPlay: FairPlay public/golden vectors, receiver-rule media framing/crypto,
  scripted pairing/setup/teardown, hostile framing/plist bounds, retransmission
  budgets and immediate cancellation.
- New regressions: successful empty control SETUP fails before the fix and passes
  after it, both with Sound off/on. Empty audio/video replies still fail with
  cleanup. A stalled video SETUP must preserve its timeout-stage label and close
  the poisoned connection without RECORD or TEARDOWN reuse.
  Video SETUP's five named timestamp descriptors are checked independently by
  the scripted receiver, including the normal and legacy-order session paths.
- Mirror: actual OpenH264 test-pattern output passes through AirPlay framing and
  encryption, then independent unframing/decryption and H.264 decoding.
- CLI: protocol/key selection and ambiguous-name rejection.

Exact current commands/results are recorded in [validation.md](validation.md).
The latest pre-version-bump full run was **125 passed, 8 failed, 7 ignored**;
all eight failures were sandbox `PermissionDenied` at socket creation (five Cast
session, two Cast streaming, one Cast mirror). AirPlay currently has **25 passing
tests**, including timing vectors; use the latest validation
section for the final full count. Do not
convert the sandbox failures into an all-tests-passed claim.

Desktop builds use `./scripts/build.sh desktop` only. Debug executable:
`target/desktop/debug/janus`. Smoke checks fail here with `NoCompositor`, so
native appearance and shell-icon lookup must be checked by the user. User
screenshots now show the supplied header logo and per-receiver controls. New
RPMs 0.1.1-1 and 0.1.2-1 were built and digest-verified; 0.1.3-1 adds fixed
navigation/list-only scrolling. See validation.md for its final build result.
No package was installed by the agent; do not infer the user's installed version.

## Current native UI

`janus-app/src/main.rs` now owns per-receiver Screen/Window, Sound on/off and
Cast/Cancel/Stop controls. Preferences and element IDs use discovery keys;
the bounded map is pruned when candidates leave the snapshot. Each action selects
its own card's receiver, so duplicate friendly names never select by list index.
Each card has a concise **AirPlay / Google Cast / Janus** protocol pill, as the
user explicitly requested. Do not merge independent protocols into one unverified
identity just because names/models match.

Shared button helper: 44 px targets, Tab/Shift+Tab traversal, Enter/Space,
held-key suppression, white focus outline and disabled tab/activation handling.
Cards/buttons use one-shot 140 ms fill easing on hover entry; no scaling,
bouncing or idle loops. Focusable controls are not inside changing animation
wrappers. Native screen-reader/scale/motion validation remains unverified.

The production Screens header has **Refresh at top right**. The receiver list
owns its ScrollHandle and scrolls separately; the logo, app navigation and
Screens header remain outside that scroller. Other pages scroll their content
below the fixed navigation. Page Up/Down target the content/list scroller.
Active receiver moves first, Cast startup scrolls it into view, and Escape stops
globally; a missing receiver gets a fallback Stop in the fixed Screens header.
Historical `--design-preview` fixtures are separate from production discovery.

## Logo and packaging

`docs/janus-logo.png` is the supplied, unmodified medallion PNG and single source.
The app embeds it with `include_bytes!` into an `Arc<Image>` and displays it at
36 px in the header. Window `app_id` is `janus`.

`scripts/build.sh rpm` stages it as `janus.png`;
`packaging/rpm/janus.spec` installs `/usr/share/pixmaps/janus.png`;
`packaging/rpm/janus.desktop` explicitly sets `Icon=/usr/share/pixmaps/janus.png`.
The obsolete `packaging/rpm/janus.svg` was removed. Current app/core version is
**0.1.3**, RPM Release **1**; both lockfiles changed only local Janus package
versions for these bumps. `janus --version` exits before compositor startup;
Help shows the compiled app version. Build/install instructions and changelogs
track each version. Packaging now includes AirPlay LICENSE, COPYING.GPL,
NOTICE.upstream.md and PROVENANCE.md under `/usr/share/licenses/janus/airplay/`.
External distribution still needs the complete corresponding-source/relink material.

## Dependencies, sources and licensing

The core workspace adds `janus-airplay` and pinned `plist =1.10.1`; existing
AES/CTR/ring are reused. No OS package or runtime Go dependency was added.
Both lockfiles include the adapter and plist dependency graph.

Preserve the desktop GPUI pins: its offline resolver initially failed on the
existing `core-foundation =0.10.0`, so the desktop lock was minimally updated
instead of fully regenerated. Locked desktop check/Clippy/build succeeded.

Primary sender reference: Doubletake
`5e2f1cbe3e1070bcefec8e549c7baf884727bc00`. SAP helper reference: objevovat
`v1.1.0`. Independent references: UxPlay
`4e95a9ec013c52d8555d88dbc55abe346e94d56e` and Apple ALAC
`c38887c5c5e64a4b31108733bd79ca9b2496d987`.
See [PROVENANCE.md](../crates/janus-airplay/PROVENANCE.md) for exact inspected
files, hashes and independent Go vector reproduction. Normalized source snapshots
are a disposable cache in `target/airplay-upstream-5e2f1cb`; golden-vector Go
work was under `/tmp/janus-airplay-golden`, not a build/runtime dependency.

Adapter licensing is conservatively LGPL-3.0-only; SAP retains LGPL-3.0-or-later.
`LICENSE`, `COPYING.GPL` and `NOTICE.upstream.md` are retained. Corresponding
source/relink material remains a binary-distribution gate. No upstream runtime
or physical UxPlay receiver was tested. Shell network fetches fail DNS here;
the connected web-search tool supplied source instead.

## Resume steps

1. Read the current docs/status and inspect any user changes before editing.
   Do not restart the adapter or redo completed crypto work.
2. Trace the implemented HTTP profile and its early connection closure. Use the
   latest host result above; do not repeat the completed half-close correction.
3. Compare preamble/media expectations with pinned primary sources before any
   wire change. Preserve authentication, bounded parsing, timing ownership and
   explicit unsupported audio. Add a regression for any evidence-backed fix.
4. Run focused regression tests and required repository gates; rebuild the GUI
   through the script. Update this handoff, todo, plan and validation with the
   real result. Do not claim board success without a host report.
5. Physical exit: decodable test pattern visible on the board, clean Stop,
   screen/window starts, Sound off/on and repeatability through CLI and GPUI.

```sh
cargo run --locked --bin janus -- diagnostics check-receivers \
  --name "Azuba-Board" \
  --seconds 12 --hold 0 \
  --mirror 10
```

This explicitly attempts a ten-second no-audio test-pattern cast after discovery;
the same command without `--mirror 10` is read-only. Keep `test-pattern` together
when using the separate `cast` command; a prior pasted shell line split that
argument and did not contact the receiver.

Datasheet: [65WR6CE_Datasheet_v2.pdf](65WR6CE_Datasheet_v2.pdf), reread locally.
It describes a 65-inch 3840×2160/60 Hz Android 13 interactive display and wireless
presentation, but supplies no AirPlay/EShare wire specification. Related REST
document: [HCD_DS_REST API_DM66D_ V1.0.1.pdf](HCD_DS_REST%20API_DM66D_%20V1.0.1.pdf),
for a different DM66D family; its management calls are not screen sharing.

Workspace caveat: `.git` is an empty mounted directory in this environment.
Do not invent a git diff/commit result. Do not ask for sandbox escalation;
host networking/compositor checks must be run by the user.
