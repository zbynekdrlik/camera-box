#!/usr/bin/env bash
# strih-obs-start.sh -- strih-lx supervised OBS launcher (issue 1317); extended header below.
set -euo pipefail
#
# WHY: systemd/strih-obs.service (ExecStart=/usr/local/bin/strih-obs-start.sh) supervises OBS on the
# Linux strih notebook, Restart=on-failure. This is the sibling of imag-obs-start.sh (issue 882) but
# for the strih-lx box, whose facts differ (see the Design comment on issue 1317):
#   * a GNOME WAYLAND session on Ubuntu 26.04 -- DISPLAY is UNSET in the user session, so we resolve
#     WAYLAND_DISPLAY from the wayland-* socket under XDG_RUNTIME_DIR (XWayland DISPLAY=:0 fallback,
#     else FAIL LOUD -- the unit is After=graphical-session.target so no display is a hard error);
#   * the OBS binary lives at /opt/obs-genlock/bin/obs (NOT on PATH);
#   * NO taskset CPU pin unless STRIH_ISOLATED_CPUS / /etc/strih-isolated-cpus.conf exists (never a
#     guessed pin -- the imag #841 lesson);
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
OBS_BIN="${STRIH_OBS_BIN:-/opt/obs-genlock/bin/obs}"
OBS_CFG="$HOME/.config/obs-studio"

# strih_resolve_session_env -> print the resolved graphical-session display env assignment (one
# KEY=VALUE line) to stdout and return 0; return non-zero (printing nothing) when neither a Wayland
# nor an X socket exists. Wayland first (26.04 GNOME default), XWayland/X11 DISPLAY=:0 fallback, else
# FAIL. Test seams: XDG_RUNTIME_DIR (where the wayland-* sockets live) and STRIH_X11_SOCKET_DIR
# (default /tmp/.X11-unix) for the X-socket probe. A wayland-*.lock sibling is ignored.
strih_resolve_session_env() {
  local rt="${XDG_RUNTIME_DIR:-}" xdir="${STRIH_X11_SOCKET_DIR:-/tmp/.X11-unix}" s base
  if [ -n "$rt" ]; then
    for s in "$rt"/wayland-*; do
      case "$s" in *.lock) continue ;; esac
      [ -e "$s" ] || continue
      base="$(basename "$s")"
      printf 'WAYLAND_DISPLAY=%s\n' "$base"
      return 0
    done
  fi
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

# Resolve + export the graphical-session display (Wayland-first, X11 fallback, else FAIL LOUD).
if ! SESSION_ENV="$(strih_resolve_session_env)"; then
  echo "FAIL: no graphical session display found (no wayland-* socket under XDG_RUNTIME_DIR='${XDG_RUNTIME_DIR:-<unset>}' and no X socket under '${STRIH_X11_SOCKET_DIR:-/tmp/.X11-unix}'). The unit is After=graphical-session.target -- refusing to launch OBS with no display."
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

# #882: BLOCK until obs itself exits, then propagate ITS exit status -- makes obs (not this wrapper)
# the process a Type=simple unit tracks. A signal death (segfault) reports non-zero and
# Restart=on-failure relaunches; a clean exit(0) (operator quit) reports 0 and is left alone.
echo "supervising obs (pid $OBS_PID) -- exit propagated to systemd for Restart=on-failure (#882)"
wait "$OBS_PID"
OBS_EXIT=$?
echo "=== $(date '+%F %T') obs exited (code $OBS_EXIT) ==="
exit "$OBS_EXIT"
