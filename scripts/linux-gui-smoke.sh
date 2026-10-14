#!/usr/bin/env bash
# Check a Linux open-task binary the way a user meets it:
#
#   scripts/linux-gui-smoke.sh <open-task binary> [<screenshot dir>]
#
# 1. It links no GUI toolkit (nothing GTK, Qt or GLib among its NEEDED
#    libraries), so a server without them can still run it.
# 2. Without a display it runs headless and prints passes.
# 3. With a display (a private Xvfb), each toolkit opens the window, the window
#    takes a click (the Tree tab) and is still running after it, and a
#    screenshot of each is left in the screenshot directory.
#
# Needs Xvfb, xdotool and ImageMagick (scripts/linux-deps.sh --test).
set -euo pipefail

shots="${2:-.}"
mkdir -p "$shots"
fail() { echo "FAIL: $*" >&2; exit 1; }

# Run a copy alone in an empty directory: a build directory has the plugins
# beside the binary, where it would find them, and the point is that the
# binary carries its own.
alone="$(mktemp -d)"
cp "$1" "$alone/open-task"
bin="$alone/open-task"

echo "== no toolkit among the binary's libraries"
needed="$(readelf -d "$bin" | grep NEEDED || true)"
echo "$needed"
if echo "$needed" | grep -Eqi 'gtk|qt|glib|gobject|gdk'; then
    fail "the binary links a toolkit library"
fi

echo "== headless without a display"
out="$(env -u DISPLAY -u WAYLAND_DISPLAY "$bin" --passes 1 2>/dev/null)" || fail "headless run failed"
echo "$out" | head -3
echo "$out" | grep -q '^tick ' || fail "no pass printed"

display=":${SMOKE_DISPLAY:-91}"
Xvfb "$display" -screen 0 1280x860x24 -nolisten tcp > /dev/null 2>&1 &
xvfb=$!
trap 'kill $xvfb 2> /dev/null || true; rm -rf "$alone"' EXIT
for _ in $(seq 1 50); do xdpyinfo -display "$display" > /dev/null 2>&1 && break; sleep 0.1; done

for toolkit in gtk qt; do
    echo "== $toolkit window"
    log="$shots/$toolkit.log"
    DISPLAY="$display" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb \
        "$bin" --toolkit "$toolkit" > "$log" 2>&1 &
    app=$!
    window=""
    for _ in $(seq 1 100); do
        window="$(DISPLAY="$display" xdotool search --name '^open-task' 2> /dev/null | head -1 || true)"
        [ -n "$window" ] && break
        kill -0 "$app" 2> /dev/null || break
        sleep 0.2
    done
    if [ -z "$window" ]; then
        cat "$log"
        fail "$toolkit: no window"
    fi
    name="$(echo "$toolkit" | sed 's/gtk/GTK/; s/qt/Qt/')"
    # The log is coloured; match it without the escapes.
    sed 's/\x1b\[[0-9;]*m//g' "$log" | grep -q "toolkit plugin loaded toolkit=$name" ||
        { cat "$log"; fail "$toolkit: another toolkit loaded"; }
    DISPLAY="$display" xdotool windowmove "$window" 0 0
    sleep 3
    # The Tree tab, then a moment for the view to change.
    DISPLAY="$display" xdotool mousemove 282 161 click 1
    sleep 2
    kill -0 "$app" 2> /dev/null || { cat "$log"; fail "$toolkit: exited after a click"; }
    shot="$shots/$toolkit.png"
    import -display "$display" -window root "$shot"
    # A blank frame is a failure to draw, not a window.
    spread="$(convert "$shot" -crop 1180x760+0+0 -colorspace Gray -format '%[fx:standard_deviation]' info:)"
    echo "screenshot $shot (grey spread $spread)"
    awk -v s="$spread" 'BEGIN { exit !(s > 0.02) }' || fail "$toolkit: the window is blank"
    kill "$app"
    wait "$app" 2> /dev/null || true
done
echo "OK"
