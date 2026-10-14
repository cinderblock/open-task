#!/usr/bin/env bash
# Wrap a Linux open-task binary (from scripts/build-linux.sh) as an AppImage:
#
#   scripts/build-appimage.sh <open-task binary> <x86_64|aarch64> <output .AppImage>
#
# The binary is already the whole program, both toolkit plugins inside it; the
# AppImage adds the desktop entry and icon, so app launchers and AppImage
# integration tools (Gear Lever, AppImageLauncher) list it as an app. Like the
# bare binary it uses the GTK or Qt the system has; it bundles no toolkit.
#
# Downloads appimagetool, and runs it without FUSE (APPIMAGE_EXTRACT_AND_RUN),
# so it works in a container.
set -euo pipefail

bin="$1"
arch="$2"
out="$3"
case "$arch" in x86_64 | aarch64) ;; *) echo "arch must be x86_64 or aarch64" >&2; exit 2 ;; esac

root="$(cd "$(dirname "$0")/.." && pwd)"
id="io.github.cinderblock.open-task"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/open-task.AppDir"

install -Dm755 "$bin" "$app/usr/bin/open-task"
install -Dm644 "$root/assets/linux/$id.desktop" "$app/usr/share/applications/$id.desktop"
install -Dm644 "$root/assets/logo/open-task-512.png" "$app/usr/share/icons/hicolor/512x512/apps/$id.png"
install -Dm644 "$root/assets/logo/open-task.svg" "$app/usr/share/icons/hicolor/scalable/apps/$id.svg"
# appimagetool wants the entry and icon at the top as well.
cp "$app/usr/share/applications/$id.desktop" "$app/"
cp "$app/usr/share/icons/hicolor/512x512/apps/$id.png" "$app/$id.png"
ln -s "$id.png" "$app/.DirIcon"
cat > "$app/AppRun" << 'EOF'
#!/bin/sh
# The AppImage runtime mounts the image and runs this; the binary needs nothing
# else from it.
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/open-task" "$@"
EOF
chmod 755 "$app/AppRun"

tool="$work/appimagetool"
curl -fsSL -o "$tool" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
chmod 755 "$tool"
mkdir -p "$(dirname "$out")"
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$app" "$out" >&2
chmod 755 "$out"
echo "$out"
