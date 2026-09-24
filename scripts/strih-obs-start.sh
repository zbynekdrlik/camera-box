#!/usr/bin/env bash
# strih-obs-start.sh -- strih-lx supervised OBS launcher (issue 1317); extended header below.
set -euo pipefail
#
# WHY: systemd/strih-obs.service (ExecStart=/usr/local/bin/strih-obs-start.sh) supervises OBS on the
# Linux strih notebook, Restart=on-failure. This is the sibling of imag-obs-start.sh (issue 882) but
# for the strih-lx box, whose facts differ (see the Design comment on issue 1317):
#   * the plain Xorg `:0` session of the lightdm-autologin openbox kiosk (issue 1357 -- the SAME
#     appliance session imag runs; the GNOME Wayland session and its WAYLAND_DISPLAY path are gone):
#     DISPLAY=:0 when the X0 socket exists, else FAIL LOUD (OBS is started by the openbox autostart, so
#     no X server is a hard error);
#   * the OBS binary is installed into the /usr prefix at /usr/bin/obs (setup-strih.sh step 4 --
#     issue 1317; /opt/obs-genlock stays the staged copy + marker home). STRIH_OBS_BIN overrides it;
#   * NO taskset CPU pin unless STRIH_ISOLATED_CPUS / /etc/strih-isolated-cpus.conf exists (never a
#     guessed pin -- the imag #841 lesson); since issue 1357 the shared baseline's obs_box_cpu_affinity
#     persists the TOPOLOGY-DERIVED P-core block there (AFFINITY-ONLY, like imag's taskset pin);
#   * NO DRM lease and NO scene-seeder preflight (the strih scene seeder is a separate follow-up --
#     the launcher must not depend on a seeder that does not exist yet).
#
# Idempotent: OBS already running -> prints a note and exits 0 (never a second instance).
# On launch: clear crash sentinels -> launch obs -> wait <=90 s for the :4455 WebSocket (fail loud if
# obs dies during startup) -> `wait` on the obs pid so the unit's lifetime IS obs's lifetime (Type=
# simple, issue 882): a segfault -> non-zero -> Restart=on-failure; an operator quit -> 0 -> left
# alone.
#
# BASH_SOURCE-guarded (like setup-strih.sh): when sourced (the unit tests) it defines only the pure
# strih_resolve_session_env function and returns -- it never runs the live launch flow.

LOG=/tmp/strih-obs-start.log
OBS_BIN="${STRIH_OBS_BIN:-/usr/bin/obs}"
OBS_CFG="$HOME/.config/obs-studio"
SCN="${STRIH_SCENES_BIN:-/usr/local/bin/strih_scenes.py}"   # issue 1317: the input/scene/Studio seeder

# strih_resolve_session_env -> print the resolved graphical-session display env assignment (one
# KEY=VALUE line) to stdout and return 0; return non-zero (printing nothing) when no X socket exists.
# issue 1357: X11 ONLY -- the kiosk is plain Xorg (openbox), exactly like imag; a Wayland socket is
# deliberately ignored (the GNOME Wayland session is purged, and OBS must never land on XWayland again).
# Test seam: STRIH_X11_SOCKET_DIR (default /tmp/.X11-unix) for the X-socket probe.
strih_resolve_session_env() {
  local xdir="${STRIH_X11_SOCKET_DIR:-/tmp/.X11-unix}"
  if [ -e "${xdir}/X0" ]; then
    printf 'DISPLAY=:0\n'
    return 0
  fi
  return 1
}

# --- source-guard: when sourced (the unit tests), define funcs only -- never run the launch flow ---
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

exec >>"$LOG" 2>&1
echo "=== $(date '+%F %T') strih-obs-start ==="
echo "genlock build sha: $(cat /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null || echo unknown)"

# Idempotent: never a second OBS instance.
if pgrep -x obs >/dev/null; then
  echo "OBS uz bezi -- nic nerobim."
  exit 0
fi

# Resolve + export the Xorg display (DISPLAY=:0, else FAIL LOUD).
if ! SESSION_ENV="$(strih_resolve_session_env)"; then
  echo "FAIL: no X display found (no X0 socket under '${STRIH_X11_SOCKET_DIR:-/tmp/.X11-unix}'). OBS runs on the Xorg :0 openbox kiosk session -- refusing to launch OBS with no display."
  exit 1
fi
while IFS= read -r _kv; do
  [ -n "$_kv" ] || continue
  export "${_kv?}"
  echo "session display: $_kv"
done <<< "$SESSION_ENV"

rm -rf "$OBS_CFG/.sentinel"/* 2>/dev/null || true

# Optional CPU pin -- ONLY from an explicit source (STRIH_ISOLATED_CPUS env or the persisted file);
# never a guessed taskset range (the imag #841 lesson: a hand-tuned pin was silently wrong on a
# different-core box). No source -> no taskset at all.
ISOLATED_CPUS="${STRIH_ISOLATED_CPUS:-}"
if [ -z "$ISOLATED_CPUS" ] && [ -r /etc/strih-isolated-cpus.conf ]; then
  ISOLATED_CPUS="$(cat /etc/strih-isolated-cpus.conf 2>/dev/null || true)"
fi

# --profile / --collection ONLY when those OBS dirs exist -- a fresh box has none, so the launcher
# must fall back to the default (never pass a flag for a profile/collection OBS has not seeded yet).
STRIH_OBS_PROFILE="${STRIH_OBS_PROFILE:-strih-lx}"
STRIH_OBS_COLLECTION="${STRIH_OBS_COLLECTION:-strih-lx}"
OBS_ARGS=(--disable-shutdown-check)
if [ -d "${OBS_CFG}/basic/profiles/${STRIH_OBS_PROFILE}" ]; then
  OBS_ARGS+=(--profile "$STRIH_OBS_PROFILE")
  echo "OBS profile '${STRIH_OBS_PROFILE}' present -- using it"
else
  echo "OBS profile '${STRIH_OBS_PROFILE}' not present -- launching with the default profile"
fi
if [ -f "${OBS_CFG}/basic/scenes/${STRIH_OBS_COLLECTION}.json" ]; then
  OBS_ARGS+=(--collection "$STRIH_OBS_COLLECTION")
  echo "OBS scene collection '${STRIH_OBS_COLLECTION}' present -- using it"
else
  echo "OBS scene collection '${STRIH_OBS_COLLECTION}' not present -- launching with the default collection"
fi

[ -x "$OBS_BIN" ] || { echo "FAIL: OBS binary '${OBS_BIN}' not found/executable -- install the genlock bundle (setup-strih.sh step 4) first"; exit 1; }

# issue 1357: no GPU env here. The Xorg server runs PRIME nvidia-PRIMARY (the shared baseline's
# obs_box_nvidia_prime), so OBS renders on the RTX 5050 directly -- the issue-1352 XWayland PRIME
# render-offload exports are gone with the GNOME Wayland session they worked around.

# issue 1317 (imag issue 1156 pattern): PREFLIGHT the seed's Python import chain BEFORE launching
# OBS. This wrapper launches OBS and only AFTERWARD runs the seeder; if a module
# the seed imports is missing on the box (e.g. python3-websocket -- strih_scenes imports it at module
# load), the seed would die on ModuleNotFoundError AFTER OBS is already up -> set -e aborts this
# script -> Restart=on-failure relaunches -> a HEALTHY OBS flaps on the live cut. Failing HERE, before
# any launch, fails the unit cleanly and never touches a running OBS. Loading strih_scenes from the
# REAL on-box install dir transitively validates the websocket dep too. obs_phase2 is imported LAZILY
# by strih_scenes (an older box may lack it -> the seed degrades to a direct set), so it is not
# required by this preflight.
if ! python3 -c "import sys; sys.path.insert(0, '/usr/local/bin'); import strih_scenes"; then
  echo "FAIL: strih_scenes import preflight failed -- a seed dependency is missing on the box (e.g. python3-websocket). Refusing to launch OBS (a broken seed would Restart-loop it). Fix: re-run setup-strih.sh (step 6 installs strih_scenes.py; python3-websocket is a runtime dep)."
  exit 1
fi

if [ -n "$ISOLATED_CPUS" ]; then
  echo "launching: taskset -c ${ISOLATED_CPUS} ${OBS_BIN} ${OBS_ARGS[*]}"
  taskset -c "$ISOLATED_CPUS" "$OBS_BIN" "${OBS_ARGS[@]}" &
else
  echo "launching: ${OBS_BIN} ${OBS_ARGS[*]} (no CPU pin -- neither STRIH_ISOLATED_CPUS nor /etc/strih-isolated-cpus.conf is set)"
  "$OBS_BIN" "${OBS_ARGS[@]}" &
fi
OBS_PID=$!
echo "obs launched (pid $OBS_PID)"

deadline=$((SECONDS + 90))
until (exec 3<>/dev/tcp/127.0.0.1/4455) 2>/dev/null; do
  if ! pgrep -x obs >/dev/null; then
    echo "FAIL: obs proces zmizol pocas nabehu"
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "FAIL: OBS WebSocket (port 4455) nenabehol do 90 s"
    exit 1
  fi
  sleep 3
done
exec 3<&- 3>&- 2>/dev/null || true
echo "OK: OBS bezi (pid $OBS_PID), WS :4455 up."

# issue 1317: seed the collection (idempotent) now that OBS's WebSocket is up -- CreateScene +
# CreateInput per manifest input with the certified genlock settings (genlock_fifo/ndi_sync=2/floor 3)
# + read-back-verified ndi_source_name + Studio Mode. The import chain was proven by the launch
# preflight above (imag issue 1156 pattern), so a MISSING-DEP failure is already ruled out here.
# sleep 2 lets the WS ident handshake layer settle before the seed connects (imag's same settle).
# --host defaults to 127.0.0.1 inside strih_scenes.py.
#
# BEST-EFFORT (issue 1317 review): OBS is ALREADY launched (pid $OBS_PID). A RUNTIME seed failure --
# a transient WS hiccup, a corrupt strih-lx-seed.json, or the WS demanding auth after a config regen
# (a documented strih incident) -- must NOT abort this wrapper under `set -euo pipefail`, because that
# would SIGTERM the cgroup (the live OBS) and Restart=on-failure would FLAP a healthy cut. An
# empty-but-running OBS is strictly better than a flapping one; verify-strih item 4b reports the
# unseeded state and the seed is idempotently re-tried on the next launch. Loud WARN, never fatal.
sleep 2
if python3 "$SCN" --bootstrap; then
  echo "OK: scenes seednute (strih_scenes.py --bootstrap)."
else
  echo "WARN issue 1317: strih_scenes.py --bootstrap FAILED (non-fatal) -- OBS stays UP; collection may be unseeded. Re-seed: python3 ${SCN} --bootstrap. NOT aborting the unit (would flap a live OBS)."
fi

# issue 1242: the BANDWIDTH ROLES on every launch (default on, never a forgettable manual step) --
# program-path cameras connect only while shown, the built-in multiview renders the always-connected
# low-bandwidth `MV` twins. Idempotent (a correct collection is a pure read). Same best-effort contract
# as the seed above: OBS is live, so a failure is a loud WARN, never a unit abort.
if python3 "$SCN" --apply-roles; then
  echo "OK: bandwidth roles (strih_scenes.py --apply-roles)."
else
  echo "WARN issue 1242: strih_scenes.py --apply-roles FAILED (non-fatal) -- OBS stays UP; every camera may still be pulled full-bandwidth. Re-apply: python3 ${SCN} --apply-roles."
fi

# #882: BLOCK until obs itself exits, then propagate ITS exit status -- makes obs (not this wrapper)
# the process a Type=simple unit tracks. A signal death (segfault) reports non-zero and
# Restart=on-failure relaunches; a clean exit(0) (operator quit) reports 0 and is left alone.
echo "supervising obs (pid $OBS_PID) -- exit propagated to systemd for Restart=on-failure (#882)"
wait "$OBS_PID"
OBS_EXIT=$?
echo "=== $(date '+%F %T') obs exited (code $OBS_EXIT) ==="
exit "$OBS_EXIT"
