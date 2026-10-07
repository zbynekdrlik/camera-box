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
#     installed and enabled; the shim built from the installed sources; the endpoint answering a
#     verdict while the unit runs (a stopped unit is a NOTE: EVENT mode, or not started since the
#     provisioning; a failed one is a FAIL).
#
# Test seams (tests/python/test_strih_program_audio_1404.py): STRIH_PROGRAM_AUDIO_PREFIX
# (/usr/local/lib/camera-box), STRIH_PROGRAM_AUDIO_SYSFS (/sys), STRIH_PROGRAM_AUDIO_EUID (the running
# uid), STRIH_PROGRAM_AUDIO_PYTHON (/usr/bin/python3), STRIH_PROGRAM_AUDIO_CURL (curl),
# STRIH_PROGRAM_AUDIO_PORT (8891); `dpkg-query`, `apt-get`, `sudo`, `systemctl`, `id` and `chown` are
# looked up on PATH.

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
# rig-mode.sh event leaves this file in the operator's home; the unit's ExecCondition then skips every
# start (pinned against the template's ExecCondition by a test).
STRIH_PROGRAM_AUDIO_EVENT_MARKER=.config/camera-box/program-audio-sampler.event-mode
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
  echo "  enabled ${STRIH_PROGRAM_AUDIO_UNIT} (NOT started here: rig-mode.sh test starts it, event stops it)"
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

# strih_program_audio_endpoint_read PORT -> `<verdict> <age_s>` of http://127.0.0.1:PORT/program-audio.json,
# EMPTY when it does not answer a payload with a verdict and a source. Always rc 0.
strih_program_audio_endpoint_read() {
  local port="${1:?port required}" body
  body="$("${STRIH_PROGRAM_AUDIO_CURL:-curl}" -fsS --max-time 3 "http://127.0.0.1:${port}/program-audio.json" 2>/dev/null || true)"
  [ -n "$body" ] || return 0
  printf '%s' "$body" | python3 -c 'import json, sys
try:
    j = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
if isinstance(j, dict) and j.get("verdict") in ("MEASUREMENT", "FOREIGN", "SILENT", "UNKNOWN") and "source" in j:
    print(j["verdict"], j.get("age_s"))' 2>/dev/null || true
}

# strih_program_audio_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|text`, `NOTE|text`
# or `FAIL|text` (strih_program_audio_row_count of them). Always rc 0.
strih_program_audio_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local prefix unitdir rel st bad_files="" tmp ustate enabled shim active port answer marker
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

  active="$(strih_session_apps_user_systemctl "$user" is-active "$STRIH_PROGRAM_AUDIO_UNIT" 2>/dev/null || true)"
  active="${active%%$'\n'*}"
  port="$(strih_program_audio_port)"
  marker=no; [ -e "${home}/${STRIH_PROGRAM_AUDIO_EVENT_MARKER}" ] && marker=yes
  case "$active" in
    active)
      answer="$(strih_program_audio_endpoint_read "$port")"
      if [ -n "$answer" ]; then
        printf 'OK|(program-audio-endpoint) running; http://127.0.0.1:%s/program-audio.json answers verdict=%s age_s=%s\n' \
          "$port" "${answer%% *}" "${answer#* }"
      else
        printf 'FAIL|(program-audio-endpoint) running but http://127.0.0.1:%s/program-audio.json does not answer a verdict -- read journalctl --user -u %s\n' \
          "$port" "$STRIH_PROGRAM_AUDIO_UNIT"
      fi
      ;;
    failed)
      printf 'FAIL|(program-audio-endpoint) %s failed -- read journalctl --user -u %s\n' "$STRIH_PROGRAM_AUDIO_UNIT" "$STRIH_PROGRAM_AUDIO_UNIT"
      ;;
    *)
      if [ "$marker" = yes ]; then
        printf 'NOTE|(program-audio-endpoint) stopped: EVENT mode (~/%s present) -- rig-mode.sh test starts it\n' "$STRIH_PROGRAM_AUDIO_EVENT_MARKER"
      else
        printf 'NOTE|(program-audio-endpoint) not running (%s) and no EVENT marker -- in TEST mode run rig-mode.sh test (setup-strih never starts it)\n' "${active:-unreadable}"
      fi
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
