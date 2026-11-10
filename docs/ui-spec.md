# Janus visual implementation contract

The [supplied dashboard image](reference-dashboard.png) is the visual reference. Janus translates its visual language into casting controls. The GPUI source now consumes live discovery by default; the user has supplied a native Fedora screenshot showing AzubaTV/POLYTRON4K. The new compact geometry below supersedes that larger layout; its native comparison remains pending.

## Tokens and geometry

| Token | Value |
| --- | --- |
| Background / card / raised | `#171818` / `#222323` / `#454747` |
| Text / secondary / accent / line | `#F5F6F5` / `#A0A5A3` / `#C6F8ED` / `#393C3B` |
| Track / window outline / error text | `#1C1D1D` / `#2E3130` / `#FFB4A8` |
| Base spacing / card padding / card gap | 4 / 16 / 12 logical px |
| Card / target corners | 16 / 12 px |
| Action / navigation shape | Full pill; main action 44 px; interactive minimum 44 px. Navigation and Screen/Window are segmented controls (recessed track, raised selected segment); Cast is the only mint-filled action; Sound and Resolution are neutral chips showing their state |
| Window chrome (client-side) | 14 px rounded corners, 1 px outline, soft shadow in a 12 px transparent margin that is also the resize grab area; edges tiled by the compositor drop their rounding/outline. Round 30 px Minimize and Close only (Close turns red on hover) |
| Maximize | None: no button, no double-click, and a compositor maximize is undone (user decision 10 November 2026) |
| Typography | 12 px supporting text, 14 px body, 18 px card title, 19 px semibold app name |
| Shell gutter | 16 px; content capped at 960 px |
| Navigation | Inline Cast / Settings / Help; no sidebar or workspace footer |
| Live Cast layout | Full-width receiver list; source/sound/Cast controls belong to each receiver card |
| Scrolling / Refresh | Only receiver list scrolls; app navigation and Screens heading stay fixed; Refresh at top right |
| Window | Initial 840×520; minimum 640×420 |
| Narrow layout | Source/sound/resolution controls wrap within the left group; Cast/Cancel/Stop stays at the right of the same horizontal row |

Prefer a bundled redistributable font before screenshot baselines. Current source uses the system sans serif, so glyph metrics vary by OS and visual fidelity remains unverified. Native window chrome is platform-owned. Refine colors against the supplied image with accessible contrast retained.

## Composition

- Header: 32 px supplied [Janus medallion](janus-logo.png), app name and compact navigation. The original PNG is embedded locally and shared with launcher packaging. Only explicit design fixtures carry the design-preview label; no marketing badge, oversized page heading or workspace footer.
- Cast: each discovered screen has its own Screen/Window choice, Sound on/off and resolution controls on the left, with a separate Cast action aligned at the right of the same horizontal row. No separate Share panel or unsupported Sound-only source. Choices are keyed by browse identity, independent per receiver, and removed when its advertisement expires. Cast uses the key of its own card even if another row was previously selected. Empty state asks to keep the TV awake on the same network.
- Protocol labels: compact AirPlay, Google Cast or Janus pills identify each discovered service. Dual-protocol entries remain separate, with their own controls and keys; matching friendly names are not proof that entries are interchangeable.
- Active: the active receiver moves to the top, shows connecting/sharing status and Cancel/Stop; other controls are disabled. Starting returns the scroll position to the top. Escape stops globally. If the advertisement expires, a fallback Stop remains available.
- Receive: optional Janus-native receiving, hidden from the default UI until implemented. Existing TVs need no Janus installation.
- Settings: read-only facts that are true today (appearance, Auto resolution, what Sound shares, Hisense trusted-network note, AirPlay pairing storage). Unimplemented preferences cannot appear as working persisted settings.
- Help: permission guidance and connection details. Never show estimated or simulated latency as a physical measurement.

Development fixtures require `--design-preview` and are separate from real discovery. They cover ready, empty, busy, permission-denied, active, recovering and error states. Starting and stopped transitions, real receiver output, tray persistence and native source picker are integration work still tracked in todo.md.

## Native visual acceptance

1. Compile and launch the pinned GPUI version. Verify that its renderer and accessibility backend work on the actual OS row before production integration.
2. Capture 840×520, 640×420 and a maximized window at 100/150/200% scale using fixed state fixtures; archive OS, font, scale and renderer.
3. Compare card geometry, typography, alignment, corners and colors against the approved reference; inspect clipping, scroll behavior and wrapping manually. Do not require identical native window chrome across OS families.
4. Test keyboard focus, activation, source/audio controls, dialogs, Escape/Stop, and Orca/VoiceOver/Narrator. Pointer-only div controls in the initial source are not accessibility completion.
5. Compare active and recovering output separately: recovery must blank/mute rather than display an old private image.

GPUI pin and window/style API were checked against the primary [GPUI 0.2.2 documentation](https://docs.rs/gpui/0.2.2/gpui/). Documentation review does not replace compiling or rendering the package.

## Accepted UX refinement

Primary flow: choose Screen or Window and optional Sound on the destination's card → Cast. Short titles and one-line status only. Avoid promotional cards, protocol/version/codec paragraphs and redundant instructions. Show device name, optional short model label, selection and a single main action. Put detailed connection information and actionable permission explanations in Help.

Real discovery is the default. AzubaTV is never inserted into the device list without an advertisement. Preserve selection by service identity through ordering/name changes; remove it when the target expires. Hide known audio-only receivers for screen/window sources. Pause network browsing outside Cast. Native Janus owns the full management UI; no browser or OS casting-panel handoff. OS-required capture consent remains supported.


## Receiver connection check

Choose source/sound on a real receiver card, then Help → Check connection.
Google Cast uses its trust-verified read-only status/heartbeat session. AirPlay
performs live `GET /info` and closes. Neither path launches sharing or changes
volume. Target/address changes, leaving Help, Escape and window closure cancel
pending work; generation checks prevent stale completions. Discovery and a live
capability reply do not prove rendered media. Unsupported protocols remain disabled.

## Card controls and hover behavior

Production navigation, receiver controls and Refresh use a shared button helper:
44 px hit targets, Tab/Shift+Tab traversal, Enter/Space activation, held-key repeat
suppression and a contrasting white focus outline. Page Up/Down scroll the view;
disabled controls are excluded from keyboard activation/tab stops. Window Sound
explicitly says it includes everything the computer plays. These are implemented
keyboard affordances, not completed Orca/VoiceOver/Narrator qualification.

Receiver cards and enabled buttons ease their fill slightly lighter over 140 ms
on hover entry, using GPUI's native one-shot animation. Leaving restores the
base fill. No bouncing, scaling, geometry changes, idle pulse or continuous
render loop is added. Pressed buttons have a subtle opacity response. Decorative
animation children are separate from the stable focusable button, so animation
does not recreate its keyboard focus identity. Receiver IDs use browse keys,
not list indices. This supersedes the screenshot's two-column live Cast layout;
older `--design-preview` fixtures remain separate from production discovery.
