#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the operator keyboard install of setup-strih step 16g
# and the keyboard row of verify-strih item 43; no top-level statements) -- the sibling scripts/lib/*.sh
# convention of NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict
# mode would leak into it. setup-strih.sh / verify-strih.sh set their own. Every function checks its
# own return codes.
#
# scripts/lib/strih-keyboard.sh -- the operator keyboard of every Linux workstation box (strih-snv,
# strih-pp, stream-pp). Owner 14.10.2026: "potrebujeme slovensku klavesnicu vediet sa prepnut": us +
# Slovak (QWERTZ), Alt+Shift switching. Moved here verbatim from strih-operator.sh (issue 1441, its
# ~1000-line budget); strih-operator.sh sources this file, the function names are unchanged.
#
# issue 1441 (owner 21.10.2026, stream-pp: "nejde ju prepnut na slovensku ked na to klikam"): im-config
# starts ibus-daemon in the kiosk session, and with org.freedesktop.ibus.general
# use-system-keyboard-layout false (the default) ibus overwrote the X keymap with its engine's `us`
# at every login. :0 then carried ONE group and the panel's click locked US again (read live on all
# three boxes the same day). So the install sets that key true for the operator BEFORE its live
# setxkbmap, and the keyboard row grades it.
#
# Seams: STRIH_OPERATOR_KEYBOARD_FILE (the keyboard file), STRIH_OPERATOR_SETXKBMAP (the setxkbmap
# tool), STRIH_OPERATOR_GSETTINGS (the gsettings tool), STRIH_OPERATOR_RUN_USER_DIR (/run/user, where
# the operator's session bus lives). A test that runs the install MUST set the first three, or it
# writes this box's real keyboard file, keymap and dconf.

STRIH_OPERATOR_IBUS_SCHEMA="org.freedesktop.ibus.general"
STRIH_OPERATOR_IBUS_KEY="use-system-keyboard-layout"

# --- the operator keyboard (owner 14.10.2026: "potrebujeme slovensku klavesnicu vediet sa prepnut") ----
# Every Linux workstation box types us + Slovak (QWERTZ), Alt+Shift switching between them.
strih_operator_kb_layout() { printf 'us,sk'; }
strih_operator_kb_options() { printf 'grp:alt_shift_toggle'; }
strih_operator_keyboard_file() { printf '%s' "${STRIH_OPERATOR_KEYBOARD_FILE:-/etc/default/keyboard}"; }

# strih_operator_keyboard_install USER HOME -> write XKBLAYOUT / XKBOPTIONS into the keyboard file (read
# by X at its start), tell the operator's ibus to keep that layout (issue 1441) and apply it to the
# running :0 (WARN-only when X does not answer). rc 1 when the file cannot be written.
strih_operator_keyboard_install() {
  local user="$1" home="$2" f
  f="$(strih_operator_keyboard_file)"
  [ -f "$f" ] || printf 'XKBMODEL="pc105"\nXKBVARIANT=""\nBACKSPACE="guess"\n' >"$f" || return 1
  local k v
  for k in XKBLAYOUT XKBOPTIONS; do
    if [ "$k" = XKBLAYOUT ]; then v="$(strih_operator_kb_layout)"; else v="$(strih_operator_kb_options)"; fi
    if grep -q "^${k}=" "$f"; then
      sed -i "s|^${k}=.*|${k}=\"${v}\"|" "$f" || return 1
    else
      printf '%s="%s"\n' "$k" "$v" >>"$f" || return 1
    fi
  done
  echo "  keyboard: $(strih_operator_kb_layout), switch with Alt+Shift ($f)"
  # Before the live setxkbmap: ibus with the key false would overwrite it again.
  strih_operator_ibus_keep_layout "$user" "$home"
  if ! _strih_operator_on_x "$user" "$home" 10 "${STRIH_OPERATOR_SETXKBMAP:-setxkbmap}" \
    -layout "$(strih_operator_kb_layout)" -option "" -option "$(strih_operator_kb_options)"; then
    echo "  WARN: the keyboard layout could not be applied to the running :0 (X down?) -- it applies at the next X start"
  fi
  return 0
}

# _strih_operator_gsettings USER HOME ARGS... -> `gsettings ARGS...` as the operator, bounded: over
# their running session's bus when it exists (the live ibus then takes a change at once), else over a
# private bus from dbus-run-session (dconf writes the operator's own database either way).
_strih_operator_gsettings() {
  local user="$1" home="$2" tool uid bus
  shift 2
  tool="${STRIH_OPERATOR_GSETTINGS:-gsettings}"
  uid="$(id -u "$user" 2>/dev/null || true)"
  bus="${STRIH_OPERATOR_RUN_USER_DIR:-/run/user}/${uid}/bus"
  if [ -n "$uid" ] && [ -S "$bus" ]; then
    _strih_operator_on_x "$user" "$home" 10 env "DBUS_SESSION_BUS_ADDRESS=unix:path=${bus}" "$tool" "$@"
  else
    _strih_operator_on_x "$user" "$home" 10 dbus-run-session -- "$tool" "$@"
  fi
}

# strih_operator_ibus_state USER HOME -> the operator's ibus use-system-keyboard-layout: `true` /
# `false`, `absent` when ibus (its schema) or gsettings is not installed, NOTHING when it was not read
# or is not a boolean. Always rc 0.
strih_operator_ibus_state() {
  local user="$1" home="$2" schemas v
  command -v "${STRIH_OPERATOR_GSETTINGS:-gsettings}" >/dev/null 2>&1 || { printf 'absent'; return 0; }
  schemas="$(_strih_operator_gsettings "$user" "$home" list-schemas 2>/dev/null)" || return 0
  if ! grep -qxF "$STRIH_OPERATOR_IBUS_SCHEMA" <<<"$schemas"; then
    printf 'absent'
    return 0
  fi
  v="$(_strih_operator_gsettings "$user" "$home" get "$STRIH_OPERATOR_IBUS_SCHEMA" "$STRIH_OPERATOR_IBUS_KEY" \
    2>/dev/null)" || return 0
  case "$v" in true | false) printf '%s' "$v" ;; esac
  return 0
}

# strih_operator_ibus_keep_layout USER HOME -> set the operator's ibus use-system-keyboard-layout true
# (issue 1441). Nothing to do without ibus or when it is already true; a failed set is a WARN (the
# keyboard row FAILs until it is set). Always rc 0.
strih_operator_ibus_keep_layout() {
  local user="$1" home="$2" state
  state="$(strih_operator_ibus_state "$user" "$home")"
  case "$state" in
    absent) return 0 ;;
    true)
      echo "  keyboard: ibus keeps the system layout ($STRIH_OPERATOR_IBUS_KEY true)"
      return 0
      ;;
  esac
  if _strih_operator_gsettings "$user" "$home" set "$STRIH_OPERATOR_IBUS_SCHEMA" "$STRIH_OPERATOR_IBUS_KEY" true; then
    echo "  keyboard: ibus now keeps the system layout ($STRIH_OPERATOR_IBUS_KEY true; it reset :0 to us at every login, issue 1441)"
  else
    echo "  WARN: could not set ibus $STRIH_OPERATOR_IBUS_KEY true for $user -- verify-strih (keyboard) FAILs until it is set"
  fi
  return 0
}

# strih_operator_keyboard_verdict FILE_TEXT LIVE_QUERY IBUS_STATE -> ONE row (keyboard): the keyboard
# file AND the live :0 (`setxkbmap -query`) both carry the us,sk layout with the Alt+Shift switch, and
# the operator's ibus keeps it (IBUS_STATE from strih_operator_ibus_state; `absent` = not graded).
strih_operator_keyboard_verdict() {
  local file="${1-}" live="${2-}" ibus="${3-}" want_l want_o fl fo ll lo
  want_l="$(strih_operator_kb_layout)"; want_o="$(strih_operator_kb_options)"
  fl="$(sed -n 's/^XKBLAYOUT="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' <<<"$file" | head -n 1)"
  fo="$(sed -n 's/^XKBOPTIONS="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' <<<"$file" | head -n 1)"
  ll="$(awk '$1 == "layout:" {print $2}' <<<"$live" | head -n 1)"
  lo="$(awk '$1 == "options:" {print $2}' <<<"$live" | head -n 1)"
  if [ "$fl" != "$want_l" ] || [[ ",$fo," != *",$want_o,"* ]]; then
    printf 'FAIL|(keyboard) %s has XKBLAYOUT="%s" XKBOPTIONS="%s", want "%s" + "%s" -- re-run setup-strih.sh step 16g\n' \
      "$(strih_operator_keyboard_file)" "$fl" "$fo" "$want_l" "$want_o"
  elif [ "$ibus" = false ]; then
    printf 'FAIL|(keyboard) ibus %s %s is false: ibus puts :0 back on its own us layout at every login -- re-run setup-strih.sh step 16g (issue 1441)\n' \
      "$STRIH_OPERATOR_IBUS_SCHEMA" "$STRIH_OPERATOR_IBUS_KEY"
  elif [ -z "$ibus" ]; then
    printf 'FAIL|(keyboard) ibus %s not read (gsettings as the operator answered nothing or no boolean)\n' \
      "$STRIH_OPERATOR_IBUS_KEY"
  elif [ -z "$live" ]; then
    printf 'FAIL|(keyboard) setxkbmap on :0 answered nothing -- X is down or does not answer clients\n'
  elif [ "$ll" != "$want_l" ] || [[ ",$lo," != *",$want_o,"* ]]; then
    printf 'FAIL|(keyboard) the running :0 types layout "%s" options "%s", want "%s" + "%s" -- re-run setup-strih.sh step 16g (it applies it live)\n' \
      "$ll" "$lo" "$want_l" "$want_o"
  elif [ "$ibus" = true ]; then
    printf 'OK|(keyboard) %s, Alt+Shift switches (the file and the running :0; ibus keeps the layout)\n' "$want_l"
  else
    printf 'OK|(keyboard) %s, Alt+Shift switches (the file and the running :0)\n' "$want_l"
  fi
}

# strih_operator_keyboard_row USER HOME -> the (keyboard) row of verify item 43: the keyboard file, the
# live `setxkbmap -query` on :0 and the operator's ibus setting.
strih_operator_keyboard_row() {
  local user="$1" home="$2"
  strih_operator_keyboard_verdict "$(cat "$(strih_operator_keyboard_file)" 2>/dev/null || true)" \
    "$(_strih_operator_on_x "$user" "$home" 10 "${STRIH_OPERATOR_SETXKBMAP:-setxkbmap}" -query 2>/dev/null || true)" \
    "$(strih_operator_ibus_state "$user" "$home")"
}
