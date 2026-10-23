#!/usr/bin/env bash
# issue 1470: prove the A/V-sync dock layout seed against the REAL Qt (run by linux-genlock.yml's strih job).
set -euo pipefail
#
# scripts/obs_dock_state.py edits a binary Qt `QMainWindow::saveState()` blob. A blob Qt cannot read
# makes OBS reset the operator's WHOLE dock layout (OBSBasic.cpp: restoreState fails ->
# on_resetDocks_triggered), so the pytest's pure parser checks are not enough: this runner compiles
# tests/qt/dock_state_restore_1470.cpp against Qt6 Widgets, seeds the two live user.ini fixtures
# (tests/python/fixtures/obs_dock_state_1470/: the DockState lines read off stream-snv and stream-pp on
# 23.10.2026) through the real CLI, restores each with Qt and checks where the dock lands:
#   1. stream-pp as captured  -> the dock hidden + floating (the issue: OBS's add-time default);
#   2. stream-pp seeded       -> `seeded`, the dock visible + docked right, every other dock unchanged;
#   3. stream-snv as captured -> `present` (its ported layout already holds the dock), the layout
#                                unchanged + the first-seed marker added, the dock visible + docked right;
#   4. the state an OBS saves after running case 1 (Qt's own saveState) -> `default-hidden`, seed =
#      `redocked`, the dock visible + docked right, every other dock unchanged;
#   5. a probe dock seeded into stream-snv, whose right area already exists -> the probe joins that
#      area visible + docked, the A/V-sync dock and every other dock stay where they were.
# Needs g++, pkg-config + Qt6 Widgets dev files and python3; a missing tool FAILS (never a skip).
#
#   bash tests/qt/run_dock_state_restore_1470.sh

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../.." && pwd)"
SEED="${ROOT}/scripts/obs_dock_state.py"
FIX="${ROOT}/tests/python/fixtures/obs_dock_state_1470"
DOCK="net.nagater.obs-audio-video-sync-dock..main"
PROBE="camera-box.probe.dock"   # case 5: a plugin dock the layouts never held

for tool in g++ pkg-config python3; do
  command -v "$tool" >/dev/null 2>&1 || { echo "FAIL: ${tool} is not installed (issue 1470 Qt proof)" >&2; exit 1; }
done
pkg-config --exists Qt6Widgets || { echo "FAIL: the Qt6 Widgets dev files are not installed (qt6-base-dev)" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
read -r -a QT_FLAGS <<<"$(pkg-config --cflags --libs Qt6Widgets)"
g++ -std=c++17 -fPIC -Wall -Wextra -Werror -o "${WORK}/dsr" "${HERE}/dock_state_restore_1470.cpp" "${QT_FLAGS[@]}"

FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# names INI [PLUGIN] -> every dock object name in the INI's DockState, PLUGIN (default the A/V-sync
# dock) first (the harness adds the first name the way obs_frontend_add_dock_by_id adds a plugin dock).
names() {
  python3 -I - "$SEED" "$1" "${2:-$DOCK}" <<'PY'
import base64, importlib.util, sys
spec = importlib.util.spec_from_file_location("obs_dock_state", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
line = [l for l in open(sys.argv[2], encoding="utf-8") if l.startswith("DockState=")][0]
blob = base64.b64decode(line.split("=", 1)[1].strip())
others = [i["name"] for i in m.parse(blob)["items"] if i["name"] != sys.argv[3]]
print("\n".join([sys.argv[3]] + others))
PY
}

# restore INI [PLUGIN] -> the harness output for INI's DockState (restore=, one line per dock, state=).
restore() {
  local ini="$1" plugin="${2:-$DOCK}" state
  local -a docks
  state="$(sed -n 's/^DockState=//p' "$ini")"
  mapfile -t docks < <(names "$ini" "$plugin")
  # a refused restore prints restore=failed and exits 1: report it through expect, never abort here
  QT_QPA_PLATFORM=offscreen "${WORK}/dsr" "$state" "${docks[@]}" 2>/dev/null || true
}

# expect OUTPUT LINE WHAT -> the harness OUTPUT must carry LINE exactly.
expect() {
  if grep -qxF -- "$2" <<<"$1"; then echo "ok:   $3"; else fail "$3 -- want '$2', got: $(grep -F "$DOCK" <<<"$1" || echo none)"; fi
}

# others OUTPUT -> the harness lines of every dock but the A/V-sync dock and the probe.
others() { grep -v -e "^${DOCK} " -e "^${PROBE} " -e '^state=' <<<"$1" || true; }

# same_others BEFORE AFTER WHAT -> every dock but the plugin dock placed identically.
same_others() {
  if [ "$(others "$1")" = "$(others "$2")" ]; then echo "ok:   $3"
  else fail "$3 -- $(diff <(others "$1") <(others "$2") || true)"; fi
}

# seed_cli ACTION INI -> the CLI token (empty when the CLI failed: the check then names it).
seed_cli() { python3 "$SEED" "$1" --ini "$2" --dock "$DOCK" || true; }

DOCKED="${DOCK} visible=1 floating=0 area=2"

# 1. stream-pp as captured: OBS's default for a dock its layout never held.
cp "${FIX}/stream-pp.user.ini" "${WORK}/pp.ini"
PP0="$(restore "${WORK}/pp.ini")"
expect "$PP0" "restore=ok" "stream-pp: Qt restores the captured state"
expect "$PP0" "${DOCK} visible=0 floating=1 area=2" "stream-pp as captured: the dock is hidden + floating (the issue)"

# 2. stream-pp seeded.
got="$(seed_cli seed "${WORK}/pp.ini")"
if [ "$got" = seeded ]; then echo "ok:   stream-pp: seed = seeded"; else fail "stream-pp seed printed '${got}', want seeded"; fi
PP1="$(restore "${WORK}/pp.ini")"
expect "$PP1" "restore=ok" "stream-pp seeded: Qt restores the seeded state"
expect "$PP1" "$DOCKED" "stream-pp seeded: the dock is visible + docked right"
same_others "$PP0" "$PP1" "stream-pp seeded: every other dock unchanged"

# 3. stream-snv as captured.
cp "${FIX}/stream-snv.user.ini" "${WORK}/snv.ini"
got="$(seed_cli seed "${WORK}/snv.ini")"
if [ "$got" = present ]; then echo "ok:   stream-snv: seed = present"; else fail "stream-snv seed printed '${got}', want present"; fi
if [ "$(grep '^DockState=' "${FIX}/stream-snv.user.ini")" = "$(grep '^DockState=' "${WORK}/snv.ini")" ]; then echo "ok:   stream-snv: the saved layout unchanged"; else fail "stream-snv: the seed changed the saved layout"; fi
if grep -qx 'AvsyncDockSeeded=true' "${WORK}/snv.ini"; then echo "ok:   stream-snv: the first-seed marker is in user.ini"; else fail "stream-snv: no first-seed marker in user.ini"; fi
SNV="$(restore "${WORK}/snv.ini")"
expect "$SNV" "restore=ok" "stream-snv: Qt restores the captured state"
expect "$SNV" "$DOCKED" "stream-snv: the dock is visible + docked right"

# 4. the state an OBS that ran case 1 saves on exit (Qt's own saveState of it).
sed "s|^DockState=.*|DockState=$(sed -n 's/^state=//p' <<<"$PP0")|" "${FIX}/stream-pp.user.ini" > "${WORK}/ran.ini"
got="$(seed_cli check "${WORK}/ran.ini")"
if [ "$got" = default-hidden ]; then echo "ok:   OBS-saved default: check = default-hidden"; else fail "OBS-saved default: check printed '${got}'"; fi
got="$(seed_cli seed "${WORK}/ran.ini")"
if [ "$got" = redocked ]; then echo "ok:   OBS-saved default: seed = redocked"; else fail "OBS-saved default: seed printed '${got}', want redocked"; fi
RAN="$(restore "${WORK}/ran.ini")"
expect "$RAN" "restore=ok" "OBS-saved default re-docked: Qt restores it"
expect "$RAN" "$DOCKED" "OBS-saved default re-docked: the dock is visible + docked right"
same_others "$PP0" "$RAN" "OBS-saved default re-docked: every other dock unchanged"

# 5. a dock added to a right area the layout already has (stream-snv's): the append path.
cp "${FIX}/stream-snv.user.ini" "${WORK}/probe.ini"
got="$(python3 "$SEED" seed --ini "${WORK}/probe.ini" --dock "$PROBE" || true)"
if [ "$got" = seeded ]; then echo "ok:   stream-snv probe: seed = seeded"; else fail "stream-snv probe seed printed '${got}', want seeded"; fi
PRB="$(restore "${WORK}/probe.ini" "$PROBE")"
expect "$PRB" "restore=ok" "stream-snv probe: Qt restores the seeded state"
expect "$PRB" "${PROBE} visible=1 floating=0 area=2" "stream-snv probe: the probe joins the right area, visible"
expect "$PRB" "$DOCKED" "stream-snv probe: the A/V-sync dock stays visible + docked right"
same_others "$SNV" "$PRB" "stream-snv probe: every other dock unchanged"

if [ "$FAILS" -ne 0 ]; then echo "issue 1470 Qt dock-state proof: ${FAILS} FAILED" >&2; exit 1; fi
echo "issue 1470 Qt dock-state proof: all checks passed"
