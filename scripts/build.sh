#!/usr/bin/env bash
# Janus native build entry point. One script, one function per job:
#
#   ./scripts/build.sh deps [--check] [--yes] [--ci]   install missing OS packages
#   ./scripts/build.sh desktop [--run]                 build (and run) the GPUI app
#   ./scripts/build.sh rpm                             build the native Fedora RPM
#   ./scripts/build.sh deb [--container [IMAGE]]       build the Debian/Ubuntu .deb
#   ./scripts/build.sh version X.Y.Z                    stamp a release version (CI)
#
# `desktop`, `rpm` and `deb` call install_deps first: anything missing is installed in
# ONE package-manager transaction (a single sudo prompt); when everything is
# present it does nothing. The Rust toolchain comes from rust-toolchain.toml via
# rustup. Build output stays under target/; Janus is never installed for you.
set -euo pipefail

janus_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
janus_target="$janus_root/target/desktop"

usage() {
    printf 'Usage:\n' >&2
    sed -n '4,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

# --- dependencies -----------------------------------------------------------

# Sets janus_family and the package groups for this distribution. Fedora and
# Debian names are the single source of truth (native CI uses them too).
dependency_lists() {
    # shellcheck disable=SC1091
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *" fedora "*|*" rhel "*|*" centos "*) janus_family=fedora ;;
        *" debian "*|*" ubuntu "*) janus_family=debian ;;
        *) printf 'Unsupported Linux distribution: %s. Install the equivalents of the Fedora lists in %s.\n' \
            "${PRETTY_NAME:-unknown}" "${BASH_SOURCE[0]}" >&2; exit 2 ;;
    esac
    if [[ $janus_family == fedora ]]; then
        # Compiler, linker and bindgen's libclang (PipeWire bindings).
        deps_build=(gcc gcc-c++ clang clang-devel cmake pkgconf-pkg-config git)
        # GPUI: fonts, X11/XCB, xkbcommon incl. the X11 dev link, Wayland, Vulkan.
        deps_desktop=(openssl-devel libzstd-devel fontconfig-devel freetype-devel libxcb-devel
            libX11-devel libxkbcommon-devel libxkbcommon-x11-devel wayland-devel vulkan-loader-devel)
        # Screen/window capture (portal + PipeWire), VA-API encode, Opus, ALSA,
        # nasm for OpenH264's x86 assembly (otherwise a much slower C build),
        # GStreamer for GPU H.264 encoding (vah264enc lives in plugins-bad).
        deps_media=(pipewire-devel libva-devel opus-devel alsa-lib-devel nasm
            gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-bad-free)
        deps_package=(rpm-build desktop-file-utils binutils)
        deps_ci=(curl mesa-vulkan-drivers xorg-x11-server-Xvfb xorg-x11-xauth)
    else
        deps_build=(build-essential clang libclang-dev cmake pkg-config git)
        deps_desktop=(libssl-dev libzstd-dev libfontconfig1-dev libfreetype-dev libxcb1-dev
            libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev)
        deps_media=(libpipewire-0.3-dev libva-dev libopus-dev libasound2-dev nasm
            libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-bad)
        deps_package=(dpkg-dev desktop-file-utils binutils)
        # gstreamer1.0-fdkaac (multiverse): the independent AAC-ELD decoder the
        # AAC tests use; Fedora's gstreamer1-plugins-bad-free already has it.
        deps_ci=(curl mesa-vulkan-drivers xvfb xauth gstreamer1.0-fdkaac)
    fi
}

package_installed() {
    if [[ $janus_family == fedora ]]; then
        rpm -q --quiet "$1" || rpm -q --quiet --whatprovides "$1"
    else
        dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'
    fi
}

# install_deps [--check] [--yes] [--ci]
# Installs every missing build, desktop, media and packaging package at once.
install_deps() {
    local check_only=0 assume_yes=0 ci=0 argument
    for argument in "$@"; do
        case "$argument" in
            --check) check_only=1 ;;
            --yes) assume_yes=1 ;;
            --ci) ci=1 ;;
            *) usage ;;
        esac
    done
    if [[ "$(uname -s)" != Linux ]]; then
        printf 'No OS packages needed on %s; use the platform SDK.\n' "$(uname -s)"
        return 0
    fi
    dependency_lists
    local wanted=("${deps_build[@]}" "${deps_desktop[@]}" "${deps_media[@]}" "${deps_package[@]}")
    [[ $ci == 1 ]] && wanted+=("${deps_ci[@]}")

    local missing=() package_name
    for package_name in "${wanted[@]}"; do
        package_installed "$package_name" || missing+=("$package_name")
    done
    if [[ ${#missing[@]} == 0 ]]; then
        printf 'All Janus dependencies are installed (%s).\n' "${PRETTY_NAME:-$janus_family}"
        return 0
    fi
    printf 'Missing Janus dependencies (%d): %s\n' "${#missing[@]}" "${missing[*]}"
    if [[ $check_only == 1 ]]; then
        printf 'Run %s deps to install them with one sudo prompt.\n' "${BASH_SOURCE[0]}"
        return 1
    fi

    local run_root=() yes_flag=()
    if [[ $(id -u) != 0 ]]; then
        command -v sudo >/dev/null || { printf 'sudo is required to install packages.\n' >&2; exit 1; }
        run_root=(sudo)
    fi
    [[ $assume_yes == 1 ]] && yes_flag=(-y)
    if [[ $janus_family == fedora ]]; then
        "${run_root[@]}" dnf install "${yes_flag[@]}" "${missing[@]}"
    else
        # One root shell so the index refresh and install share a single prompt.
        "${run_root[@]}" env DEBIAN_FRONTEND=noninteractive sh -c \
            'apt-get update && apt-get install "$@"' sh "${yes_flag[@]}" "${missing[@]}"
    fi

    local still_missing=()
    for package_name in "${missing[@]}"; do
        package_installed "$package_name" || still_missing+=("$package_name")
    done
    if [[ ${#still_missing[@]} != 0 ]]; then
        printf 'Still missing after install: %s\n' "${still_missing[*]}" >&2
        exit 1
    fi
    printf 'Installed %d Janus dependencies.\n' "${#missing[@]}"
}

# Quiet when complete; otherwise installs (one prompt) before building.
ensure_deps() {
    install_deps --check >/dev/null || install_deps
}

# --- desktop ----------------------------------------------------------------

# build_desktop [--run]: the actual GPUI application; never the diagnostic CLI.
build_desktop() {
    local run=0
    case "${1:-}" in
        "") ;;
        --run) run=1 ;;
        *) usage ;;
    esac
    [[ $# -le 1 ]] || usage
    ensure_deps
    cd -- "$janus_root"
    cargo build --locked --manifest-path "$janus_root/crates/janus-app/Cargo.toml" \
        --target-dir "$janus_target"
    printf 'Native GPUI executable: %s\n' "$janus_target/debug/janus"
    if [[ $run == 1 ]]; then
        exec "$janus_target/debug/janus"
    fi
}

# --- rpm --------------------------------------------------------------------

# build_rpm: locked release build, stripped and packaged with native rpmbuild.
build_rpm() {
    [[ $# == 0 ]] || usage
    ensure_deps
    local tool
    for tool in cargo rpmbuild rpmspec rpm python3 strip desktop-file-validate getconf; do
        command -v "$tool" >/dev/null || {
            printf 'Missing build tool: %s. Run %s deps.\n' "$tool" "${BASH_SOURCE[0]}" >&2
            exit 1
        }
    done
    local rpm_root="$janus_root/target/rpm" version glibc filename package
    version="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' \
        "$janus_root/crates/janus-app/Cargo.toml")"
    glibc="$(getconf GNU_LIBC_VERSION)"
    glibc="${glibc#glibc }"

    cd -- "$janus_root"
    desktop-file-validate packaging/rpm/janus.desktop
    cargo build --release --locked --manifest-path crates/janus-app/Cargo.toml \
        --target-dir "$janus_target"

    mkdir -p "$rpm_root"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS,tmp}
    install -m 0755 "$janus_target/release/janus" "$rpm_root/SOURCES/janus"
    strip --strip-unneeded "$rpm_root/SOURCES/janus"
    install -m 0644 packaging/rpm/janus.desktop README.md "$rpm_root/SOURCES/"
    install -m 0644 docs/janus-logo.png "$rpm_root/SOURCES/janus.png"
    local airplay_notice
    for airplay_notice in LICENSE COPYING.GPL NOTICE.upstream.md PROVENANCE.md; do
        install -m 0644 "crates/janus-airplay/$airplay_notice" \
            "$rpm_root/SOURCES/janus-airplay.$airplay_notice"
    done

    local aac_notice
    for aac_notice in NOTICE.FDK-AAC PROVENANCE.aac.md; do
        install -m 0644 "crates/janus-encode/$aac_notice" \
            "$rpm_root/SOURCES/janus-encode.$aac_notice"
    done

    rpmbuild -bb packaging/rpm/janus.spec \
        --define "_topdir $rpm_root" \
        --define "_tmppath $rpm_root/tmp" \
        --define "janus_version $version" \
        --define "janus_glibc_version $glibc"

    filename="$(rpmspec -q --qf '%{ARCH}/%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}.rpm' \
        --define "janus_version $version" \
        --define "janus_glibc_version $glibc" packaging/rpm/janus.spec)"
    package="$rpm_root/RPMS/$filename"
    rpm -K "$package"
    printf 'Native RPM: %s\nInstall or upgrade: sudo dnf install %s\n' "$package" "$package"
}

# --- deb --------------------------------------------------------------------

# build_deb [--container [IMAGE]]: locked release build packaged with dpkg-deb.
# Library dependencies come from dpkg-shlibdeps, so the package targets the
# release it is built on and newer ones (Ubuntu 24.04 also covers Kubuntu 24.04
# and later). A .deb made on Fedora would carry Fedora's newer glibc, so on a
# non-Debian host use --container: it builds inside IMAGE (default
# ubuntu:24.04) with Podman or Docker. Output: target/deb/.
build_deb() {
    if [[ ${1:-} == --container ]]; then
        shift
        [[ $# -le 1 ]] || usage
        build_deb_container "${1:-ubuntu:24.04}"
        return
    fi
    [[ $# == 0 ]] || usage
    dependency_lists
    if [[ $janus_family != debian ]]; then
        printf 'A .deb must be built on Debian/Ubuntu. On %s run: %s deb --container\n' \
            "${PRETTY_NAME:-this system}" "${BASH_SOURCE[0]}" >&2
        exit 2
    fi
    ensure_deps
    local tool
    for tool in cargo dpkg dpkg-deb dpkg-shlibdeps strip desktop-file-validate; do
        command -v "$tool" >/dev/null || {
            printf 'Missing build tool: %s. Run %s deps.\n' "$tool" "${BASH_SOURCE[0]}" >&2
            exit 1
        }
    done
    # Container builds keep sources read-only and write elsewhere.
    local out="${JANUS_DEB_OUT:-$janus_root/target/deb}"
    local target="${JANUS_DEB_TARGET:-$janus_target}"
    local version arch revision name stage work shlibs size control
    version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$janus_root/crates/janus-app/Cargo.toml" | head -n 1)"
    revision="$(sed -n 's/^Version: @VERSION@-//p' "$janus_root/packaging/deb/control")"
    arch="$(dpkg --print-architecture)"
    [[ -n $version && -n $revision ]] || { printf 'Cannot read the package version.\n' >&2; exit 1; }
    if [[ "$(sed -n '1s/^janus (\(.*\)).*/\1/p' "$janus_root/packaging/deb/changelog")" != "$version-$revision" ]]; then
        printf 'Add a packaging/deb/changelog entry for %s-%s first.\n' "$version" "$revision" >&2
        exit 1
    fi

    cd -- "$janus_root"
    desktop-file-validate packaging/rpm/janus.desktop
    cargo build --release --locked --manifest-path crates/janus-app/Cargo.toml \
        --target-dir "$target"

    name="janus_${version}-${revision}_${arch}"
    stage="$out/$name"
    rm -rf -- "$stage"
    mkdir -p "$stage/DEBIAN"
    install -D -m 0755 "$target/release/janus" "$stage/usr/bin/janus"
    strip --strip-unneeded "$stage/usr/bin/janus"
    install -D -m 0644 packaging/rpm/janus.desktop "$stage/usr/share/applications/janus.desktop"
    install -D -m 0644 docs/janus-logo.png "$stage/usr/share/pixmaps/janus.png"
    install -D -m 0644 README.md "$stage/usr/share/doc/janus/README.md"
    install -D -m 0644 packaging/deb/copyright "$stage/usr/share/doc/janus/copyright"
    gzip -9nc packaging/deb/changelog >"$stage/usr/share/doc/janus/changelog.Debian.gz"
    chmod 0644 "$stage/usr/share/doc/janus/changelog.Debian.gz"
    local notice
    for notice in LICENSE COPYING.GPL NOTICE.upstream.md PROVENANCE.md; do
        install -D -m 0644 "crates/janus-airplay/$notice" "$stage/usr/share/doc/janus/airplay/$notice"
    done
    for notice in NOTICE.FDK-AAC PROVENANCE.aac.md; do
        install -D -m 0644 "crates/janus-encode/$notice" "$stage/usr/share/doc/janus/aac/$notice"
    done

    # dpkg-shlibdeps wants a debian/control beside it; a throwaway one suffices.
    work="$(mktemp -d)"
    mkdir "$work/debian"
    printf 'Source: janus\n\nPackage: janus\nArchitecture: any\n' >"$work/debian/control"
    shlibs="$(cd "$work" && dpkg-shlibdeps -O "$stage/usr/bin/janus")"
    rm -rf -- "$work"
    shlibs="${shlibs#shlibs:Depends=}"
    [[ -n $shlibs ]] || { printf 'dpkg-shlibdeps found no dependencies.\n' >&2; exit 1; }
    size="$(du -sk --exclude=DEBIAN "$stage" | cut -f1)"
    control="$(<packaging/deb/control)"
    control="${control//@VERSION@/$version}"
    control="${control//@ARCH@/$arch}"
    control="${control//@SIZE@/$size}"
    control="${control//@SHLIBDEPS@/$shlibs}"
    printf '%s\n' "$control" >"$stage/DEBIAN/control"
    find "$stage" -type d -exec chmod 0755 {} +

    dpkg-deb --root-owner-group --build "$stage" "$out/$name.deb"
    rm -rf -- "$stage"
    dpkg-deb --field "$out/$name.deb" Package Version Architecture Depends
    printf 'Debian package: %s\nInstall or upgrade: sudo apt install ./%s.deb\n' "$out/$name.deb" "$name"
}

# build_deb_container IMAGE: the same job inside a Debian/Ubuntu container.
# Sources are mounted read-only; Rust, crates and build output are cached in a
# per-image volume; only the .deb is written to target/deb/.
build_deb_container() {
    local image="$1" engine="" candidate
    for candidate in podman docker; do
        command -v "$candidate" >/dev/null && { engine="$candidate"; break; }
    done
    [[ -n $engine ]] || {
        printf 'deb --container needs Podman or Docker.\n' >&2
        exit 1
    }
    local out="$janus_root/target/deb" cache owner=""
    cache="janus-deb-${image//[^A-Za-z0-9_.-]/-}"
    mkdir -p "$out"
    # Rootful Docker writes as root; hand the output back to the caller.
    # Rootless Podman's root already is the caller.
    [[ $engine == docker ]] && owner="$(id -u):$(id -g)"
    "$engine" run --rm \
        -v "$janus_root:/src:ro" -v "$out:/out" -v "$cache:/cache" \
        -e JANUS_DEB_OUT=/out -e JANUS_DEB_TARGET=/cache/target \
        -e CARGO_HOME=/cache/cargo -e RUSTUP_HOME=/cache/rustup \
        -e JANUS_OWNER="$owner" -w /src "$image" bash -euo pipefail -c '
            trap "[[ -z \$JANUS_OWNER ]] || chown -R \"\$JANUS_OWNER\" /out" EXIT
            export DEBIAN_FRONTEND=noninteractive PATH="/cache/cargo/bin:$PATH"
            apt-get update -qq
            apt-get install -y -qq --no-install-recommends ca-certificates curl >/dev/null
            command -v rustup >/dev/null || curl -fsSL https://sh.rustup.rs |
                sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
            rustup toolchain install   # the version pinned in rust-toolchain.toml
            ./scripts/build.sh deps --yes
            ./scripts/build.sh deb'
}

# --- version ----------------------------------------------------------------

# build_version X.Y.Z: stamps a release version into this checkout so --locked
# builds and both packages carry it: the core workspace and desktop manifests,
# the janus-* entries of both lockfiles, RPM Release 1 and Debian revision 1
# with a matching changelog entry each. Used by the release workflow; nothing
# is committed (the release tag records the version).
build_version() {
    [[ $# == 1 && $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
    local version="$1" lock spec="$janus_root/packaging/rpm/janus.spec"
    local deb_changelog="$janus_root/packaging/deb/changelog" tmp
    cd -- "$janus_root"
    # [workspace.package] and the desktop package each hold the first version
    # line. awk, not GNU-only sed, so macOS and Windows (Git Bash) runners work.
    local manifest
    for manifest in Cargo.toml crates/janus-app/Cargo.toml; do
        awk -v v="$version" '
            !done && /^version = "[^"]*"$/ { print "version = \"" v "\""; done = 1; next }
            { print }' "$manifest" >"$manifest.tmp" && mv "$manifest.tmp" "$manifest"
    done
    for lock in Cargo.lock crates/janus-app/Cargo.lock; do
        awk -v v="$version" '
            /^name = "janus-[a-z-]+"$/ { janus = 1; print; next }
            janus && /^version = / { print "version = \"" v "\""; janus = 0; next }
            { janus = 0; print }' "$lock" >"$lock.tmp" && mv "$lock.tmp" "$lock"
    done

    sed -i.bak 's/^Release:\( *\)[0-9][0-9]*%{?dist}$/Release:\11%{?dist}/' "$spec"
    sed -i.bak 's/^Version: @VERSION@-[0-9][0-9]*$/Version: @VERSION@-1/' packaging/deb/control
    rm -f "$spec.bak" packaging/deb/control.bak
    if ! grep -q " - $version-1\$" "$spec"; then
        tmp="$(mktemp)"
        awk -v v="$version" -v day="$(LC_ALL=C date -u '+%a %b %d %Y')" '
            { print }
            /^%changelog$/ {
                print "* " day " Janus contributors - " v "-1"
                print "- Release v" v "; see the release notes."
                print ""
            }' "$spec" >"$tmp" && mv "$tmp" "$spec"
    fi
    if [[ "$(sed -n '1s/^janus (\(.*\)).*/\1/p' "$deb_changelog")" != "$version-1" ]]; then
        tmp="$(mktemp)"
        {
            printf 'janus (%s-1) noble; urgency=medium\n\n' "$version"
            printf '  * Release v%s; see the release notes.\n\n' "$version"
            printf ' -- Janus contributors <janus@localhost.invalid>  %s\n\n' "$(LC_ALL=C date -u -R)"
            cat "$deb_changelog"
        } >"$tmp" && mv "$tmp" "$deb_changelog"
    fi
    printf 'Stamped version %s (RPM %s-1, Debian %s-1).\n' "$version" "$version" "$version"
}

# --- dispatch ---------------------------------------------------------------

command="${1:-}"
[[ $# -gt 0 ]] && shift
case "$command" in
    deps) install_deps "$@" ;;
    desktop) build_desktop "$@" ;;
    rpm) build_rpm "$@" ;;
    deb) build_deb "$@" ;;
    version) build_version "$@" ;;
    *) usage ;;
esac
