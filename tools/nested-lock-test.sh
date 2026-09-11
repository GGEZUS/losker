#!/usr/bin/env bash
# Protocol test for --lock inside a NESTED niri — the host desktop session is
# never touched. Runs three drills and prints PASS/FAIL per drill:
#   1. acquire   — the locker takes the lock (::locked, one surface)
#   2. conflict  — a second locker exits 1 with ::failed
#   3. sigkill   — killing the client keeps the session locked; the client's
#                  exit code observed by its wrapper is 137
# No drill unlocks anything, so a nested niri window appears on screen for
# ~15 s; it is killed by exact pgid at the end (never by name).
#
# usage: tools/nested-lock-test.sh [path-to-losker]
#        default: target/release/losker
set -u
BIN=$(realpath "${1:-target/release/losker}")
TMP=$(mktemp -d)
trap 'kill -9 -- -$NPID 2>/dev/null; pkill -9 -x losker 2>/dev/null; rm -rf "$TMP"' EXIT

cat > "$TMP/nested.kdl" <<EOF
spawn-at-startup "sh" "-c" "sleep 1.5; env > $TMP/env; $BIN --lock > $TMP/lock1.log 2>&1; echo \$? > $TMP/rc1"
EOF

echo "== spawning nested niri (window on screen for ~15 s)"
setsid env NIRI_CONFIG="$TMP/nested.kdl" niri > "$TMP/niri.log" 2>&1 &
NPID=$!
sleep 6
if [ ! -f "$TMP/env" ]; then
    echo "FAIL: nested niri never spawned its startup line"; tail -5 "$TMP/niri.log"; exit 1
fi
WL=$(grep '^WAYLAND_DISPLAY=' "$TMP/env" | cut -d= -f2)
XDGR=$(grep '^XDG_RUNTIME_DIR=' "$TMP/env" | cut -d= -f2)

echo "== drill 1: acquire"
grep -q '::locked' "$TMP/lock1.log" && echo "PASS: ::locked" || {
    echo "FAIL: no ::locked; log:"; cat "$TMP/lock1.log"; exit 1; }
grep -q 'refusing to lock' "$TMP/lock1.log" && {
    echo "NOTE: preflight refused (PAM service file missing/empty?) — fix that first"; exit 1; }

echo "== drill 2: conflict"
env XDG_RUNTIME_DIR="$XDGR" WAYLAND_DISPLAY="$WL" timeout 10 "$BIN" --lock > "$TMP/lock2.log" 2>&1
rc2=$?
[ "$rc2" -eq 1 ] && grep -q '::failed' "$TMP/lock2.log" \
    && echo "PASS: second locker rc=1 with ::failed" \
    || { echo "FAIL: rc2=$rc2"; cat "$TMP/lock2.log"; }

echo "== drill 3: sigkill"
LOCKER=$(pgrep -x losker | head -1)
kill -9 "$LOCKER"; sleep 2
rc1=$(cat "$TMP/rc1")
[ "$rc1" -eq 137 ] && echo "PASS: client died by SIGKILL, wrapper saw 137 (session stays locked)" \
    || echo "FAIL: rc1=$rc1 (expect 137)"

echo "== lock1 trace:"
cat "$TMP/lock1.log"
echo "== done (nested niri killed)"
