#!/usr/bin/env bash
# Install what building open-task's Linux toolkit plugins needs: a C++ compiler,
# pkg-config, and the GTK 4 and Qt 6 development files. With --test, also what
# scripts/linux-gui-smoke.sh needs to open the window on a virtual display.
#
# Debian and Ubuntu (apt). Runs as root (a CI container) or through sudo. On
# Ubuntu 22.04 this gets GTK 4.6 and Qt 6.2, the oldest versions the plugins
# support, which is why releases are built there.
set -euo pipefail

test_deps=0
for a in "$@"; do
    case "$a" in
        --test) test_deps=1 ;;
        *) echo "usage: $0 [--test]" >&2; exit 2 ;;
    esac
done

sudo=""
if [ "$(id -u)" != 0 ]; then sudo="sudo"; fi

packages=(
    build-essential pkg-config git curl ca-certificates
    # appimagetool (scripts/build-appimage.sh) needs `file`.
    file
    libgtk-4-dev qt6-base-dev qt6-base-dev-tools
)
if [ "$test_deps" = 1 ]; then
    packages+=(
        xvfb xdotool x11-utils imagemagick
        # Icons and a font, as a desktop would have them; Qt reads SVG icons
        # only through its SVG plugin.
        adwaita-icon-theme fonts-dejavu-core libqt6svg6
    )
fi

export DEBIAN_FRONTEND=noninteractive
$sudo apt-get update -qq
$sudo apt-get install -y -qq --no-install-recommends "${packages[@]}" > /dev/null
echo "GTK $(pkg-config --modversion gtk4)"
# Ubuntu 22.04's Qt has no pkg-config files; its qmake knows the version.
qmake="$(command -v qmake6 || echo /usr/lib/qt6/bin/qmake)"
echo "Qt $("$qmake" -query QT_VERSION)"
