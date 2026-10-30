#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
# setup-strih.sh / verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/strih-pipewire-purge.sh -- issue 1440 (phase 4 of the issue-1437 plan, design 6379965724,
# Approach 1): PipeWire OUT of the strih role, on a box whose facts say nothing needs it. The owner
# blacklisted PipeWire on the strih role (comment 6337295922); on strih-snv every path that used it is gone
# (the program is obs-vban, issue 1439; the cutters' card is the hub's ALSA leg, issue 1438; every hub
# crossing is the resampler, issue 1461).
#
#   * strih_pipewire_purge_decision ROLE STREAMS TOML -- the DERIVED condition (pure but for reading TOML):
#     strih role AND a direct program VBAN (STRIH_PROGRAM_VBAN_STREAMS != none) AND no `adapter =
#     "pipewire"` participant in the box's hub routing. strih-snv + strih-pp = purge (strih-pp since its
#     lv1 sends `strih-obs` straight into OBS, issue 1439, 24.10.2026); a hub-fed strih (program via a
#     hub `program_out` -> pw-cat -> PipeWire) = keep; the stream role = keep (its LOCKED operator
#     PipeWire, issue 1465). No new fact, no box name.
#   * strih_pipewire_purge_install REPO USER_HOME DESKTOP_USER -- setup-strih step 12 on such a box:
#       1. the apt pin (Pin-Priority -1) on every PipeWire / WirePlumber / pulseaudio server package, for
#          EVERY architecture dpkg knows on the box: a bare name pins the native arch only (live
#          strih-snv 24.10.2026: `pulseaudio:i386` + 132 i386 packages, or `pipewire-pulse:i386`, came in);
#       2. the shim `camera-box-pipewire-shim` (`Provides: gstreamer1.0-pipewire`, so RustDesk 1.4.9's
#          dependency holds), built ON the box with dpkg-deb from the checked-in control file
#          scripts/strih-pipewire/camera-box-pipewire-shim.control (equivs would pull 22 build packages);
#       3. libpulse0 + libpipewire-0.3-0t64 marked manual (OBS links them; apt does not know OBS);
#       4. an `apt-get -s purge` read FIRST: the purge REFUSES when it would remove anything outside the
#          declared list (STRIH_PW_PURGE_SERVER_PACKAGES + STRIH_PW_PURGE_COLLATERAL) or install anything;
#       5. the purge, then a read-back (no server package left, the kept libraries still there);
#       6. the strih-role PipeWire / WirePlumber drop-ins removed (the SAME list the stream role removes,
#          STREAM_OPERATOR_AUDIO_LEFTOVERS).
#     Idempotent: a re-run finds the pin unchanged, the shim installed, nothing to purge. It never stops a
#     running PipeWire: the operator's processes keep running until the session restarts (a reboot).
#   * strih_pipewire_purge_grade_rows / _report REPO USER_HOME USER -- verify-strih item 9b's rows: on a
#     purge box packages + pin + shim, no operator PipeWire process, RustDesk active, the card PCMs held by
#     the hub's aplay / arecord, the drop-ins, the browser sources; on any other box ONE NOTE naming why
#     not purged (+ the browser-source row on the strih role).
#
# Seams (tests/python/test_strih_pipewire_purge_1440.py): STRIH_PW_PURGE_PREF (the pin file),
# STRIH_PW_PURGE_OWNER (its owner, root:root), STRIH_PW_PURGE_TOML (the hub routing file instead of the
# box fact's), STRIH_PW_PURGE_ASOUND (/proc/asound), STRIH_PW_PURGE_PROC (/proc), STRIH_PW_PURGE_SCENES
# (strih_scenes.py), STRIH_PW_PURGE_HUB_DROPIN (the hub's 10-local-audio.conf). apt-get, apt-mark,
# dpkg, dpkg-query, dpkg-deb, pgrep and systemctl are looked up on PATH.

# The sound-server packages: purged when installed, pinned out for every architecture.
STRIH_PW_PURGE_SERVER_PACKAGES=(pipewire pipewire-bin pipewire-pulse pipewire-alsa pipewire-audio pipewire-jack
  wireplumber pulseaudio gstreamer1.0-pipewire)
# What the purge may take along (the 22.10.2026 inventory, comment 6337559249): nothing runs them, no meta
# package needs them. Anything else in the `apt-get -s` read refuses the purge.
STRIH_PW_PURGE_COLLATERAL=(gnome-control-center gnome-initial-setup gnome-settings-daemon gnome-snapshot
  libcanberra-pulse)
# The libraries OBS links (not dpkg-known to apt as OBS's): kept, marked manual.
STRIH_PW_PURGE_KEEP=(libpulse0 libpipewire-0.3-0t64)
STRIH_PW_PURGE_SHIM=camera-box-pipewire-shim
# How many rows strih_pipewire_purge_grade_rows prints on a purge box.
# shellcheck disable=SC2034  # read by the tests
STRIH_PW_PURGE_ROWS=6

_strih_pw_purge_pref() { printf '%s' "${STRIH_PW_PURGE_PREF:-/etc/apt/preferences.d/camera-box-no-pipewire.pref}"; }

# _strih_pw_purge_toml_participants TOML -> one `name<TAB>adapter<TAB>card<TAB>device<TAB>out<TAB>in` line per
# [[participant]] (empty fields when absent); rc 2 + a stderr line when the file cannot be read as TOML.
_strih_pw_purge_toml_participants() {
  python3 - "${1-}" <<'PY'
import sys
try:
    import tomllib
    with open(sys.argv[1], "rb") as fh:
        doc = tomllib.load(fh)
except Exception as e:  # noqa: BLE001 -- any read / parse failure: the condition cannot be decided
    print("strih-pipewire-purge: cannot read the hub routing %s (%s)" % (sys.argv[1], e), file=sys.stderr)
    sys.exit(2)
for p in doc.get("participant") or []:
    if isinstance(p, dict):
        print("\t".join(str(p.get(k, "")) for k in
                        ("name", "adapter", "alsa_card", "alsa_device", "out_channels", "in_channels")))
PY
}

# strih_pipewire_purge_decision ROLE STREAMS TOML -> `purge|<why>` (rc 0) / `keep|<why>` (rc 1) /
# `unknown|<why>` (rc 2) on stdout. TOML is read only on the strih role with a declared program VBAN.
strih_pipewire_purge_decision() {
  local role="${1-}" streams="${2-}" toml="${3-}" rows rc=0 pw="" name adapter rest
  case "$role" in
    stream) printf 'keep|the stream role keeps its locked operator PipeWire for the headphones (issue 1465)\n'; return 1 ;;
    strih) ;;
    *) printf 'unknown|the box role %s is neither strih nor stream\n' "${role:-<unread>}"; return 2 ;;
  esac
  case "$streams" in
    "") printf 'unknown|STRIH_PROGRAM_VBAN_STREAMS could not be read\n'; return 2 ;;
    none) printf 'keep|no direct program VBAN on this box (STRIH_PROGRAM_VBAN_STREAMS=none): its program reaches OBS through the hub'"'"'s adapter = "pipewire" program_out, so PipeWire stays until its console sends the program as VBAN\n'; return 1 ;;
  esac
  rows="$(_strih_pw_purge_toml_participants "$toml" 2>&1)" || rc=$?
  if [ "$rc" != 0 ]; then
    printf 'unknown|%s\n' "${rows##*$'\n'}"
    return 2
  fi
  while IFS=$'\t' read -r name adapter rest; do
    [ "$adapter" = pipewire ] && pw="${pw}${pw:+, }${name}"
  done <<<"$rows"
  if [ -n "$pw" ]; then
    printf 'keep|the hub routing %s still has adapter = "pipewire" participant(s): %s\n' "${toml##*/}" "$pw"
    return 1
  fi
  printf 'purge|strih role, the console program as VBAN (STRIH_PROGRAM_VBAN_STREAMS=%s) and no pipewire participant in %s: nothing on this box needs PipeWire (issue 1440)\n' \
    "$streams" "${toml##*/}"
  return 0
}

# _strih_pw_purge_toml -> the loaded box's hub routing file (the fact STRIH_INTERCOM_CONFIG, repo-relative).
_strih_pw_purge_toml() {
  local rel
  if [ -n "${STRIH_PW_PURGE_TOML:-}" ]; then printf '%s' "$STRIH_PW_PURGE_TOML"; return 0; fi
  rel="$(strih_lx_intercom_config 2>/dev/null)" || return 1
  printf '%s/../../%s' "$(dirname "${BASH_SOURCE[0]}")" "$rel"
}

# strih_pipewire_purge_why -> the decision line of the LOADED box (see strih_pipewire_purge_decision).
strih_pipewire_purge_why() {
  local role="" streams="" toml=""
  role="$(strih_box_role 2>/dev/null)" || role=""
  streams="$(strih_lx_program_vban_streams 2>/dev/null)" || streams=""
  toml="$(_strih_pw_purge_toml)" || toml=""
  strih_pipewire_purge_decision "$role" "$streams" "$toml"
}

# strih_pipewire_purge_applies -> rc 0 + the reason when the loaded box purges PipeWire; rc 1 + the reason
# when it keeps it; rc 2 + the reason (also on stderr) when it cannot be decided.
strih_pipewire_purge_applies() {
  local line rc=0
  line="$(strih_pipewire_purge_why)" || rc=$?
  printf '%s\n' "${line#*|}"
  [ "$rc" = 2 ] && echo "WARNING strih-pipewire-purge: ${line#*|}" >&2
  return "$rc"
}

# strih_pipewire_purge_dropin_mode -> `no-pipewire` on a purge box, else `pipewire` (strih_intercom_audio_dropin).
strih_pipewire_purge_dropin_mode() {
  if strih_pipewire_purge_applies >/dev/null 2>&1; then printf 'no-pipewire'; else printf 'pipewire'; fi
}

# strih_intercom_audio_dropin_no_pipewire USER -> the hub's 10-local-audio.conf on a box without PipeWire
# (strih_intercom_audio_dropin USER UID no-pipewire): still the operator user, for the HDMI leg's xrandr (it
# reads the operator's ~/.Xauthority) and the card leg's aplay / arecord (group audio); no pipewire group (it
# does not exist on a fresh box) and no PipeWire runtime env or user-session ordering. Pure.
strih_intercom_audio_dropin_no_pipewire() {
  local user="${1:?strih_intercom_audio_dropin_no_pipewire needs the operator USER}"
  cat <<NOPWDROPIN
# intercom-hub local-audio override (issue 1344; issue 1440: no PipeWire on this box) — installed by
# setup-strih step 13. The hub runs as the operator: the card leg opens the hw PCMs with aplay / arecord
# (group audio), the HDMI leg reads the X screen through the operator's ~/.Xauthority (group render).
# No sound server runs here, so no pipewire group and no PipeWire runtime. Do NOT edit by hand.
[Service]
DynamicUser=no
User=${user}
Group=${user}
SupplementaryGroups=audio render
# ProtectHome=read-only keeps ~/.Xauthority readable for the HDMI leg; the sensitive dirs are blanked
# (\`-\` = tolerate absence).
ProtectHome=read-only
InaccessiblePaths=-/home/${user}/.ssh -/home/${user}/.gnupg -/home/${user}/.config/gh
NOPWDROPIN
}

# strih_pipewire_purge_archs -> every architecture dpkg knows on the box, native first, one per line.
strih_pipewire_purge_archs() {
  local native foreign a
  native="$(dpkg --print-architecture 2>/dev/null)" || native=""
  [ -n "$native" ] || { echo "strih-pipewire-purge: dpkg --print-architecture answered nothing" >&2; return 1; }
  printf '%s\n' "$native"
  foreign="$(dpkg --print-foreign-architectures 2>/dev/null)" || foreign=""
  while IFS= read -r a; do
    [ -n "$a" ] && [ "$a" != "$native" ] && printf '%s\n' "$a"
  done <<<"$foreign"
  return 0
}

# strih_pipewire_purge_pin_text ARCH... -> the apt preferences file: every server package for every ARCH at
# Pin-Priority -1 (never installable). Pure.
strih_pipewire_purge_pin_text() {
  local p a list=""
  [ "$#" -gt 0 ] || { echo "strih-pipewire-purge: the pin needs at least one architecture" >&2; return 1; }
  for p in "${STRIH_PW_PURGE_SERVER_PACKAGES[@]}"; do
    for a in "$@"; do list="${list}${list:+ }${p}:${a}"; done
  done
  printf 'Explanation: camera-box issue 1440 -- no sound server on the strih role (setup-strih step 12; do not edit by hand)\n'
  printf 'Explanation: libpulse0 and libpipewire-0.3-0t64 stay (OBS links them); camera-box-pipewire-shim provides gstreamer1.0-pipewire for RustDesk\n'
  printf 'Package: %s\n' "$list"
  printf 'Pin: release *\n'
  printf 'Pin-Priority: -1\n'
}

# _strih_pw_purge_in_list NAME LIST... -> rc 0 when NAME (an arch suffix dropped) is in LIST.
_strih_pw_purge_in_list() {
  local n="${1%%:*}" x
  shift
  for x in "$@"; do [ "$n" = "$x" ] && return 0; done
  return 1
}

# strih_pipewire_purge_sim_verdict SIMTEXT -> `ok <N> package(s) to remove` (rc 0) or `refuse <why>` (rc 1)
# for the `apt-get -s purge` text: refuses any Inst / Conf line, any `E:` line, any removal of a package
# outside the declared list, and a read with no removal line at all. Pure.
strih_pipewire_purge_sim_verdict() {
  local text="${1-}" line pkg n=0 extra="" inst="" err=""
  while IFS= read -r line; do
    case "$line" in
      E:*) err="${err}${err:+; }${line}" ;;
      "Inst "* | "Conf "*) [ -n "$inst" ] || read -r _ inst _ <<<"$line" ;;
      "Purg "* | "Remv "*)
        read -r _ pkg _ <<<"$line"
        n=$((n + 1))
        if ! _strih_pw_purge_in_list "$pkg" "${STRIH_PW_PURGE_SERVER_PACKAGES[@]}" "${STRIH_PW_PURGE_COLLATERAL[@]}"; then
          extra="${extra}${extra:+, }${pkg}"
        fi ;;
    esac
  done <<<"$text"
  if [ -n "$err" ]; then printf 'refuse apt-get -s failed: %s\n' "$err"; return 1; fi
  if [ -n "$inst" ]; then printf 'refuse the purge would INSTALL packages (first: %s) -- a pin is missing\n' "$inst"; return 1; fi
  if [ -n "$extra" ]; then printf 'refuse the purge would also remove %s, outside the declared list\n' "$extra"; return 1; fi
  if [ "$n" = 0 ]; then printf 'refuse the apt-get -s read lists nothing to remove\n'; return 1; fi
  printf 'ok %s package(s) to remove\n' "$n"
}

# _strih_pw_purge_installed NAME... -> the installed instances (`name` or `name:arch`, as dpkg prints them),
# one per line. Any dpkg status but not-installed / config-files counts. Always rc 0.
_strih_pw_purge_installed() { _strih_pw_purge_status_list installed "$@"; }

# _strih_pw_purge_present NAME... -> as _strih_pw_purge_installed, plus the ones left in config-files state
# (a purge removes their configuration too: review round 1). Always rc 0.
_strih_pw_purge_present() { _strih_pw_purge_status_list present "$@"; }

# _strih_pw_purge_status_list installed|present NAME... -> the dpkg instances of NAME... in that class.
_strih_pw_purge_status_list() {
  local class="$1" out pkg st _v
  shift
  out="$(dpkg-query -W -f='${binary:Package}\t${db:Status-Status}\t${Version}\n' "$@" 2>/dev/null)" || true
  while IFS=$'\t' read -r pkg st _v; do
    [ -n "$pkg" ] || continue
    case "$st" in
      not-installed | "") ;;
      config-files) [ "$class" = present ] && printf '%s\n' "$pkg" ;;
      *) printf '%s\n' "$pkg" ;;
    esac
  done <<<"$out"
  return 0
}

# _strih_pw_purge_shim_version CONTROL -> the Version field of the shim control file.
_strih_pw_purge_shim_version() { sed -n '/^Version:/{s/^Version:[[:space:]]*//p;q;}' "${1-}" 2>/dev/null; }

# strih_pipewire_purge_shim_install REPO -> builds + installs the shim when it is not installed at the
# control file's version. The built file goes in through apt-get (review round 1: a bare `dpkg -i` ignores
# the DPkg::Lock::Timeout wait obs_box_apt_lock_timeout wrote, and failed at once on a held lock). rc 1 + a
# stderr line on a failure.
strih_pipewire_purge_shim_install() {
  local ctl="${1:?repo root required}/scripts/strih-pipewire/${STRIH_PW_PURGE_SHIM}.control" want have dir out
  [ -f "$ctl" ] || { echo "strih-pipewire-purge: ${ctl} not found (stage scripts/ with the setup run)" >&2; return 1; }
  want="$(_strih_pw_purge_shim_version "$ctl")"
  [ -n "$want" ] || { echo "strih-pipewire-purge: ${ctl} carries no Version" >&2; return 1; }
  have="$(dpkg-query -W -f='${db:Status-Status} ${Version}' "$STRIH_PW_PURGE_SHIM" 2>/dev/null)" || have=""
  if [ "$have" = "installed ${want}" ]; then
    echo "  ${STRIH_PW_PURGE_SHIM} ${want}: installed -- unchanged"
    return 0
  fi
  dir="$(mktemp -d)" || { echo "strih-pipewire-purge: mktemp failed" >&2; return 1; }
  # 0755: apt's unprivileged _apt user reads a local .deb (a 0700 dir makes apt warn it ran unsandboxed)
  chmod 0755 "$dir" || { rm -rf "$dir"; echo "strih-pipewire-purge: chmod ${dir} failed" >&2; return 1; }
  out="${dir}/${STRIH_PW_PURGE_SHIM}_${want}_all.deb"
  if ! { install -d -m 0755 "${dir}/pkg/DEBIAN" && install -m 0644 "$ctl" "${dir}/pkg/DEBIAN/control" \
      && dpkg-deb --build --root-owner-group "${dir}/pkg" "$out" >/dev/null \
      && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-remove "$out" >/dev/null; }; then
    rm -rf "$dir"
    echo "strih-pipewire-purge: building / installing ${STRIH_PW_PURGE_SHIM} ${want} failed (above)" >&2
    return 1
  fi
  rm -rf "$dir"
  echo "  ${STRIH_PW_PURGE_SHIM} ${want}: built with dpkg-deb + installed (Provides: gstreamer1.0-pipewire, RustDesk's dependency)"
}

# _strih_pw_purge_qualified NAME NATIVE -> NAME:<arch>; a bare name (how apt prints a native-arch package) gets
# NATIVE. dpkg-query prints `libpulse0:amd64`, apt-mark showmanual `libpulse0`: both read `libpulse0:amd64`.
_strih_pw_purge_qualified() {
  case "$1" in *:*) printf '%s' "$1" ;; *) printf '%s:%s' "$1" "$2" ;; esac
}

# strih_pipewire_purge_install REPO USER_HOME DESKTOP_USER -> see the header. rc 0 / 1 (+ a stderr line).
strih_pipewire_purge_install() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local pref archs sim verdict rel f m left kept_before kept_after manual found
  local -a arch targets keep_inst unmarked=()
  pref="$(_strih_pw_purge_pref)"
  archs="$(strih_pipewire_purge_archs)" || return 1
  mapfile -t arch <<<"$archs"
  install -d -m 0755 "$(dirname "$pref")" || { echo "strih-pipewire-purge: cannot create $(dirname "$pref")" >&2; return 1; }
  strih_pipewire_purge_pin_text "${arch[@]}" \
    | obs_box_write_if_changed "$pref" 0644 "${STRIH_PW_PURGE_OWNER:-root:root}" "apt pin (no PipeWire / pulseaudio, ${arch[*]})" \
    || return 1
  strih_pipewire_purge_shim_install "$repo" || return 1
  mapfile -t keep_inst < <(_strih_pw_purge_installed "${STRIH_PW_PURGE_KEEP[@]}")
  kept_before="${keep_inst[*]}"
  if [ "${#keep_inst[@]}" -gt 0 ] && [ -n "${keep_inst[0]}" ]; then
    manual="$(apt-mark showmanual "${keep_inst[@]}" 2>/dev/null)" || manual=""
    for f in "${keep_inst[@]}"; do
      found=0
      while IFS= read -r m; do
        if [ -n "$m" ] && [ "$(_strih_pw_purge_qualified "$m" "${arch[0]}")" = "$(_strih_pw_purge_qualified "$f" "${arch[0]}")" ]; then found=1; fi
      done <<<"$manual"
      [ "$found" = 1 ] || unmarked+=("$f")
    done
    if [ "${#unmarked[@]}" -gt 0 ]; then
      apt-mark manual "${unmarked[@]}" >/dev/null || { echo "strih-pipewire-purge: apt-mark manual ${unmarked[*]} failed" >&2; return 1; }
      echo "  marked manual: ${unmarked[*]} (OBS links them)"
    fi
  fi
  mapfile -t targets < <(_strih_pw_purge_present "${STRIH_PW_PURGE_SERVER_PACKAGES[@]}")
  if [ "${#targets[@]}" -eq 0 ] || [ -z "${targets[0]}" ]; then
    echo "  no PipeWire / WirePlumber / pulseaudio server package installed -- nothing to purge"
  else
    sim="$(LC_ALL=C apt-get -s purge "${targets[@]}" 2>&1)" || sim="${sim}"$'\n'"E: apt-get -s exited non-zero"
    if ! verdict="$(strih_pipewire_purge_sim_verdict "$sim")"; then
      echo "strih-pipewire-purge: REFUSED: ${verdict#refuse } -- nothing purged (the apt-get -s read of: ${targets[*]})" >&2
      return 1
    fi
    echo "  apt-get -s purge ${targets[*]}: ${verdict#ok }, all in the declared list"
    DEBIAN_FRONTEND=noninteractive apt-get purge -y "${targets[@]}" \
      || { echo "strih-pipewire-purge: apt-get purge ${targets[*]} failed (above)" >&2; return 1; }
    left="$(_strih_pw_purge_installed "${STRIH_PW_PURGE_SERVER_PACKAGES[@]}")"
    [ -z "$left" ] || { echo "strih-pipewire-purge: still installed after the purge: ${left//$'\n'/ }" >&2; return 1; }
    kept_after="$(_strih_pw_purge_installed "${STRIH_PW_PURGE_KEEP[@]}")"
    [ "${kept_after//$'\n'/ }" = "$kept_before" ] \
      || { echo "strih-pipewire-purge: the kept libraries changed (${kept_before:-none} -> ${kept_after//$'\n'/ })" >&2; return 1; }
    echo "  purged ${targets[*]}; kept ${kept_before:-<none installed>}"
  fi
  for rel in "${STREAM_OPERATOR_AUDIO_LEFTOVERS[@]}"; do
    f="${home}/.config/${rel}"
    if [ -e "$f" ] || [ -L "$f" ]; then
      rm -f "$f" || { echo "strih-pipewire-purge: could not remove ${f}" >&2; return 1; }
      echo "  removed the strih-role PipeWire drop-in ${f}"
    fi
  done
  if pgrep -u "$user" -x 'pipewire|pipewire-pulse|wireplumber' >/dev/null 2>&1; then
    echo "  NOTE: ${user}'s PipeWire processes still run from the old binaries until the session restarts -- reboot the box (owner approval), then verify-strih item 9b"
  fi
  return 0
}

# --- verify-strih item 9b -------------------------------------------------------------------------------

# _strih_pw_purge_packages_row ARCHS -> the (pipewire-packages) row. Always rc 0.
_strih_pw_purge_packages_row() {
  local pref bad="" left keep archv=() a
  pref="$(_strih_pw_purge_pref)"
  left="$(_strih_pw_purge_installed "${STRIH_PW_PURGE_SERVER_PACKAGES[@]}")"
  [ -z "$left" ] || bad="${bad} still installed: ${left//$'\n'/ };"
  while IFS= read -r a; do [ -n "$a" ] && archv+=("$a"); done <<<"${1-}"
  if [ "${#archv[@]}" -eq 0 ]; then
    bad="${bad} the box's architectures could not be read;"
  elif ! [ -f "$pref" ] || ! cmp -s "$pref" <(strih_pipewire_purge_pin_text "${archv[@]}"); then
    bad="${bad} the pin ${pref} is missing or differs from the rendered one;"
  fi
  [ -n "$(_strih_pw_purge_installed "$STRIH_PW_PURGE_SHIM")" ] || bad="${bad} ${STRIH_PW_PURGE_SHIM} not installed;"
  keep="$(_strih_pw_purge_installed "${STRIH_PW_PURGE_KEEP[@]}")"
  for a in "${STRIH_PW_PURGE_KEEP[@]}"; do
    grep -qE "^${a//./\\.}(:|$)" <<<"$keep" || bad="${bad} ${a} is gone (OBS links it);"
  done
  if [ -n "$bad" ]; then
    printf 'FAIL|(pipewire-packages)%s -- re-run setup-strih.sh step 12\n' "$bad"
  else
    printf 'OK|(pipewire-packages) no PipeWire / WirePlumber / pulseaudio server installed, pinned out for %s, %s in, %s kept\n' \
      "${archv[*]}" "$STRIH_PW_PURGE_SHIM" "${STRIH_PW_PURGE_KEEP[*]}"
  fi
  return 0
}

# _strih_pw_purge_process_row USER -> the (pipewire-process) row. Always rc 0.
_strih_pw_purge_process_row() {
  local user="$1" out rc=0
  out="$(pgrep -u "$user" -a -x 'pipewire|pipewire-pulse|wireplumber' 2>&1)" || rc=$?
  case "$rc" in
    1) printf 'OK|(pipewire-process) no PipeWire / WirePlumber process runs for %s\n' "$user" ;;
    0) printf 'FAIL|(pipewire-process) still running for %s: %s -- the purge applies when the session restarts: reboot the box (owner approval)\n' \
         "$user" "${out//$'\n'/; }" ;;
    *) printf 'FAIL|(pipewire-process) pgrep could not be read (rc %s: %s)\n' "$rc" "${out%%$'\n'*}" ;;
  esac
  return 0
}

# _strih_pw_purge_rustdesk_row -> the (pipewire-rustdesk) row. Always rc 0.
_strih_pw_purge_rustdesk_row() {
  local st
  if [ -z "$(_strih_pw_purge_installed rustdesk)" ]; then
    printf 'NOTE|(pipewire-rustdesk) RustDesk not installed (setup-strih step 16b places it with its password file)\n'
    return 0
  fi
  st="$(systemctl is-active rustdesk 2>/dev/null)" || true
  if [ "$st" = active ]; then
    printf 'OK|(pipewire-rustdesk) RustDesk installed and active without PipeWire\n'
  else
    printf 'FAIL|(pipewire-rustdesk) RustDesk installed but %s -- systemctl enable --now rustdesk; the shim keeps its gstreamer1.0-pipewire dependency\n' \
      "${st:-<unread>}"
  fi
  return 0
}

# _strih_pw_purge_pcm_owner STATUSFILE -> the comm of the process holding the PCM substream, `closed` when
# nothing holds it, `missing` when the file is not there. Always rc 0.
_strih_pw_purge_pcm_owner() {
  local f="$1" pid="" line comm
  [ -f "$f" ] || { printf 'missing'; return 0; }
  while IFS= read -r line; do
    case "$line" in owner_pid*) pid="${line##*:}"; pid="${pid//[[:space:]]/}" ;; esac
  done < "$f"
  if [ -z "$pid" ]; then printf 'closed'; return 0; fi
  comm="$( { tr -d '\n' < "${STRIH_PW_PURGE_PROC:-/proc}/${pid}/comm"; } 2>/dev/null)" || comm=""
  printf '%s' "${comm:-pid ${pid} (gone)}"
}

# _strih_pw_purge_cards_row TOML -> the (pipewire-cards) row: every side of every hub `alsa` participant is
# held by the hub's aplay (playback) / arecord (capture), never by a sound server. Always rc 0.
_strih_pw_purge_cards_row() {
  local rows name adapter card dev out in asound="${STRIH_PW_PURGE_ASOUND:-/proc/asound}" bad="" good="" o side tool
  rows="$(_strih_pw_purge_toml_participants "${1-}" 2>&1)" || { printf 'FAIL|(pipewire-cards) %s\n' "${rows##*$'\n'}"; return 0; }
  while IFS=$'\t' read -r name adapter card dev out in; do
    [ "$adapter" = alsa ] || continue
    dev="${dev:-0}"
    if [ ! -d "${asound}/${card}" ]; then bad="${bad} ${name}: card ${card} not on the box;"; continue; fi
    for side in p c; do
      if [ "$side" = p ]; then tool=aplay; [ "${out:-0}" -gt 0 ] 2>/dev/null || continue
      else tool=arecord; [ "${in:-0}" -gt 0 ] 2>/dev/null || continue; fi
      o="$(_strih_pw_purge_pcm_owner "${asound}/${card}/pcm${dev}${side}/sub0/status")"
      case "$o" in
        "$tool") good="${good}${good:+, }${card} pcm${dev}${side}=${tool}" ;;
        closed) bad="${bad} ${card} pcm${dev}${side} is held by nothing (the hub's card leg is not running);" ;;
        missing) bad="${bad} ${card} has no pcm${dev}${side};" ;;
        *) bad="${bad} ${card} pcm${dev}${side} is held by ${o}, not ${tool};" ;;
      esac
    done
  done <<<"$rows"
  if [ -n "$bad" ]; then
    printf 'FAIL|(pipewire-cards)%s\n' "$bad"
  elif [ -n "$good" ]; then
    printf 'OK|(pipewire-cards) the card PCMs are the hub'"'"'s own: %s\n' "$good"
  else
    printf 'NOTE|(pipewire-cards) the hub routing has no alsa card participant\n'
  fi
  return 0
}

# _strih_pw_purge_dropins_row USER_HOME USER -> the (pipewire-dropins) row. Always rc 0.
_strih_pw_purge_dropins_row() {
  local home="$1" user="$2" rel bad="" hub uid
  hub="${STRIH_PW_PURGE_HUB_DROPIN:-/etc/systemd/system/intercom-hub.service.d/10-local-audio.conf}"
  for rel in "${STREAM_OPERATOR_AUDIO_LEFTOVERS[@]}"; do
    if [ -e "${home}/.config/${rel}" ] || [ -L "${home}/.config/${rel}" ]; then bad="${bad} ${rel##*/} is back;"; fi
  done
  uid="$(id -u "$user" 2>/dev/null)" || uid=""
  if [ -z "$uid" ]; then
    bad="${bad} unknown user ${user};"
  elif ! [ -f "$hub" ] || ! cmp -s "$hub" <(strih_intercom_audio_dropin "$user" "$uid" no-pipewire); then
    bad="${bad} ${hub##*/} missing or not the no-PipeWire render (setup-strih step 13);"
  fi
  if [ -n "$bad" ]; then
    printf 'FAIL|(pipewire-dropins)%s -- re-run setup-strih.sh steps 12 + 13\n' "$bad"
  else
    printf 'OK|(pipewire-dropins) no strih-role PipeWire drop-in left; the hub drop-in carries no pipewire group or runtime\n'
  fi
  return 0
}

# _strih_pw_purge_browser_row REPO -> the (browser-audio) row: every OBS browser source reroutes its audio into
# OBS and is muted there (strih_scenes.py --browser-audio). Always rc 0.
_strih_pw_purge_browser_row() {
  local out rc=0 line
  out="$(timeout 20 python3 "${STRIH_PW_PURGE_SCENES:-${1}/scripts/strih_scenes.py}" --browser-audio \
    --host "${OBS_WS_HOST:-127.0.0.1}" 2>&1)" || rc=$?
  line="${out##*$'\n'}"
  if [ "$rc" = 2 ] || [ "$rc" = 124 ]; then
    printf 'NOTE|(browser-audio) OBS websocket not answering (%s) -- the browser sources are graded while OBS runs\n' "${out%%$'\n'*}"
  elif [ "$rc" = 0 ] && [ "${line#browser audio: OK (}" != "$line" ]; then
    line="${line#browser audio: OK (}"
    printf 'OK|(browser-audio) %s\n' "${line%)}"
  else
    printf 'FAIL|(browser-audio) %s -- the launch seed (strih_scenes.py --bootstrap) sets reroute_audio + mute at the next OBS start\n' \
      "${line:-the reader printed nothing (rc ${rc})}"
  fi
  return 0
}

# strih_pipewire_purge_grade_rows REPO USER_HOME USER -> the rows (STATUS|text), then `END|<n>` = how many
# rows came before it. Always rc 0.
strih_pipewire_purge_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?user required}"
  local line rc=0 archs toml
  line="$(strih_pipewire_purge_why)" || rc=$?
  case "$rc" in
    0) archs="$(strih_pipewire_purge_archs 2>/dev/null)" || archs=""
       toml="$(_strih_pw_purge_toml)" || toml=""
       _strih_pw_purge_packages_row "$archs"
       _strih_pw_purge_process_row "$user"
       _strih_pw_purge_rustdesk_row
       _strih_pw_purge_cards_row "$toml"
       _strih_pw_purge_dropins_row "$home" "$user"
       _strih_pw_purge_browser_row "$repo"
       printf 'END|%s\n' "$STRIH_PW_PURGE_ROWS" ;;
    1) printf 'NOTE|(pipewire-purge) PipeWire is not purged here: %s\n' "${line#*|}"
       if [ "$(strih_box_role 2>/dev/null)" = strih ]; then
         _strih_pw_purge_browser_row "$repo"
         printf 'END|2\n'
       else
         printf 'END|1\n'
       fi ;;
    *) printf 'FAIL|(pipewire-purge) cannot decide whether PipeWire belongs on this box: %s\n' "${line#*|}"
       printf 'END|1\n' ;;
  esac
  return 0
}

# strih_pipewire_purge_grade_report REPO USER_HOME USER -> the rows through verify-strih's ok / note / bad.
# rc 1 unless every row was printed (a grader that stopped part way is a FAIL, never a shorter list).
strih_pipewire_purge_grade_report() {
  local st d rows=0 expect=""
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    case "$st" in
      END) expect="$d"; continue ;;
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
    rows=$((rows + 1))
  done < <(strih_pipewire_purge_grade_rows "$@")
  [ -n "$expect" ] && [ "$rows" = "$expect" ]
}
