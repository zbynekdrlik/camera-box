#!/usr/bin/env bash
# issue 1361 byte-identity net: render every FACT-DEPENDENT file/value setup-strih.sh writes for one box.
set -euo pipefail
#
# Usage: render.sh REPO_ROOT BOX
#
# Sources REPO_ROOT/scripts/setup-strih.sh with `--box BOX` (its source-guard stops before the
# root-only provisioning flow, so nothing is written anywhere) and prints, section by section, the
# exact text each generator produces -- the same function calls setup-strih.sh makes. The committed
# `strih-lx.golden` next to this file was captured from the PRE-issue-1361 scripts (strih-lx literals,
# run as `STRIH_LX_IP=10.77.9.202`, the documented real invocation); tests/strih_box_facts_1361.rs
# renders `--box strih-lx` from the fact file and diffs it against that golden, so the fact file must
# reproduce today's output byte-for-byte.
#
# The `type -t` fallbacks exist ONLY so this one script also ran against the pre-change tree (where
# the fact accessors did not exist yet) to capture the golden; on the current tree every accessor
# exists and the fallback branch is never taken.

ROOT="${1:?repo root required}"
BOX="${2:?box name required}"

# shellcheck source=scripts/setup-strih.sh
. "${ROOT}/scripts/setup-strih.sh" --box "$BOX"

has() { type -t "$1" >/dev/null 2>&1; }

section() { printf '=== %s ===\n' "$1"; }

section hostname
strih_lx_hostname; echo
section ip
strih_lx_ip; echo
section host
strih_lx_host; echo
section ndi-outputs
strih_lx_ndi_outputs
section ndi-republishes
strih_lx_ndi_republishes
section ndi-inputs
strih_lx_ndi_inputs
section seed-manifest
strih_lx_seed_manifest_json
section profile-facts
strih_lx_profile_facts
section dantesync-unit
if has strih_lx_dantesync_role; then
  DS_R="$(strih_lx_dantesync_role)"; DS_A="$(strih_lx_dantesync_args)"
else
  DS_R=server; DS_A=""
fi
strih_dantesync_unit_text "$DS_R" "$DS_A"
section janus-audiobridge
strih_janus_audiobridge_jcfg_text 1000 /etc/intercom-hub/janus-room.secret "$(strih_lx_ip)"
section janus-ws
strih_janus_ws_jcfg_text "$(strih_lx_ip)"
section openbox-autostart
strih_openbox_autostart_text
section openbox-menu
obs_box_openbox_menu_xml "$(strih_lx_hostname)" "systemctl --user start strih-obs.service" "/usr/local/bin/strih-obs-stop.sh"
section nic-irq-script
strih_nic_irq_affinity_script_text
section intercom-toml
if has strih_lx_intercom_config; then
  IC="$(strih_lx_intercom_config)"
else
  IC="intercom/intercom.strih-lx.toml"
fi
# the installed /etc/intercom-hub/intercom.toml is a verbatim copy -- pin its content by sha256.
sha256sum < "${ROOT}/${IC}" | cut -d' ' -f1
section ndi-runtime-peer
if has strih_lx_ndi_runtime_peer; then strih_lx_ndi_runtime_peer; else printf '10.77.9.61'; fi; echo
section companion-host-conf
strih_companion_satellite_config_text "$(strih_companion_satellite_host)"
section companion-appconfig
strih_companion_satellite_appconfig_json "$(strih_companion_satellite_host)" "$(strih_companion_satellite_port)"
