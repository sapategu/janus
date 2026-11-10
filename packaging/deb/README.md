# Debian / Ubuntu package

Janus ships as a `.deb` for Ubuntu 24.04 LTS and its flavours (Kubuntu,
Xubuntu, …) and later releases, on x86_64 (`amd64`).

## Install (teammates)

```sh
sudo apt install ./janus_0.1.5-1_amd64.deb
```

Use `apt` with the `./` path, not `dpkg -i`. apt then installs the runtime
dependencies too. Open **Janus** from the applications menu.

- **Screen and window picker:** the desktop's portal shows it.
  `xdg-desktop-portal-gnome` (Ubuntu) or `xdg-desktop-portal-kde` (Kubuntu)
  is recommended and normally already installed.
- **GPU encoding:** Janus encodes on the GPU through GStreamer's VA plugin
  (`gstreamer1.0-plugins-bad`, recommended). It needs a VA driver that can
  encode H.264: `mesa-va-drivers` for AMD, `intel-media-va-driver` for Intel.
  Without one, Janus encodes on the CPU. You can check with
  `vainfo | grep EncSlice`.
- **Sound:** sound comes from PipeWire, which is the default on Ubuntu and
  Kubuntu 24.04.

To remove Janus: `sudo apt remove janus`.

## Build

On Ubuntu/Debian:

```sh
./scripts/build.sh deb
```

From any other Linux machine, such as Fedora, with Podman or Docker:

```sh
./scripts/build.sh deb --container                # ubuntu:24.04
./scripts/build.sh deb --container ubuntu:25.10   # another release
```

The package lands in `target/deb/`. A native build first installs missing OS
packages with one sudo prompt (`./scripts/build.sh deps`).

The container build:
- mounts the sources read-only,
- keeps Rust, crates and build output in a per-image volume
  (`janus-deb-ubuntu-24.04`) so later builds are incremental,
- writes only the `.deb`, owned by you.

Library dependencies come from `dpkg-shlibdeps`, so a package works on the
release it was built on and on newer ones. Build on the oldest release you want
to support. A `.deb` built directly on Fedora would require Fedora's newer glibc
and would not start on Ubuntu; that is why non-Debian hosts must use
`--container`.

## Releases

The Debian revision is the `-1` in `Version: @VERSION@-1` in
[control](control). The upstream version comes from
`crates/janus-app/Cargo.toml`.
- When you rebuild the same version with packaging changes, bump the revision
  (`-2`, …).
- When the app version changes, reset it to `-1`.
- Every package version needs a matching top entry in [changelog](changelog);
  the build refuses to package without one.

[copyright](copyright) summarises the bundled third-party terms; the full
notices are installed under `/usr/share/doc/janus/`. The package is unsigned
and meant for local/team use. Public distribution still needs the release gates
in `docs/todo.md` (D03), including LGPL source/relink material for the AirPlay
port.
