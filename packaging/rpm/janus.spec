# Binary packaging of the locked native GPUI build made by scripts/build.sh rpm.
%global debug_package %{nil}
# The input executable is already stripped; no host BRP rewriting is needed.
%global __os_install_post %{nil}

Name:           janus
Version:        %{janus_version}
Release:        4%{?dist}
Summary:        Native desktop casting application
# No project license has been selected. This local package grants no new rights.
License:        LicenseRef-Unspecified AND LGPL-3.0-only AND LGPL-3.0-or-later AND LicenseRef-FDK-AAC
Source0:        janus
Source1:        janus.desktop
Source2:        janus.png
Source3:        README.md
Source4:        janus-airplay.LICENSE
Source5:        janus-airplay.COPYING.GPL
Source6:        janus-airplay.NOTICE.upstream.md
Source7:        janus-airplay.PROVENANCE.md
Source8:        janus-encode.NOTICE.FDK-AAC
Source9:        janus-encode.PROVENANCE.aac.md

# Prebuilt executable: explicit native runtime dependencies, including libraries
# GPUI loads dynamically. The glibc floor matches the native build host.
AutoReqProv:    no
Requires:       glibc >= %{janus_glibc_version}
Requires:       libgcc
Requires:       libxcb
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       libwayland-client
Requires:       fontconfig
Requires:       freetype
Requires:       vulkan-loader
Requires:       hicolor-icon-theme
# Screen/window capture and desktop audio (PipeWire via xdg-desktop-portal), Opus.
Requires:       pipewire-libs
Requires:       opus
Requires:       xdg-desktop-portal
# GPU H.264 encoding through GStreamer's VA plugin (vah264enc). Weak
# dependencies: dnf installs them when available and skips them otherwise;
# Janus then uses its built-in software encoder. Fedora's own Mesa VA driver
# has H.264 encoding disabled; RPM Fusion's freeworld driver enables it.
Requires:       gstreamer1
Requires:       gstreamer1-plugins-base
Recommends:     gstreamer1-plugins-bad-free
Recommends:     mesa-va-drivers-freeworld

%description
Janus is a standalone Rust/GPUI desktop casting application with native receiver
discovery and controls. This build provides native Google Cast and AirPlay
(including AirPlay 2 PIN pairing) screen/window mirroring with optional desktop
sound, GPU-accelerated encoding when available.

%prep

%build

%install
install -D -m 0755 "%{SOURCE0}" "%{buildroot}%{_bindir}/janus"
install -D -m 0644 "%{SOURCE1}" "%{buildroot}%{_datadir}/applications/janus.desktop"
install -D -m 0644 "%{SOURCE2}" "%{buildroot}%{_datadir}/pixmaps/janus.png"
install -D -m 0644 "%{SOURCE3}" "%{buildroot}%{_docdir}/janus/README.md"
install -D -m 0644 "%{SOURCE4}" "%{buildroot}%{_licensedir}/janus/airplay/LICENSE"
install -D -m 0644 "%{SOURCE5}" "%{buildroot}%{_licensedir}/janus/airplay/COPYING.GPL"
install -D -m 0644 "%{SOURCE6}" "%{buildroot}%{_licensedir}/janus/airplay/NOTICE.upstream.md"
install -D -m 0644 "%{SOURCE7}" "%{buildroot}%{_licensedir}/janus/airplay/PROVENANCE.md"

install -D -m 0644 "%{SOURCE8}" "%{buildroot}%{_licensedir}/janus/aac/NOTICE.FDK-AAC"
install -D -m 0644 "%{SOURCE9}" "%{buildroot}%{_licensedir}/janus/aac/PROVENANCE.aac.md"

%files
%{_bindir}/janus
%{_datadir}/applications/janus.desktop
%{_datadir}/pixmaps/janus.png
%{_docdir}/janus/
%license %{_licensedir}/janus/

%changelog
* Tue Nov 10 2026 Janus contributors - 0.1.5-4
- Receivers (Google Cast, AirPlay, Hisense/EShare) show this computer as
  janus@<hostname>, e.g. janus@fedora.
- Remove the per-receiver "Sound includes everything" caption.

* Tue Nov 10 2026 Janus contributors - 0.1.5-3
- Rounded 14 px window corners with outline and soft shadow; square when tiled.
- No maximize: Minimize and Close only; a compositor maximize is undone.
- Segmented navigation and Screen/Window; Cast is the only accent action;
  Sound and Resolution chips show their state; cleaner Settings and Help.

* Tue Nov 10 2026 Janus contributors - 0.1.5-2
- Use the Hisense board's own casting canvas (3840x2160) so 2K/4K choices apply.

* Tue Nov 10 2026 Janus contributors - 0.1.5-1
- Support Hisense/EShare interactive displays natively; remove the Settings switch.
- Sharper casts: GPU VBR with a quality floor, refinement after the picture settles,
  TCP back-pressure instead of dropped frames and forced key frames.
- Honour the board's key-frame requests and follow the network with the bitrate.

* Tue Nov 10 2026 Janus contributors - 0.1.4-3
- Add alpha EShare AAC-ELD desktop sound matching the native 480-sample format.
- Place Cast separately at the right of the receiver control row.

* Tue Nov 10 2026 Janus contributors - 0.1.4-2
- Auto now casts the shared screen's own size up to the TV's maximum at the
  highest frame rate the encoder sustains there (2K30 / 1080p60 on the GPU);
  smaller sizes are a manual choice for smoother motion.
- High-quality area-averaging downscaler: no more jagged text at 1080p.
- Playout delay sized to the chosen resolution (150–300 ms on the GPU).

* Tue Nov 10 2026 Janus contributors - 0.1.4-1
- AirPlay 2 (HomeKit) PIN pairing with an in-app pairing card, encrypted
  control and ChaCha20-Poly1305 video and sound; the TV volume is never changed.
- GPU H.264 encoding (VA-API via GStreamer) with software fallback; Auto picks
  the largest size the encoder keeps smooth (1080p60 on the GPU).
- Resolution choice (Auto, 720p, 1080p, 2K, 4K) for Google Cast and AirPlay.
- Own minimize/close buttons, window move and resize on GNOME/Wayland.
- IPv6 discovery for receivers that announce AirPlay only there.

* Mon Nov 09 2026 Janus contributors - 0.1.3-1
- Keep app navigation and Screens header fixed while receiver cards scroll.
- Move Refresh to the top-right of the Screens header.

* Mon Nov 09 2026 Janus contributors - 0.1.2-1
- Add protocol pills to distinguish AirPlay, Google Cast and Janus destinations.
- Preserve separate browse identities and receiver-owned casting controls.

* Mon Nov 09 2026 Janus contributors - 0.1.1-1
- Bump the app/core version to 0.1.1 so native package upgrades are explicit.
- Show the app version in Help and support --version without opening a window.
- Include per-receiver controls, hover feedback, logo and AirPlay diagnostics.

* Mon Nov 09 2026 Janus contributors - 0.1.0-5
- Use the supplied Janus medallion in the native interface and launcher.
- Add experimental native legacy AirPlay mirroring; physical validation pending.
- Inline receiver controls, keyboard navigation and restrained hover feedback.
- Preserve AirPlay license/provenance notices and report startup timing counts.

* Mon Nov 09 2026 Janus contributors - 0.1.0-4
- Screen and window casting to Cast TVs: portal capture, H.264, Cast Streaming.
- Optional desktop sound (Opus); live Stop; janus cast and acceptance --mirror.

* Mon Nov 09 2026 Janus contributors - 0.1.0-3
- Offline Cast device trust and live read-only receiver session in Help.
- Generic receiver acceptance: janus diagnostics check-receivers (CLI build).

* Mon Nov 09 2026 Janus contributors - 0.1.0-2
- Compact native interface and read-only receiver authentication probe.

* Mon Nov 09 2026 Janus contributors - 0.1.0-1
- Package the native Rust/GPUI desktop application.
