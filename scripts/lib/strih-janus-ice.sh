#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# issue 1499: Janus gathers ICE candidates on the LAN address only, on a box with an audio-VLAN leg.
#
# dstack's two-networks rule (section 4, owner 10.11.2026): anything a box announces for a non-Dante service
# carries the LAN or loopback address only, never the Dante one, "a Janus candidate list" included. Janus
# gathers host candidates on every interface by default, so on strih-snv (the leg enp7s0.70 = 10.77.7.202) a
# phone's WebRTC session got the Dante-network address. The box fact STRIH_PROGRAM_VBAN_LEG decides:
#
#   a leg declared (mac:/vlan:) -> janus.jcfg's nat section gets the managed `ice_enforce_list = "<STRIH_IP>"`
#                                  block (scripts/janus_ice_cfg.py); Janus then gathers on the LAN address only
#                                  and ignores the package's `ice_ignore_list = "vmnet"` line, which stays as is.
#   no leg (none)               -> the block is removed, so a box without the Dante path is untouched (owner
#                                  11.11.2026: nothing Dante-specific acts on a PC without it).
#
# strih_janus_ice_apply  = setup-strih step 14, after the jcfg writes and before its restart of a running Janus.
# strih_janus_ice_grade  = verify item 9, from strih_vban_leg_grade (a declared leg only): the block = STRIH_IP,
#                          and a running Janus started after the jcfg's last change (else it serves the old list).
# Sourced by scripts/lib/strih-provision.sh after the box facts; reads facts, no network.

# strih_janus_ice_jcfg -> the Janus core config path (a test seam).
strih_janus_ice_jcfg() { printf '%s' "${STRIH_JANUS_ICE_JCFG:-/etc/janus/janus.jcfg}"; }

# _strih_janus_ice_tool -> the editor next to this lib's scripts dir.
_strih_janus_ice_tool() { printf '%s/../janus_ice_cfg.py' "$(dirname "${BASH_SOURCE[0]}")"; }

# strih_janus_ice_wanted -> the address the enforce list must hold, or `none` when the box declares no leg.
strih_janus_ice_wanted() {
  local leg
  leg="$(strih_lx_program_vban_leg 2>/dev/null || echo none)"
  case "$leg" in
    mac:*|vlan:*) strih_lx_ip ;;
    *) echo none ;;
  esac
}

# strih_janus_ice_apply -> write (or remove) the managed block; one line saying what it did; rc 1 on a failure.
strih_janus_ice_apply() {
  local jcfg want out
  jcfg="$(strih_janus_ice_jcfg)"
  want="$(strih_janus_ice_wanted)" || { echo "  janus.jcfg ICE: the box fact STRIH_IP cannot be read" >&2; return 1; }
  if [ ! -f "$jcfg" ]; then
    echo "  janus.jcfg ICE: ${jcfg} absent (Janus not installed) -- nothing to write"
    return 0
  fi
  if [ "$want" = none ]; then
    out="$(python3 "$(_strih_janus_ice_tool)" --file "$jcfg" --clear)" || { echo "  janus.jcfg ICE: removing the enforce block failed (above)" >&2; return 1; }
    echo "  janus.jcfg ICE: no audio-VLAN leg on this box -> no enforce list (${out}); Janus keeps the package default"
  else
    out="$(python3 "$(_strih_janus_ice_tool)" --file "$jcfg" --set "$want")" || { echo "  janus.jcfg ICE: writing ice_enforce_list = ${want} failed (above)" >&2; return 1; }
    echo "  janus.jcfg ICE: ice_enforce_list = \"${want}\" (${out}) -- candidates on the LAN address only, never the leg (issue 1499)"
  fi
}

# strih_janus_ice_grade -> verify rows (ok / bad / note from the caller) for a box with a declared leg.
strih_janus_ice_grade() {
  local jcfg want v since started mtime
  jcfg="$(strih_janus_ice_jcfg)"
  want="$(strih_janus_ice_wanted 2>/dev/null || echo '?')"
  if [ ! -f "$jcfg" ]; then
    note "(janus-ice) ${jcfg} absent: Janus is not installed on this box"
    return 0
  fi
  v="$(python3 "$(_strih_janus_ice_tool)" --file "$jcfg" --check "$want" 2>/dev/null || true)"
  if [ "$v" != ok ]; then
    bad "(janus-ice) janus.jcfg nat: the ice_enforce_list block is ${v:-unreadable}, wants \"${want}\" -- Janus would offer the audio-VLAN address to phones (re-run setup-strih.sh step 14)"
    return 0
  fi
  if ! systemctl is-active --quiet janus 2>/dev/null; then
    ok "(janus-ice) janus.jcfg nat: ice_enforce_list = \"${want}\" (Janus not running)"
    return 0
  fi
  since="$(systemctl show janus -p ActiveEnterTimestamp --value 2>/dev/null || true)"
  started="$(date -d "$since" +%s 2>/dev/null || echo 0)"
  mtime="$(stat -c %Y "$jcfg" 2>/dev/null || echo 0)"
  if [ "$started" -lt "$mtime" ]; then
    bad "(janus-ice) janus.jcfg nat: ice_enforce_list = \"${want}\", but the running Janus started ($(date -d "@${started}" '+%F %T' 2>/dev/null || echo '?')) before the jcfg changed ($(date -d "@${mtime}" '+%F %T')) -- it still gathers on every interface (systemctl restart janus)"
  else
    ok "(janus-ice) janus.jcfg nat: ice_enforce_list = \"${want}\", the running Janus loaded it -- candidates on the LAN address only"
  fi
}
