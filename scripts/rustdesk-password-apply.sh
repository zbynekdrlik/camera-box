#!/usr/bin/env bash
# rustdesk-password-apply.sh BOX... -- apply the owner's RustDesk permanent password on Linux OBS boxes (issue 1462).
set -euo pipefail
#
# Owner 25.10.2026: "vsade na rustdeskoch mame rovnake heslo". The ONE password lives on dev1 in the
# credential channel; this script never reads a file itself. Run it through `secret exec`, which hands the
# value in the environment and filters it out of this script's output:
#
#   python3 ~/devel/airuleset/airuleset.py secret exec --file ~/.secrets/rustdesk-permanent-password \
#     --env RUSTDESK_PERMANENT_PASSWORD -- scripts/rustdesk-password-apply.sh stream-snv
#
# Per box: ssh as the box's desktop user (STRIH_LX_USER / STRIH_LX_PW, the rig's shared Linux-box creds,
# the deploy arm's defaults) and run `sudo -n rustdesk --password` there. The value travels on the ssh
# STDIN only, never in an argv on dev1 (RustDesk 1.4.x has no other way to set it, so the `rustdesk`
# process on the box sees it in its own argv for the moment it runs: a CLI limit, local to the box). Its
# output is discarded (never echoed). A box name without a dot that does not resolve is tried as <box>.lan.
# Exit 0 only when every box applied; each box prints one `<box>: applied` / `<box>: FAILED (...)` line.
#
# Seams for the tests: RUSTDESK_APPLY_SSHPASS (default sshpass), RUSTDESK_APPLY_RESOLVE (default getent hosts).

: "${RUSTDESK_PERMANENT_PASSWORD:?run through: airuleset.py secret exec ... --env RUSTDESK_PERMANENT_PASSWORD -- $0 BOX...}"
[ "$#" -gt 0 ] || { echo "usage: $0 BOX..." >&2; exit 2; }
USER_NAME="${STRIH_LX_USER:-newlevel}"
SSHPASS_BIN="${RUSTDESK_APPLY_SSHPASS:-sshpass}"
RESOLVE="${RUSTDESK_APPLY_RESOLVE:-getent hosts}"
# The remote side reads the value from stdin into a shell variable, hands it to the root rustdesk the same
# way, and reports only an exit code.
REMOTE='IFS= read -r p; printf "%s\n" "$p" | sudo -n sh -c "IFS= read -r q; rustdesk --password \"\$q\" >/dev/null 2>&1"; r=$?; unset p; exit "$r"'

rc=0
for box in "$@"; do
  host="$box"
  case "$box" in
    *.*) ;;
    *) $RESOLVE "$box" >/dev/null 2>&1 || host="${box}.lan" ;;
  esac
  if printf '%s\n' "$RUSTDESK_PERMANENT_PASSWORD" | "$SSHPASS_BIN" -p "${STRIH_LX_PW:-newlevel}" timeout 40 ssh \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "${USER_NAME}@${host}" "$REMOTE"; then
    echo "${box}: applied (${host})"
  else
    echo "${box}: FAILED (${host}, rc $?) -- check ssh to ${USER_NAME}@${host} and NOPASSWD sudo there"
    rc=1
  fi
done
exit "$rc"
