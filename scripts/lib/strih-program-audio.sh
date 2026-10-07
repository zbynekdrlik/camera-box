#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih-lx program-audio sampler constants, the
# setup-strih step 16e install and the verify-strih item 41 grader; no top-level statements besides
# constants) -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here:
# sourcing runs in the CALLER's shell, so strict mode would leak into it. setup-strih.sh /
# verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/strih-program-audio.sh -- issue 1404 (ROZHODNUTÉ 6039368611): the stream program-audio
# sampler (the YouTube channel guard) moved off dev1 to strih-lx. It runs there as the operator's
# `--user` unit program-audio-sampler.service, pinned to the E-cores at normal priority, and serves its
# own read-only endpoint http://<strih-lx>:8891/program-audio.json (scripts/program_audio_http.py).
#
#   * strih_program_audio_install -- setup-strih step 16e (as root):
#     - the apt package python3-numpy (only when missing) + an `import numpy` preflight;
#     - the sampler's files in the CHECKOUT layout under /usr/local/lib/camera-box (the python modules,
#       the decoder shim's C++ source + its two vendored headers + the build script): the sampler
#       compares the decoder library it loads against those sources, and the build script builds from
#       them. Each file is written only when its bytes or mode differ;
#     - the decoder shim built AS THE OPERATOR with that build script into the default
#       ~/.local/lib/camera-box/libqpsk-guard-shim.so, only when it is missing or was built from other
#       sources (never loaded as root: the library lives in the operator's home);
#     - the unit rendered from systemd/program-audio-sampler.strih-lx.service: CPUAffinity= = the box's
#       own /sys/devices/cpu_atom/cpus (the E-cores, read the way strih_lx_lowprio_prefix reads them;
#       no affinity line on a non-hybrid box), written to ~/.config/systemd/user only when it differs;
#     - `systemctl --user enable` + a daemon-reload when the unit file changed + a `try-restart` when
#       anything changed. A stopped sampler is NEVER started here (enable-only): rig-mode.sh test starts
#       it, rig-mode.sh event stops it (scripts/lib/program-audio-mode.sh).
#   * strih_program_audio_grade_rows / _report -- verify-strih item 41: the files + the rendered unit
#     installed and enabled; the shim built from the installed sources; the unit's state against the
#     TEST marker: running = its endpoint must answer a FRESH verdict; down without the marker = a NOTE
#     (EVENT mode, or never put in TEST mode -- the default after provisioning); down WITH the marker,
#     failed, or stuck activating (a crash loop) = a FAIL.
#
# Test seams (tests/python/test_strih_program_audio_1404.py): STRIH_PROGRAM_AUDIO_PREFIX
# (/usr/local/lib/camera-box), STRIH_PROGRAM_AUDIO_SYSFS (/sys), STRIH_PROGRAM_AUDIO_EUID (the running
# uid), STRIH_PROGRAM_AUDIO_PYTHON (/usr/bin/python3), STRIH_PROGRAM_AUDIO_CURL (curl),
# STRIH_PROGRAM_AUDIO_PORT (the endpoint port when the env file names none: 8891),
# STRIH_PROGRAM_AUDIO_RETRY_SLEEP (1 s between the grader's re-reads); `dpkg-query`, `apt-get`, `sudo`,
# `systemctl`, `id` and `chown` are looked up on PATH.

# The write-if-changed installer + the operator-session systemctl live in the session-apps lib.
if ! declare -F strih_session_apps_put >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi

STRIH_PROGRAM_AUDIO_UNIT=program-audio-sampler.service
STRIH_PROGRAM_AUDIO_UNIT_TEMPLATE=program-audio-sampler.strih-lx.service
STRIH_PROGRAM_AUDIO_PACKAGES=(python3-numpy)
# Everything the sampler imports + the decoder shim's sources and build script, in the checkout layout
# (pinned against the sampler's real import closure by a test).
STRIH_PROGRAM_AUDIO_FILES=(
  scripts/program_audio.py
  scripts/program_audio_capture.py
  scripts/program_audio_http.py
  scripts/program_audio_marker.py
  scripts/program_audio_ndi.py
  scripts/program_audio_sampler.py
  scripts/rig_serve_files.py
  scripts/qpsk_guard_shim.cpp
  scripts/build-qpsk-guard-shim.sh
  vendor/av-sync-dock/src/camera-box-audio.hpp
  vendor/av-sync-dock/src/camera-box-marker-scan.hpp
)
# rig-mode.sh test leaves this file in the operator's home and rig-mode.sh event removes it; the unit's
# ExecCondition skips every start without it, so the sampler is DOWN by default (a fresh provisioning, a
# reboot during a production). Pinned against the template's ExecCondition by a test.
STRIH_PROGRAM_AUDIO_TEST_MARKER=.config/camera-box/program-audio-sampler.test-mode
# The sampler's private env file (the unit's EnvironmentFile): the grader reads the endpoint port from it.
STRIH_PROGRAM_AUDIO_ENV_FILE=.config/camera-box/program-audio-sampler.env
# The guard's own freshness window, -FUTURE_TOLERANCE_S <= age_s <= MAX_AGE_S: older is a sampler that
# stopped writing (= program_audio_guard.py DEFAULT_MAX_AGE_S, its --max-age default); a little in the
# future is a dantesync date step -- strih-lx is the date master -- (= its NEGATIVE_AGE_TOLERANCE_S).
# Both pinned to the guard by a test.
STRIH_PROGRAM_AUDIO_MAX_AGE_S=10
STRIH_PROGRAM_AUDIO_FUTURE_TOLERANCE_S=1
# The build script's default output, relative to the operator's home (= program_audio_marker.py's
# DEFAULT_SHIM_PATH).
STRIH_PROGRAM_AUDIO_SHIM=.local/lib/camera-box/libqpsk-guard-shim.so

strih_program_audio_prefix() { printf '%s' "${STRIH_PROGRAM_AUDIO_PREFIX:-/usr/local/lib/camera-box}"; }
strih_program_audio_port() { printf '%s' "${STRIH_PROGRAM_AUDIO_PORT:-8891}"; }
strih_program_audio_python() { printf '%s' "${STRIH_PROGRAM_AUDIO_PYTHON:-/usr/bin/python3}"; }

# strih_program_audio_file_mode REL -> the install mode: the sampler + the build script run (0755),
# everything else is read (0644).
strih_program_audio_file_mode() {
  case "${1-}" in
    scripts/program_audio_sampler.py|scripts/build-qpsk-guard-shim.sh) printf '0755' ;;
    *) printf '0644' ;;
  esac
}

# strih_program_audio_ecores [SYSFS_ROOT] -> the E-core cpu list of this box, the strih_lx_lowprio_prefix
# contract: <root>/devices/cpu_atom/cpus with whitespace removed; EMPTY when the file is absent,
# unreadable, empty, or not a cpu list (digits, commas, dashes) -- never a garbage CPUAffinity= value.
strih_program_audio_ecores() {
  local root="${1:-${STRIH_PROGRAM_AUDIO_SYSFS:-/sys}}" file cpus=""
  file="${root%/}/devices/cpu_atom/cpus"
  if [ -r "$file" ]; then
    cpus="$(tr -d '[:space:]' < "$file" 2>/dev/null || true)"
  fi
  case "$cpus" in
    ''|*[!0-9,-]*|[,-]*|*[,-]) printf '' ;;
    *) printf '%s' "$cpus" ;;
  esac
}

# strih_program_audio_unit_text REPO ECORES -> the unit as installed: the template with its ONE
# `CPUAffinity=@E_CORES@` line filled in, or dropped when ECORES is empty (a non-hybrid box runs on
# every cpu). rc 1 + a stderr line when the template is missing or does not hold exactly one such line.
strih_program_audio_unit_text() {
  local repo="${1:?repo root required}" ecores="${2-}" tpl n
  tpl="${repo}/systemd/${STRIH_PROGRAM_AUDIO_UNIT_TEMPLATE}"
  [ -f "$tpl" ] || { echo "strih-program-audio: ${tpl} not found" >&2; return 1; }
  n="$(grep -c '^CPUAffinity=@E_CORES@$' "$tpl" || true)"
  [ "$n" = 1 ] || { echo "strih-program-audio: ${tpl} must hold exactly one CPUAffinity=@E_CORES@ line (found ${n})" >&2; return 1; }
  awk -v cpus="$ecores" '
    $0 == "CPUAffinity=@E_CORES@" { if (cpus != "") print "CPUAffinity=" cpus; next }
    { print }
  ' "$tpl"
}

# strih_program_audio_as_user USER HOME CMD... -> run CMD as the operator: as root through
# `sudo -u USER env HOME=HOME`, else inline. Returns CMD's rc.
strih_program_audio_as_user() {
  local user="${1:?user required}" home="${2:?home required}"
  shift 2
  if [ "${STRIH_PROGRAM_AUDIO_EUID:-$(id -u)}" = 0 ]; then
    sudo -u "$user" env HOME="$home" "$@"
  else
    "$@"
  fi
}

# strih_program_audio_shim_state USER HOME -> current | stale | unloadable | missing, always rc 0: the
# operator's decoder library against the INSTALLED shim sources, checked AS THE OPERATOR with the
# installed program_audio_marker.py (a library in the operator's home is never loaded as root).
strih_program_audio_shim_state() {
  local user="${1:?user required}" home="${2:?home required}" shim out prog
  shim="${home}/${STRIH_PROGRAM_AUDIO_SHIM}"
  [ -f "$shim" ] || { printf 'missing'; return 0; }
  prog='import sys
sys.path.insert(0, sys.argv[1])
import program_audio_marker as pam
try:
    d = pam.MarkerDecoder(sys.argv[2])
except pam.DecoderUnavailable as exc:
    print("unloadable")
    sys.exit(0)
print("stale" if d.sources_stale() else "current")'
  out="$(strih_program_audio_as_user "$user" "$home" "$(strih_program_audio_python)" -c "$prog" \
    "$(strih_program_audio_prefix)/scripts" "$shim" 2>/dev/null || true)"
  case "${out##*$'\n'}" in
    current|stale|unloadable) printf '%s' "${out##*$'\n'}" ;;
    *) printf 'unloadable' ;;
  esac
}

strih_program_audio_missing_packages() {
  local p
  for p in "${STRIH_PROGRAM_AUDIO_PACKAGES[@]}"; do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] || printf '%s\n' "$p"
  done
  return 0
}

# --- setup-strih step 16e ----------------------------------------------------------------------------

# strih_program_audio_install REPO USER_HOME DESKTOP_USER -> see the header. rc 1 + a stderr line on a
# missing source, a package that will not install, numpy that will not import, a failed file install
# or a shim that will not build; a `systemctl --user` call that cannot reach the user bus only WARNs.
strih_program_audio_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local prefix unitdir rel w missing ecores tmp err shim changed=0 unit_written=0
  prefix="$(strih_program_audio_prefix)"
  unitdir="${home}/.config/systemd/user"
  for rel in "${STRIH_PROGRAM_AUDIO_FILES[@]}"; do
    [ -f "${repo}/${rel}" ] || { echo "strih-program-audio: ${rel} not found under ${repo}" >&2; return 1; }
  done

  missing="$(strih_program_audio_missing_packages)"
  if [ -n "$missing" ]; then
    echo "  installing ${missing//$'\n'/ }"
    # shellcheck disable=SC2086  # one word per package name
    DEBIAN_FRONTEND=noninteractive apt-get install -y $missing || {
      echo "strih-program-audio: apt-get install ${missing//$'\n'/ } failed" >&2; return 1; }
  fi
  "$(strih_program_audio_python)" -c 'import numpy' || {
    echo "strih-program-audio: $(strih_program_audio_python) cannot import numpy (package ${STRIH_PROGRAM_AUDIO_PACKAGES[*]})" >&2
    return 1; }
  echo "  numpy importable (${STRIH_PROGRAM_AUDIO_PACKAGES[*]})"

  for rel in "${STRIH_PROGRAM_AUDIO_FILES[@]}"; do
    install -d -m 0755 "${prefix}/$(dirname "$rel")" || { echo "strih-program-audio: cannot create ${prefix}/$(dirname "$rel")" >&2; return 1; }
    w="$(strih_session_apps_put "${repo}/${rel}" "${prefix}/${rel}" "$(strih_program_audio_file_mode "$rel")")" || return 1
    [ "$w" = written ] && changed=1
  done
  if [ "$changed" = 1 ]; then
    echo "  installed the sampler files -> ${prefix} (${#STRIH_PROGRAM_AUDIO_FILES[@]} files, the checkout layout)"
  else
    echo "  the sampler files in ${prefix} unchanged"
  fi

  shim="$(strih_program_audio_shim_state "$user" "$home")"
  if [ "$shim" != current ]; then
    echo "  decoder shim ${shim}: building it as ${user} -> ~/${STRIH_PROGRAM_AUDIO_SHIM}"
    strih_program_audio_as_user "$user" "$home" "${prefix}/scripts/build-qpsk-guard-shim.sh" || {
      echo "strih-program-audio: building the decoder shim as ${user} failed (g++ present?)" >&2; return 1; }
    shim="$(strih_program_audio_shim_state "$user" "$home")"
    [ "$shim" = current ] || { echo "strih-program-audio: the decoder shim reads '${shim}' right after its build" >&2; return 1; }
    changed=1
  else
    echo "  decoder shim current (built from the installed sources)"
  fi

  ecores="$(strih_program_audio_ecores "${STRIH_PROGRAM_AUDIO_SYSFS:-/sys}")"
  install -d -m 0755 "$unitdir" || { echo "strih-program-audio: cannot create ${unitdir}" >&2; return 1; }
  tmp="$(mktemp)" || { echo "strih-program-audio: mktemp failed" >&2; return 1; }
  if ! strih_program_audio_unit_text "$repo" "$ecores" > "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  w="$(strih_session_apps_put "$tmp" "${unitdir}/${STRIH_PROGRAM_AUDIO_UNIT}" 0644)" || { rm -f "$tmp"; return 1; }
  rm -f "$tmp"
  if [ "$w" = written ]; then
    unit_written=1
    changed=1
  fi
  echo "  ${STRIH_PROGRAM_AUDIO_UNIT} ${w} (CPUAffinity=${ecores:-none: no cpu_atom, every cpu}; normal priority)"
  chown -R "${user}:${user}" "$unitdir" 2>/dev/null || echo "  WARN: could not chown ${unitdir} to ${user}"

  if ! err="$(strih_session_apps_user_systemctl "$user" enable "$STRIH_PROGRAM_AUDIO_UNIT" 2>&1)"; then
    echo "  WARN: systemctl --user enable ${STRIH_PROGRAM_AUDIO_UNIT} failed for ${user} (${err//$'\n'/ }) -- enable it by hand once logged in"
    return 0
  fi
  echo "  enabled ${STRIH_PROGRAM_AUDIO_UNIT} (NOT started here: it runs in TEST mode only -- rig-mode.sh test starts it, event stops it)"
  if [ "$unit_written" = 1 ] && ! err="$(strih_session_apps_user_systemctl "$user" daemon-reload 2>&1)"; then
    echo "  WARN: systemctl --user daemon-reload failed (${err//$'\n'/ }) -- run daemon-reload + try-restart ${STRIH_PROGRAM_AUDIO_UNIT} by hand"
    return 0
  fi
  if [ "$changed" = 1 ]; then
    if err="$(strih_session_apps_user_systemctl "$user" try-restart "$STRIH_PROGRAM_AUDIO_UNIT" 2>&1)"; then
      echo "  try-restart ${STRIH_PROGRAM_AUDIO_UNIT} (only a running sampler is restarted onto the new files)"
    else
      echo "  WARN: systemctl --user try-restart ${STRIH_PROGRAM_AUDIO_UNIT} failed (${err//$'\n'/ }) -- a running sampler still runs the old files"
    fi
  fi
  return 0
}

# --- verify-strih item 41 ------------------------------------------------------------------------------

# strih_program_audio_env_value HOME KEY -> KEY's value in the sampler's env file (the last
# `KEY=value` line; whitespace around `=` and after the value dropped and quotes stripped, as systemd's
# EnvironmentFile reads it), EMPTY when the file or the key is absent. Parsed, never sourced. Always rc 0.
strih_program_audio_env_value() {
  local file="${1:?home required}/${STRIH_PROGRAM_AUDIO_ENV_FILE}" key="${2:?key required}" v=""
  [ -r "$file" ] && v="$(sed -n "s/^[[:space:]]*${key}[[:space:]]*=[[:space:]]*//p" "$file" 2>/dev/null | tail -n 1 || true)"
  v="${v%"${v##*[![:space:]]}"}"
  v="${v%\"}"; v="${v#\"}"; v="${v%\'}"; v="${v#\'}"
  printf '%s' "$v"
}

# strih_program_audio_endpoint HOME -> `<host> <port>` the grader reads: the env file's
# PROGRAM_AUDIO_HTTP_PORT / _BIND, else the default port; a wildcard or loopback bind is read on
# 127.0.0.1. Port 0 (no endpoint) prints `- 0`.
strih_program_audio_endpoint() {
  local home="${1:?home required}" port bind
  port="$(strih_program_audio_env_value "$home" PROGRAM_AUDIO_HTTP_PORT)"
  bind="$(strih_program_audio_env_value "$home" PROGRAM_AUDIO_HTTP_BIND)"
  case "$port" in ''|*[!0-9]*) port="$(strih_program_audio_port)" ;; esac
  case "$bind" in ''|0.0.0.0|::|127.0.0.1|localhost) bind=127.0.0.1 ;; esac
  [ "$port" = 0 ] && bind=-
  printf '%s %s' "$bind" "$port"
}

# strih_program_audio_endpoint_read HOST PORT -> `<verdict> <age_s> fresh|stale` of
# http://HOST:PORT/program-audio.json (stale = outside the guard's -FUTURE_TOLERANCE_S..MAX_AGE_S
# window, or no age), EMPTY when it does not answer a payload with a verdict and a source. Always rc 0.
strih_program_audio_endpoint_read() {
  local host="${1:?host required}" port="${2:?port required}" body
  body="$("${STRIH_PROGRAM_AUDIO_CURL:-curl}" -fsS --max-time 3 "http://${host}:${port}/program-audio.json" 2>/dev/null || true)"
  [ -n "$body" ] || return 0
  printf '%s' "$body" | python3 -c 'import json, sys
try:
    j = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
if isinstance(j, dict) and j.get("verdict") in ("MEASUREMENT", "FOREIGN", "SILENT", "UNKNOWN") and "source" in j:
    age = j.get("age_s")
    fresh = (isinstance(age, (int, float)) and not isinstance(age, bool)
             and -float(sys.argv[2]) <= age <= float(sys.argv[1]))
    print(j["verdict"], age, "fresh" if fresh else "stale")' \
    "$STRIH_PROGRAM_AUDIO_MAX_AGE_S" "$STRIH_PROGRAM_AUDIO_FUTURE_TOLERANCE_S" 2>/dev/null || true
}

# strih_program_audio_state USER -> the unit's is-active word, re-read (up to 3 x, STRIH_PROGRAM_AUDIO_RETRY_SLEEP
# apart) while it is in a transition (activating / deactivating / reloading): a sampler that is just
# starting settles, a crash loop stays `activating`. `unreadable` when systemctl printed nothing (the
# operator's user manager unreachable). Always rc 0.
strih_program_audio_state() {
  local user="${1:?user required}" s i
  for i in 1 2 3; do
    s="$(strih_session_apps_user_systemctl "$user" is-active "$STRIH_PROGRAM_AUDIO_UNIT" 2>/dev/null || true)"
    s="${s%%$'\n'*}"
    case "$s" in activating|deactivating|reloading) ;; *) break ;; esac
    [ "$i" = 3 ] || sleep "${STRIH_PROGRAM_AUDIO_RETRY_SLEEP:-1}"
  done
  printf '%s' "${s:-unreadable}"
}

# strih_program_audio_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|text`, `NOTE|text`
# or `FAIL|text` (strih_program_audio_row_count of them). Always rc 0.
strih_program_audio_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local prefix unitdir rel st bad_files="" tmp ustate enabled shim active endpoint host port answer marker i
  prefix="$(strih_program_audio_prefix)"
  unitdir="${home}/.config/systemd/user"
  for rel in "${STRIH_PROGRAM_AUDIO_FILES[@]}"; do
    st="$(strih_session_app_file_state "${prefix}/${rel}" "${repo}/${rel}")"
    [ "$st" = ok ] || bad_files="${bad_files} ${rel}=${st}"
  done
  tmp="$(mktemp)"
  if strih_program_audio_unit_text "$repo" "$(strih_program_audio_ecores "${STRIH_PROGRAM_AUDIO_SYSFS:-/sys}")" > "$tmp" 2>/dev/null; then
    ustate="$(strih_session_app_file_state "${unitdir}/${STRIH_PROGRAM_AUDIO_UNIT}" "$tmp")"
  else
    ustate=template-broken
  fi
  rm -f "$tmp"
  enabled=0; [ -L "${unitdir}/default.target.wants/${STRIH_PROGRAM_AUDIO_UNIT}" ] && enabled=1
  if [ -z "$bad_files" ] && [ "$ustate" = ok ] && [ "$enabled" = 1 ]; then
    printf 'OK|(program-audio-files) %s + the sampler files in %s match the checkout, enabled\n' "$STRIH_PROGRAM_AUDIO_UNIT" "$prefix"
  else
    printf 'FAIL|(program-audio-files) unit=%s enabled=%s files:%s -- re-run setup-strih.sh step 16e (issue 1404)\n' \
      "$ustate" "$enabled" "${bad_files:- ok}"
  fi

  shim="$(strih_program_audio_shim_state "$user" "$home")"
  if [ "$shim" = current ]; then
    printf 'OK|(program-audio-shim) ~/%s built from the installed decoder sources\n' "$STRIH_PROGRAM_AUDIO_SHIM"
  else
    printf 'FAIL|(program-audio-shim) ~/%s is %s -- re-run setup-strih.sh step 16e (it builds the shim as %s)\n' \
      "$STRIH_PROGRAM_AUDIO_SHIM" "$shim" "$user"
  fi

  active="$(strih_program_audio_state "$user")"
  endpoint="$(strih_program_audio_endpoint "$home")"
  host="${endpoint%% *}"; port="${endpoint#* }"
  marker=no; [ -e "${home}/${STRIH_PROGRAM_AUDIO_TEST_MARKER}" ] && marker=yes
  case "$active" in
    active)
      if [ "$marker" = no ]; then
        # EVENT mode, yet running: rig-mode.sh event removed the marker but its stop failed or timed
        # out, or the marker was removed by hand -- the state the opt-in marker exists to prevent
        printf 'FAIL|(program-audio-endpoint) running but not in TEST mode (no ~/%s): rig-mode.sh event did not stop it -- systemctl --user stop %s, or rig-mode.sh test if this is a development period\n' \
          "$STRIH_PROGRAM_AUDIO_TEST_MARKER" "$STRIH_PROGRAM_AUDIO_UNIT"
        return 0
      fi
      if [ "$port" = 0 ]; then
        printf 'FAIL|(program-audio-endpoint) running with no endpoint (PROGRAM_AUDIO_HTTP_PORT=0 in ~/%s) -- its consumers read nothing\n' "$STRIH_PROGRAM_AUDIO_ENV_FILE"
        return 0
      fi
      answer=""
      for i in 1 2 3; do  # a grade right after a (re)start: the sampler binds within a second or two
        answer="$(strih_program_audio_endpoint_read "$host" "$port")"
        [ -z "$answer" ] || break
        [ "$i" = 3 ] || sleep "${STRIH_PROGRAM_AUDIO_RETRY_SLEEP:-1}"
      done
      if [ -z "$answer" ]; then
        printf 'FAIL|(program-audio-endpoint) running but http://%s:%s/program-audio.json does not answer a verdict -- read journalctl --user -u %s\n' \
          "$host" "$port" "$STRIH_PROGRAM_AUDIO_UNIT"
      elif [ "${answer##* }" = fresh ]; then
        answer="${answer% *}"
        printf 'OK|(program-audio-endpoint) running; http://%s:%s/program-audio.json answers verdict=%s age_s=%s\n' \
          "$host" "$port" "${answer%% *}" "${answer#* }"
      else
        answer="${answer% *}"
        printf 'FAIL|(program-audio-endpoint) running but its verdict is stale (verdict=%s age_s=%s, outside -%s..%s s) -- the sampler stopped writing, or the clock moved; read journalctl --user -u %s\n' \
          "${answer%% *}" "${answer#* }" "$STRIH_PROGRAM_AUDIO_FUTURE_TOLERANCE_S" "$STRIH_PROGRAM_AUDIO_MAX_AGE_S" "$STRIH_PROGRAM_AUDIO_UNIT"
      fi
      ;;
    failed)
      printf 'FAIL|(program-audio-endpoint) %s failed -- read journalctl --user -u %s\n' "$STRIH_PROGRAM_AUDIO_UNIT" "$STRIH_PROGRAM_AUDIO_UNIT"
      ;;
    inactive)
      if [ "$marker" = yes ]; then
        printf 'FAIL|(program-audio-endpoint) TEST mode (~/%s present) but %s is not running -- systemctl --user start %s; read its journal\n' \
          "$STRIH_PROGRAM_AUDIO_TEST_MARKER" "$STRIH_PROGRAM_AUDIO_UNIT" "$STRIH_PROGRAM_AUDIO_UNIT"
      else
        printf 'NOTE|(program-audio-endpoint) down: not in TEST mode (no ~/%s: EVENT mode, or never put in TEST mode since the provisioning) -- rig-mode.sh test starts it\n' \
          "$STRIH_PROGRAM_AUDIO_TEST_MARKER"
      fi
      ;;
    unreadable)
      printf 'FAIL|(program-audio-endpoint) the state of %s is unreadable: the operator'"'"'s user manager does not answer (is %s lingering? loginctl show-user %s -p Linger)\n' \
        "$STRIH_PROGRAM_AUDIO_UNIT" "$user" "$user"
      ;;
    *)
      printf 'FAIL|(program-audio-endpoint) %s is %s (a crash loop, or stuck in a transition) -- read journalctl --user -u %s\n' \
        "$STRIH_PROGRAM_AUDIO_UNIT" "$active" "$STRIH_PROGRAM_AUDIO_UNIT"
      ;;
  esac
  return 0
}

strih_program_audio_row_count() { printf '3'; }

# strih_program_audio_grade_report REPO USER_HOME DESKTOP_USER -> print the rows through the CALLER's ok /
# note / bad functions (verify-strih). rc 1 unless the grader printed every row.
strih_program_audio_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_program_audio_grade_rows "$@")
  [ "$rows" = "$(strih_program_audio_row_count)" ]
}
