#!/usr/bin/env bash
# Build open-task for Linux with both toolkit plugins inside it.
#
#   scripts/build-linux.sh [--release] [--target <triple>]
#
# The plugins (GTK 4: ot-shell-gtk, Qt 6: ot-shell-qt) are built first, then
# ot-app with OT_SHELL_GTK_SO and OT_SHELL_QT_SO naming them, which its build
# script embeds. The result is one file that loads whichever toolkit the desktop
# has and still runs headless where there is neither. Prints the binary's path.
#
# A plain `cargo build` needs none of this: the binary then finds the plugins
# beside itself in target/.
set -euo pipefail

profile=debug
cargo_profile=()
target=()
target_dir_part=""
while [ $# -gt 0 ]; do
    case "$1" in
        --release) profile=release; cargo_profile=(--release) ;;
        --target) target=(--target "$2"); target_dir_part="$2/"; shift ;;
        *) echo "usage: $0 [--release] [--target <triple>]" >&2; exit 2 ;;
    esac
    shift
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
out="$target_dir/${target_dir_part}${profile}"

cargo build --locked "${cargo_profile[@]}" "${target[@]}" -p ot-shell-gtk -p ot-shell-qt >&2

# Copies under their own names, so the app's build sees a change only when a
# plugin's bytes do, and a later plugin build cannot change what was embedded.
mkdir -p "$out/embed"
cp "$out/libot_shell_gtk.so" "$out/embed/libot_shell_gtk.so"
cp "$out/libot_shell_qt.so" "$out/embed/libot_shell_qt.so"
OT_SHELL_GTK_SO="$out/embed/libot_shell_gtk.so" \
OT_SHELL_QT_SO="$out/embed/libot_shell_qt.so" \
    cargo build --locked "${cargo_profile[@]}" "${target[@]}" -p ot-app >&2

echo "$out/open-task"
