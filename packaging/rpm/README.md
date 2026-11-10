# Native Fedora RPM

Build from the locked Rust/GPUI desktop workspace:

```sh
./scripts/build.sh rpm
```

Dependencies: Rust 1.96.0 (rustup, from `rust-toolchain.toml`) plus OS packages.
`./scripts/build.sh rpm` first installs every missing OS package (build,
desktop, media capture, packaging) in one transaction with a single sudo
prompt; `./scripts/build.sh deps` does only that step. Building
itself needs no root permission.

The helper compiles the release executable into `target/desktop/release/janus`,
stages a stripped copy and desktop resources under `target/rpm`, and invokes
native `rpmbuild`. The resulting package is below `target/rpm/RPMS/<arch>/`.
All build output stays in `target`; Janus itself is never installed automatically.

For this Fedora 44 x86_64 build:

```sh
sudo dnf install ./target/rpm/RPMS/x86_64/janus-0.1.5-4.fc44.x86_64.rpm
```

Then open **Janus** from the applications menu or run `janus` in a desktop
terminal. The installed executable is the native GPUI app, not the diagnostics
CLI. It includes the launcher, supplied PNG icon and project README. The same
`docs/janus-logo.png` is embedded in the app header and installed in the
freedesktop pixmaps directory. `janus.desktop` points directly to the installed
`/usr/share/pixmaps/janus.png`. Use the desktop
session to launch it; packaging cannot give a sandbox compositor/GPU access.

The package declares native runtime dependencies, including dynamically loaded
Wayland/font/Vulkan libraries and GStreamer for video encoding. Alpha EShare
sound uses the bundled AAC-ELD codec; no libav audio plugin is required. GPU video encoding is optional:
the package *recommends* GStreamer's VA plugin and RPM Fusion's
`mesa-va-drivers-freeworld` (Fedora's own Mesa driver ships with H.264 encoding
disabled). `dnf` installs recommended packages that are available; when RPM
Fusion's free repository isn't enabled, installation still succeeds and Janus
encodes on the CPU. To enable GPU encoding later:

```sh
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install mesa-va-drivers-freeworld
```
 Its glibc floor matches the native build host;
this is not a universal binary for older RPM distributions. A working graphics
driver and desktop session are still required.

This local package is unsigned. Public signing/distribution, installation and
upgrade qualification, license/notice approval and complete live casting remain
separate open release gates. The project has no selected license; RPM metadata
uses `LicenseRef-Unspecified` alongside the AirPlay LGPL terms and grants no new
rights to the rest of the project.

Release 5 also links the LGPL AirPlay port. Before distributing binaries, provide
the corresponding source and relink material described in
[the port provenance](../../crates/janus-airplay/PROVENANCE.md). This change was
verified as debug and optimized native builds and local RPMs. Current 0.1.4-2
adds AirPlay 2 pairing/sound, GPU encoding with software fallback, the
resolution choice (Auto full quality) and window controls. Package installation and
upgrade behavior require the user's desktop; no package is installed automatically.


Release 2 upgrades the previous RPM. It opens at 840×520 with compact navigation
and adds Help → Check connection for the selected TV. This read-only probe sends
no screen/audio or receiver controls; a reply remains unverified.
