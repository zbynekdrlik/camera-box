#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih-tally constants, the setup-strih step 16f
# install and the verify-strih item 42 grader; no top-level statements besides constants) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. setup-strih.sh / verify-strih.sh set their own.
# Every function checks its own return codes.
#
# scripts/lib/strih-tally.sh -- issue 1417 (owner 12.10.2026): the camera tally lights driven by the
# strih OBS, on every box of the strih ROLE (strih-snv, strih-pp; never stream-pp -- the role gate is
# `obs_box_step_applies 16f` in scripts/lib/obs-box-role.sh). Spec:
# docs/superpowers/specs/2026-10-12-strih-tally-design.md.
#
#   * strih-tally.service (system unit, User = the operator, rendered from
#     systemd/strih-tally.service.in) runs scripts/strih_tally.py: a WebSocket relay on :9240 (the
#     WsTally protocol the ESP8266 firmware in tally/esp/ speaks) + an obs-websocket client to the local
#     strih OBS (the scene -> camera rule: scripts/strih_tally_decision.py), plus GET /state.
#   * strih-tally-mdns.service runs `avahi-publish -a -R atemrpi.local <STRIH_IP>`: the firmware
#     hard-codes that name. The address comes from /etc/default/strih-tally (STRIH_TALLY_IP = the box
#     fact STRIH_IP).
#
#   * strih_tally_install -- setup-strih step 16f (as root): the apt packages python3-websockets +
#     avahi-utils only when missing, an import preflight of the websockets asyncio API, the two python
#     files into /usr/local/lib/camera-box, both units and the default file -- each written only when
#     its bytes or mode differ (strih_session_apps_put) -- then a daemon-reload after a write, a
#     try-restart of a unit whose files changed (a running one moves onto them, a stopped one is not
#     started by it), and `enable --now` of a unit not yet enabled / `start` of an enabled one. Tally is
#     production function with no TEST/EVENT dependency, so this step is NOT enable-only.
#   * strih_tally_grade_rows / _report -- verify-strih item 42, 4 rows:
#       (tally-units) both units enabled + active, the installed files equal to the checkout;
#       (tally-port)  GET :9240/state answers with obs_connected=true;
#       (tally-mdns)  atemrpi.local resolves to exactly ONE address, this box's STRIH_IP (another device
#                     answering the name is named);
#       (tally-obs)   the service's program scene equals the strih OBS's own (GetCurrentProgramScene).
#
# Test seams (tests/python/test_strih_tally_install_1417.py): STRIH_TALLY_PREFIX
# (/usr/local/lib/camera-box), STRIH_TALLY_UNIT_DIR (/etc/systemd/system), STRIH_TALLY_DEFAULT_FILE
# (/etc/default/strih-tally), STRIH_TALLY_SYSTEMCTL (systemctl), STRIH_TALLY_PYTHON (/usr/bin/python3,
# the interpreter the unit runs), STRIH_TALLY_STATE_URL (http://127.0.0.1:9240/state),
# STRIH_TALLY_RESOLVE_CMD (avahi-resolve -4 -n atemrpi.local), STRIH_TALLY_OBS_PROGRAM_CMD (default:
# a websocket-client read of the strih OBS on :4455), STRIH_TALLY_RETRY_SLEEP (1 s); `dpkg-query` and
# `apt-get` are looked up on PATH.

# The write-if-changed installer lives in the session-apps lib.
if ! declare -F strih_session_apps_put >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi

STRIH_TALLY_UNIT="strih-tally.service"
STRIH_TALLY_MDNS_UNIT="strih-tally-mdns.service"
STRIH_TALLY_UNIT_TEMPLATE="strih-tally.service.in"
STRIH_TALLY_FILES=(scripts/strih_tally.py scripts/strih_tally_decision.py)
STRIH_TALLY_PACKAGES=(python3-websockets avahi-utils)
STRIH_TALLY_NAME="atemrpi.local"

strih_tally_prefix() { printf '%s' "${STRIH_TALLY_PREFIX:-/usr/local/lib/camera-box}"; }
strih_tally_unit_dir() { printf '%s' "${STRIH_TALLY_UNIT_DIR:-/etc/systemd/system}"; }
strih_tally_default_file() { printf '%s' "${STRIH_TALLY_DEFAULT_FILE:-/etc/default/strih-tally}"; }
strih_tally_systemctl() { "${STRIH_TALLY_SYSTEMCTL:-systemctl}" "$@"; }

# strih_tally_unit_text REPO USER -> the tally unit as installed: the template with its ONE
# `User=@USER@` line filled in. rc 1 + a stderr line when the template is missing or malformed.
strih_tally_unit_text() {
  local repo="${1:?repo root required}" user="${2:?user required}" tpl n
  tpl="${repo}/systemd/${STRIH_TALLY_UNIT_TEMPLATE}"
  [ -f "$tpl" ] || { echo "strih-tally: ${tpl} not found" >&2; return 1; }
  n="$(grep -c '^User=@USER@$' "$tpl" || true)"
  [ "$n" = 1 ] || { echo "strih-tally: ${tpl} must hold exactly one User=@USER@ line (found ${n})" >&2; return 1; }
  [[ "$user" =~ ^[a-z_][a-z0-9_-]*$ ]] || { echo "strih-tally: '${user}' is not a user name" >&2; return 1; }
  sed "s/^User=@USER@\$/User=${user}/" "$tpl"
}

# strih_tally_default_text -> the mDNS unit's EnvironmentFile: the loaded box's STRIH_IP.
strih_tally_default_text() {
  local ip
  ip="$(strih_lx_ip)" || return 1
  [[ "$ip" =~ ^[0123456789]{1,3}(\.[0123456789]{1,3}){3}$ ]] \
    || { echo "strih-tally: STRIH_IP '${ip}' is not an IPv4 address" >&2; return 1; }
  printf 'STRIH_TALLY_IP=%s\n' "$ip"
}

strih_tally_missing_packages() {
  local p
  for p in "${STRIH_TALLY_PACKAGES[@]}"; do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] || printf '%s\n' "$p"
  done
  return 0
}

# _strih_tally_put_text TEXT DST -> strih_session_apps_put of TEXT (0644); prints written / unchanged.
_strih_tally_put_text() {
  local text="$1" dst="$2" tmp w
  tmp="$(mktemp)" || { echo "strih-tally: mktemp failed" >&2; return 1; }
  printf '%s\n' "$text" > "$tmp" || { rm -f "$tmp"; echo "strih-tally: cannot write ${tmp}" >&2; return 1; }
  w="$(strih_session_apps_put "$tmp" "$dst" 0644)" || { rm -f "$tmp"; return 1; }
  rm -f "$tmp"
  printf '%s' "$w"
}

# --- setup-strih step 16f -------------------------------------------------------------------------------

# strih_tally_install REPO DESKTOP_USER -> see the header. One line per action. rc 1 + a stderr line on
# a missing source, a package that will not install, a python without the websockets asyncio API, a
# failed write or a failed systemctl call.
strih_tally_install() {
  local repo="${1:?repo root required}" user="${2:?desktop user required}"
  local prefix unitdir deffile rel w missing text tally_changed=0 mdns_changed=0 u
  local -a restart=() to_enable=() to_start=()
  prefix="$(strih_tally_prefix)"
  unitdir="$(strih_tally_unit_dir)"
  deffile="$(strih_tally_default_file)"
  for rel in "${STRIH_TALLY_FILES[@]}" "systemd/${STRIH_TALLY_MDNS_UNIT}" "systemd/${STRIH_TALLY_UNIT_TEMPLATE}"; do
    [ -f "${repo}/${rel}" ] || { echo "strih-tally: ${rel} not found under ${repo}" >&2; return 1; }
  done

  missing="$(strih_tally_missing_packages)"
  if [ -n "$missing" ]; then
    echo "  installing ${missing//$'\n'/ }"
    # shellcheck disable=SC2086  # one word per package name
    DEBIAN_FRONTEND=noninteractive apt-get install -y $missing || {
      echo "strih-tally: apt-get install ${missing//$'\n'/ } failed" >&2; return 1; }
  fi
  "${STRIH_TALLY_PYTHON:-/usr/bin/python3}" -c 'import websockets.asyncio.server, websockets.asyncio.client' || {
    echo "strih-tally: ${STRIH_TALLY_PYTHON:-/usr/bin/python3} cannot import the websockets asyncio API (package python3-websockets)" >&2
    return 1; }
  echo "  websockets asyncio API importable (${STRIH_TALLY_PACKAGES[*]})"

  install -d -m 0755 "$prefix" "$unitdir" "$(dirname "$deffile")" || {
    echo "strih-tally: cannot create ${prefix} / ${unitdir} / $(dirname "$deffile")" >&2; return 1; }
  for rel in "${STRIH_TALLY_FILES[@]}"; do
    w="$(strih_session_apps_put "${repo}/${rel}" "${prefix}/$(basename "$rel")" 0644)" || return 1
    echo "  ${prefix}/$(basename "$rel") ${w}"
    [ "$w" = written ] && tally_changed=1
  done
  text="$(strih_tally_unit_text "$repo" "$user")" || return 1
  w="$(_strih_tally_put_text "$text" "${unitdir}/${STRIH_TALLY_UNIT}")" || return 1
  echo "  ${unitdir}/${STRIH_TALLY_UNIT} ${w} (User=${user})"
  [ "$w" = written ] && tally_changed=1
  w="$(strih_session_apps_put "${repo}/systemd/${STRIH_TALLY_MDNS_UNIT}" "${unitdir}/${STRIH_TALLY_MDNS_UNIT}" 0644)" || return 1
  echo "  ${unitdir}/${STRIH_TALLY_MDNS_UNIT} ${w}"
  [ "$w" = written ] && mdns_changed=1
  text="$(strih_tally_default_text)" || return 1
  w="$(_strih_tally_put_text "$text" "$deffile")" || return 1
  echo "  ${deffile} ${w} (${text})"
  [ "$w" = written ] && mdns_changed=1

  if [ "$tally_changed" = 1 ] || [ "$mdns_changed" = 1 ]; then
    strih_tally_systemctl daemon-reload || { echo "strih-tally: systemctl daemon-reload failed" >&2; return 1; }
    [ "$tally_changed" = 1 ] && restart+=("$STRIH_TALLY_UNIT")
    [ "$mdns_changed" = 1 ] && restart+=("$STRIH_TALLY_MDNS_UNIT")
    # only a RUNNING unit is restarted onto the new files; a stopped one is started below
    strih_tally_systemctl try-restart "${restart[@]}" || {
      echo "strih-tally: systemctl try-restart ${restart[*]} failed" >&2; return 1; }
    echo "  try-restart ${restart[*]}"
  fi
  for u in "$STRIH_TALLY_UNIT" "$STRIH_TALLY_MDNS_UNIT"; do
    if [ "$(strih_tally_systemctl is-enabled "$u" 2>/dev/null || true)" = enabled ]; then
      to_start+=("$u")
    else
      to_enable+=("$u")
    fi
  done
  if [ "${#to_enable[@]}" -gt 0 ]; then
    strih_tally_systemctl enable --now "${to_enable[@]}" || {
      echo "strih-tally: systemctl enable --now ${to_enable[*]} failed" >&2; return 1; }
    echo "  enabled + started ${to_enable[*]}"
  fi
  if [ "${#to_start[@]}" -gt 0 ]; then
    # a no-op on a running unit: tally runs always, so a stopped one is started
    strih_tally_systemctl start "${to_start[@]}" || {
      echo "strih-tally: systemctl start ${to_start[*]} failed" >&2; return 1; }
    echo "  running ${to_start[*]}"
  fi
  return 0
}

# --- verify-strih item 42 -------------------------------------------------------------------------------

# strih_tally_state_read URL -> two lines from GET URL: `<obs_connected> <pgm> <pvw> <tallies>` and the
# program scene (empty when null); EMPTY when it does not answer a JSON object. Always rc 0.
strih_tally_state_read() {
  local url="${1:?url required}"
  python3 -c 'import json, sys, urllib.request
try:
    with urllib.request.urlopen(sys.argv[1], timeout=3) as r:
        j = json.loads(r.read().decode("utf-8", "replace"))
except Exception:
    sys.exit(0)
if not isinstance(j, dict):
    sys.exit(0)
t = j.get("tallies")
print("true" if j.get("obs_connected") is True else "false", j.get("pgm"), j.get("pvw"),
      len(t) if isinstance(t, list) else 0)
print(str(j.get("program_scene") or "").replace("\n", " "))' "$url" 2>/dev/null || true
}

# strih_tally_resolve -> the addresses the resolver answers for atemrpi.local, one per line. Always rc 0.
strih_tally_resolve() {
  local -a cmd
  read -r -a cmd <<<"${STRIH_TALLY_RESOLVE_CMD:-avahi-resolve -4 -n ${STRIH_TALLY_NAME}}"
  { timeout 8 "${cmd[@]}" 2>/dev/null || true; } | awk 'NF >= 2 { print $2 }'
}

# strih_tally_obs_program -> the strih OBS's own program scene name, EMPTY when OBS does not answer.
# Always rc 0.
strih_tally_obs_program() {
  local -a cmd
  if [ -n "${STRIH_TALLY_OBS_PROGRAM_CMD:-}" ]; then
    read -r -a cmd <<<"$STRIH_TALLY_OBS_PROGRAM_CMD"
    timeout 10 "${cmd[@]}" 2>/dev/null | head -n 1 || true
    return 0
  fi
  timeout 10 python3 -c 'import json, sys
from websocket import create_connection
ws = create_connection(sys.argv[1], timeout=5)
json.loads(ws.recv())
ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}))
json.loads(ws.recv())
ws.send(json.dumps({"op": 6, "d": {"requestType": "GetCurrentProgramScene", "requestId": "v42"}}))
while True:
    m = json.loads(ws.recv())
    if m.get("op") == 7 and (m.get("d") or {}).get("requestId") == "v42":
        r = m["d"].get("responseData") or {}
        print(r.get("currentProgramSceneName") or r.get("sceneName") or "")
        break
ws.close()' ws://127.0.0.1:4455 2>/dev/null || true
}

# strih_tally_grade_rows REPO DESKTOP_USER -> verify-strih rows `OK|text` or `FAIL|text`
# (strih_tally_row_count of them). Always rc 0.
strih_tally_grade_rows() {
  local repo="${1:?repo root required}" user="${2:?desktop user required}"
  local prefix unitdir deffile rel st bad_files="" tmp u en act units_ok=1 states="" ip url state head scene
  local obs_program addrs n i
  prefix="$(strih_tally_prefix)"
  unitdir="$(strih_tally_unit_dir)"
  deffile="$(strih_tally_default_file)"
  for rel in "${STRIH_TALLY_FILES[@]}"; do
    st="$(strih_session_app_file_state "${prefix}/$(basename "$rel")" "${repo}/${rel}")"
    [ "$st" = ok ] || bad_files="${bad_files} $(basename "$rel")=${st}"
  done
  tmp="$(mktemp)"
  if strih_tally_unit_text "$repo" "$user" > "$tmp" 2>/dev/null; then
    st="$(strih_session_app_file_state "${unitdir}/${STRIH_TALLY_UNIT}" "$tmp")"
  else
    st=template-broken
  fi
  [ "$st" = ok ] || bad_files="${bad_files} ${STRIH_TALLY_UNIT}=${st}"
  st="$(strih_session_app_file_state "${unitdir}/${STRIH_TALLY_MDNS_UNIT}" "${repo}/systemd/${STRIH_TALLY_MDNS_UNIT}")"
  [ "$st" = ok ] || bad_files="${bad_files} ${STRIH_TALLY_MDNS_UNIT}=${st}"
  if strih_tally_default_text > "$tmp" 2>/dev/null; then
    st="$(strih_session_app_file_state "$deffile" "$tmp")"
  else
    st=fact-unreadable
  fi
  [ "$st" = ok ] || bad_files="${bad_files} $(basename "$deffile")=${st}"
  rm -f "$tmp"
  for u in "$STRIH_TALLY_UNIT" "$STRIH_TALLY_MDNS_UNIT"; do
    en="$(strih_tally_systemctl is-enabled "$u" 2>/dev/null | head -n 1 || true)"
    act="$(strih_tally_systemctl is-active "$u" 2>/dev/null | head -n 1 || true)"
    states="${states} ${u}=${en:-unreadable}/${act:-unreadable}"
    { [ "$en" = enabled ] && [ "$act" = active ]; } || units_ok=0
  done
  if [ -z "$bad_files" ] && [ "$units_ok" = 1 ]; then
    printf 'OK|(tally-units) %s + %s enabled + active, the installed files match the checkout\n' \
      "$STRIH_TALLY_UNIT" "$STRIH_TALLY_MDNS_UNIT"
  else
    printf 'FAIL|(tally-units)%s; files:%s -- re-run setup-strih.sh step 16f (issue 1417), journalctl -u %s -u %s\n' \
      "$states" "${bad_files:- ok}" "$STRIH_TALLY_UNIT" "$STRIH_TALLY_MDNS_UNIT"
  fi

  url="${STRIH_TALLY_STATE_URL:-http://127.0.0.1:9240/state}"
  state="$(strih_tally_state_read "$url")"
  head="${state%%$'\n'*}"
  scene=""
  [[ "$state" == *$'\n'* ]] && scene="${state#*$'\n'}"
  if [ -z "$state" ]; then
    printf 'FAIL|(tally-port) %s does not answer -- is %s running? journalctl -u %s\n' "$url" "$STRIH_TALLY_UNIT" "$STRIH_TALLY_UNIT"
  else
    read -r -a i <<<"$head"
    if [ "${i[0]:-false}" = true ]; then
      printf 'OK|(tally-port) %s answers: pgm=%s pvw=%s, OBS connected, %s tally light(s) joined\n' \
        "$url" "${i[1]:-?}" "${i[2]:-?}" "${i[3]:-0}"
    else
      printf 'FAIL|(tally-port) %s answers but obs_connected=false: the service cannot reach the strih OBS on :4455 (OBS down, or a WS password) -- journalctl -u %s\n' \
        "$url" "$STRIH_TALLY_UNIT"
    fi
  fi

  ip="$(strih_lx_ip 2>/dev/null || true)"
  addrs="$(strih_tally_resolve)"
  n="$(printf '%s' "$addrs" | grep -c . || true)"
  if [ "$n" = 0 ]; then
    printf 'FAIL|(tally-mdns) %s does not resolve -- is %s running? (avahi-daemon up? journalctl -u %s)\n' \
      "$STRIH_TALLY_NAME" "$STRIH_TALLY_MDNS_UNIT" "$STRIH_TALLY_MDNS_UNIT"
  elif [ "$n" = 1 ] && [ "$addrs" = "$ip" ]; then
    printf 'OK|(tally-mdns) %s resolves to this box only (%s)\n' "$STRIH_TALLY_NAME" "$ip"
  else
    printf 'FAIL|(tally-mdns) %s resolves to %s, not only this box (%s): another device on the venue LAN answers the name the tally firmware dials -- rename or unplug it\n' \
      "$STRIH_TALLY_NAME" "${addrs//$'\n'/ + }" "${ip:-STRIH_IP unreadable}"
  fi

  if [ -z "$state" ]; then
    printf 'FAIL|(tally-obs) not graded: %s does not answer\n' "$url"
    return 0
  fi
  obs_program="$(strih_tally_obs_program)"
  if [ -n "$obs_program" ] && [ "$obs_program" != "$scene" ]; then
    # a cut between the two reads: read both once more
    sleep "${STRIH_TALLY_RETRY_SLEEP:-1}"
    state="$(strih_tally_state_read "$url")"
    scene=""
    [[ "$state" == *$'\n'* ]] && scene="${state#*$'\n'}"
    obs_program="$(strih_tally_obs_program)"
  fi
  if [ -z "$obs_program" ]; then
    printf 'FAIL|(tally-obs) the strih OBS did not answer GetCurrentProgramScene on :4455 -- the tally program cannot be checked\n'
  elif [ "$obs_program" = "$scene" ]; then
    printf "OK|(tally-obs) the service's program scene '%s' = the strih OBS program scene\n" "$scene"
  else
    printf "FAIL|(tally-obs) the service reads program '%s' but the strih OBS program is '%s' -- the tally lights show the wrong camera; journalctl -u %s\n" \
      "$scene" "$obs_program" "$STRIH_TALLY_UNIT"
  fi
  return 0
}

strih_tally_row_count() { printf '4'; }

# strih_tally_grade_report REPO DESKTOP_USER -> print the rows through the CALLER's ok / note / bad
# functions (verify-strih). rc 1 unless the grader printed every row.
strih_tally_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_tally_grade_rows "$@")
  [ "$rows" = "$(strih_tally_row_count)" ]
}
