#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions + three constants, nothing executed) --
# the scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes it in
# the CALLER's shell (deploy-genlock-fleet.sh sets its own strict mode). Every step below checks its
# own rc explicitly and returns a named failure, because the caller runs it in an `||` context where
# errexit is off by design.
#
# scripts/lib/strih-lx-deploy.sh -- issue 1317 part 6: the strih-lx EXECUTE arm of
# scripts/deploy-genlock-fleet.sh, plus the builders its --plan arm prints (the remote commands are
# the same strings in both).
#
# strih-lx (10.77.9.202, a linux-genlock box since the M4 cut-over) installs the strih FULL genlock
# artifact through scripts/setup-strih.sh (release-parity gate, runtime packages, /usr prefix,
# chrome-sandbox) and runs OBS under the strih-obs.service user unit. Before this lib the supervisor
# deployed it with an ad-hoc scratch script that twice failed half-silently: the box's /tmp quota was
# full (each staged bundle is ~2.2 GB on a 7.5 GB tmpfs), rsync died `Disk quota exceeded` (rc 11),
# and the old build simply kept running with nothing refusing.
#
# Two phases, so deploy-genlock-fleet.sh can resolve EVERY requested box before changing any:
#
#   strih_lx_prepare (exit 3 on failure, the box is never touched)
#     resolve  -- the linux-genlock.yml run at the anchor's SAME SHA; the dial host must be a dotted
#                 IPv4 (setup-strih.sh pins STRIH_LX_IP as the box's own static address);
#     download -- the strih FULL artifact, REFUSED unless its GENLOCK_BUILD_SHA.txt is the canonical
#                 SHA and its BUNDLE_MANIFEST.json carries the libobs.so.30 sha256 the read-back uses;
#     tree     -- the COMMITTED scripts/ systemd/ intercom/ of this checkout (an archive of HEAD) + the
#                 generated run-setup.sh; the GH token (STRIH_LX_GH_TOKEN, else `gh auth token`).
#   strih_lx_apply (exit 4 on failure, every message names its step)
#     preflight -- refuse while a previous setup-strih.sh still runs on the box;
#     sweep     -- the stage is created + touched NEWEST, then the EXISTING obs-backup-retention.sh
#                  --local-sweep decision (--stages-only, keep the newest 1, as the operator -- the
#                  stage dirs are the operator's) removes every older /tmp/genlock-stage-<sha>; a stage
#                  it could not remove is a loud WARNING (the rsync below is the hard gate);
#     stage     -- rsync bundle/ + repo/ into /tmp/genlock-stage-<sha>/ WHILE THE OLD OBS KEEPS
#                  RUNNING: a failed rsync exits 4 before anything is stopped;
#     stop      -- delegated to the sanctioned stop code (/usr/local/bin/strih-obs-stop.sh, which
#                  routes through `systemctl --user stop`) + a bounded wait; the deploy itself sends no
#                  kill (the stop code's own SIGTERM->grace->SIGKILL ladder is the unit's contract);
#     setup     -- setup-strih.sh as root, DETACHED on the box (an ssh drop cannot kill it mid-apt),
#                  the GH token on the launch's STDIN only (never an argv, never a file), rc polled;
#                  OBS is never started over an installer that is still running;
#     start     -- systemctl --user start strih-obs.service (after touching a start marker);
#     verify    -- REFUSE unless /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == the canonical SHA, the
#                  installed /usr/lib/x86_64-linux-gnu/libobs.so.30 bytes match the bundle manifest,
#                  strih-obs.service is active with the SAME MainPID and NRestarts for a settle time
#                  (a crash-looping Type=simple unit reads `active` between restarts), the OBS log
#                  written after the start shows `render tick ENABLED`, and :8899 reports the SHA.
#
# Every failure prints `ERROR: [strih-lx <step>] failed (rc=N): <what>` on stderr.
#
# Test seams (tests/deploy_genlock_fleet_strih_lx_exec_1317.rs stubs gh/sshpass/ssh/rsync/curl on
# PATH): STRIH_LX_SETUP_POLLS / STRIH_LX_SETUP_POLL_SECS (setup rc poll, default 270 x 10 s = 45 min),
# STRIH_LX_VERIFY_POLLS / STRIH_LX_VERIFY_POLL_SECS (read-back poll, default 24 x 10 s = 4 min),
# STRIH_LX_VERIFY_SETTLE_SECS (how long the MainPID/NRestarts must hold, default 90 s),
# STRIH_LX_SSH_TIMEOUT (per remote command, default 180 s).
# Transport env: STRIH_LX_IP (dial override, via fleet_box_ip -- must be the box's IPv4), STRIH_LX_USER
# / STRIH_LX_PW (default newlevel / newlevel -- the rig's shared Linux-box creds, targets.md; sshpass
# -p is the repo-wide convention), STRIH_LX_GH_TOKEN (a read-only token instead of the operator's).

STRIH_LX_STAGE_PARENT="/tmp"
STRIH_LX_SSH_OPTS="-o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR -o ConnectTimeout=12 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"
STRIH_LX_RETENTION_ARGS="--local-sweep --stages-only --stage-parent /tmp --keep-runs 1 --keep-days 0 --execute"

# strih_lx_stage_dir SHA -> /tmp/genlock-stage-<sha>, the obs-backup-retention allowlist shape
# (`^(stage-genlock|genlock-stage)-[0-9a-f]+$`) so a later sweep can prune it. A non-hex SHA is
# refused (rc 2) rather than turned into a name the sweep would never recognise. Pure.
strih_lx_stage_dir() {
  local sha="${1:-}"
  case "$sha" in
    ''|*[!0-9a-f]*) echo "strih_lx_stage_dir: SHA must be lowercase hex, got '$sha'" >&2; return 2 ;;
  esac
  printf '%s/genlock-stage-%s\n' "$STRIH_LX_STAGE_PARENT" "$sha"
}

# --- remote command builders (pure; the SAME text execute runs and --plan prints) ----------------

strih_lx_remote_installer_cmd() {
  printf '%s\n' 'if pgrep -x setup-strih.sh >/dev/null; then echo alive; else echo idle; fi'
}

# create the stage dirs and touch the stage so it is the NEWEST stage dir -> the keep-newest-1 sweep
# can never delete the stage being deployed.
strih_lx_remote_prep_cmd() {
  printf "mkdir -p '%s/bundle' '%s/repo' && touch '%s'\n" "$1" "$1" "$1"
}

# the obs-backup-retention.sh --local-sweep leg (the leg `obs-backup-retention.sh --box strih-lx`
# runs), dialled at the deploy's own host, stage dirs only, as the operator: stdin = the retention
# script itself (from the committed tree) -- no sudo, so no password line ever reaches a shell.
strih_lx_remote_sweep_cmd() {
  printf 'bash -s -- %s\n' "$STRIH_LX_RETENTION_ARGS"
}

# the deploying stage must still be there; any OTHER stage dir the sweep left is printed LEFTOVER.
strih_lx_remote_stage_check_cmd() {
  printf "test -d '%s/bundle' && test -d '%s/repo' || exit 3; for d in /tmp/genlock-stage-* /tmp/stage-genlock-*; do if [ -d \"\$d\" ] && [ \"\$d\" != '%s' ]; then echo \"LEFTOVER \$d\"; fi; done; exit 0\n" "$1" "$1" "$1"
}

# the graceful stop, delegated to strih-obs-stop.sh (plain mode routes through `systemctl --user
# stop`, so the unit's ExecStop runs and Restart=on-failure does not relaunch it), then a bounded wait
# for BOTH the unit and every obs process to be gone. Not gone in 30 s = exit 5 (a refusal); this
# command itself never sends a signal.
strih_lx_remote_stop_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf '%s\n' 'export XDG_RUNTIME_DIR=/run/user/$(id -u); if [ -x /usr/local/bin/strih-obs-stop.sh ]; then /usr/local/bin/strih-obs-stop.sh || exit 5; else systemctl --user stop strih-obs.service || exit 5; fi; i=0; while systemctl --user is-active --quiet strih-obs.service || pgrep -x obs >/dev/null; do i=$((i+1)); if [ "$i" -gt 30 ]; then echo "strih-obs/obs still running 30 s after the graceful stop" >&2; exit 5; fi; sleep 1; done; echo "strih-obs stopped (graceful)"'
}

# launch the staged run-setup.sh as root. stdin = the sudo password line, then `ghtoken:<token>`
# (the runner skips a password line a NOPASSWD sudo left unread). `-k` ignores a cached credential.
strih_lx_remote_setup_launch_cmd() {
  printf "sudo -k -S -p '' bash '%s/repo/run-setup.sh'\n" "$1"
}

strih_lx_remote_setup_rc_cmd() {
  printf "cat '%s/setup-strih.rc' 2>/dev/null || true\n" "$1"
}

strih_lx_remote_setup_log_cmd() {
  printf "tail -n 60 '%s/setup-strih.log' 2>/dev/null || true\n" "$1"
}

# setup-strih.sh exits 0 with a reboot-pending warning (the baseline lands at the next boot): pass
# THAT line on instead of letting VERIFIED swallow it. Keyed on the warning's own text (setup-strih.sh
# step 17; a test pins the coupling) -- the baseline prints many routine "next boot" lines earlier.
strih_lx_remote_setup_notes_cmd() {
  printf "grep -F -e 'reboot strih-lx, then run verify-strih' '%s/setup-strih.log' 2>/dev/null || true\n" "$1"
}

# touch the start marker (the read-back only trusts an OBS log written after it), then start.
strih_lx_remote_start_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf 'export XDG_RUNTIME_DIR=/run/user/$(id -u); touch %s; systemctl --user reset-failed strih-obs.service 2>/dev/null; systemctl --user start strih-obs.service\n' "'$1/obs-start.marker'"
}

# one line `installed=<marker sha> active=<unit state> pid=<MainPID> restarts=<NRestarts>
# lib=<sha256 of the installed libobs.so.30> tick=<1 when the newest OBS log is newer than the start
# marker AND shows `render tick ENABLED`, else 0>`.
strih_lx_remote_readback_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf 'export XDG_RUNTIME_DIR=/run/user/$(id -u); L=$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -n 1); t=0; M=%s; if [ -n "$L" ] && [ -f "$M" ] && [ "$L" -nt "$M" ] && LC_ALL=C grep -a -q "render tick ENABLED" "$L"; then t=1; fi; printf "installed=%%s active=%%s pid=%%s restarts=%%s lib=%%s tick=%%s\\n" "$(tr -d "[:space:]" 2>/dev/null < /opt/obs-genlock/GENLOCK_BUILD_SHA.txt)" "$(systemctl --user is-active strih-obs.service 2>/dev/null)" "$(systemctl --user show -p MainPID --value strih-obs.service 2>/dev/null)" "$(systemctl --user show -p NRestarts --value strih-obs.service 2>/dev/null)" "$(sha256sum /usr/lib/x86_64-linux-gnu/libobs.so.30 2>/dev/null | cut -d" " -f1)" "$t"\n' "'$1/obs-start.marker'"
}

strih_lx_bundle_state_url() {
  printf 'http://%s:8899/bundle-state.json\n' "$1"
}

# strih_lx_setup_runner STAGE HOST -> the on-box run-setup.sh (staged at STAGE/repo/run-setup.sh).
# Run as root with stdin = the rest of the launch pipe: it reads the `ghtoken:` line (skipping a
# sudo password line a NOPASSWD sudo left unread), exports it with STRIH_LX_BUNDLE_SRC/STRIH_LX_IP,
# and re-execs itself DETACHED (setsid nohup, stdio to files) to run setup-strih.sh and write its rc
# to STAGE/setup-strih.rc -- so an ssh drop mid-apt never kills the install, and the rc is explicit.
strih_lx_setup_runner() {
  local stage="$1" host="$2"
  printf '#!/bin/bash\n# run-setup.sh -- generated by scripts/lib/strih-lx-deploy.sh (issue 1317 part 6).\n'
  printf 'set -uo pipefail\n'
  printf 'S=%q\nLX_IP=%q\n' "$stage" "$host"
  cat <<'RUNNER'
if [ "${1:-}" = "--child" ]; then
  "$S/repo/scripts/setup-strih.sh" > "$S/setup-strih.log" 2>&1
  echo "$?" > "$S/setup-strih.rc"
  exit 0
fi
[ "$(id -u)" = 0 ] || { echo "run-setup: must run as root" >&2; exit 2; }
GH_TOKEN=""
while IFS= read -r line; do
  case "$line" in ghtoken:*) GH_TOKEN="${line#ghtoken:}"; break ;; esac
done
[ -n "$GH_TOKEN" ] || { echo "run-setup: no ghtoken: line on stdin" >&2; exit 2; }
[ -x "$S/repo/scripts/setup-strih.sh" ] || { echo "run-setup: $S/repo/scripts/setup-strih.sh missing or not executable" >&2; exit 2; }
export GH_TOKEN STRIH_LX_IP="$LX_IP" STRIH_LX_BUNDLE_SRC="$S/bundle"
rm -f "$S/setup-strih.rc" "$S/setup-strih.log"
setsid nohup bash "$0" --child </dev/null >/dev/null 2>&1 &
echo "run-setup: setup-strih.sh launched detached (log $S/setup-strih.log, rc $S/setup-strih.rc)"
RUNNER
}

# --- pure verdicts --------------------------------------------------------------------------------

# strih_lx_deploy_verdict WANT INSTALLED ACTIVE BS_SHA WANT_LIB LIB TICK -> `OK` (rc 0) or
# `FAIL: <why>` lines (rc 1). Fail-closed: an empty value anywhere is a FAIL, never "unknown = fine".
strih_lx_deploy_verdict() {
  local want="${1:-}" installed="${2:-}" active="${3:-}" bs="${4:-}" want_lib="${5:-}" lib="${6:-}" tick="${7:-}" bad=0
  if [ -z "$want" ]; then echo "FAIL: no canonical SHA to compare against"; return 1; fi
  if [ "$installed" != "$want" ]; then
    echo "FAIL: /opt/obs-genlock/GENLOCK_BUILD_SHA.txt=${installed:-<unreadable>} != canonical ${want}"; bad=1
  fi
  if [ -z "$want_lib" ] || [ "$lib" != "$want_lib" ]; then
    echo "FAIL: installed /usr/lib/x86_64-linux-gnu/libobs.so.30 sha256=${lib:-<unreadable>} != the bundle manifest's ${want_lib:-<none>}"; bad=1
  fi
  if [ "$active" != "active" ]; then
    echo "FAIL: strih-obs.service is ${active:-<unreadable>}, not active"; bad=1
  fi
  if [ "$tick" != "1" ]; then
    echo "FAIL: no 'render tick ENABLED' in an OBS log written after the start"; bad=1
  fi
  if [ "$bs" != "$want" ]; then
    echo "FAIL: :8899 genlock_build_sha=${bs:-<unreadable>} != canonical ${want}"; bad=1
  fi
  [ "$bad" = 0 ] && { echo "OK"; return 0; }
  return 1
}

# strih_lx_stable_verdict PID1 RESTARTS1 PID2 RESTARTS2 -> `OK` (rc 0) when the SAME non-zero MainPID
# and an unchanged NRestarts were read on two consecutive polls; else `FAIL: <why>` (rc 1). Pure.
strih_lx_stable_verdict() {
  local p1="${1:-}" r1="${2:-}" p2="${3:-}" r2="${4:-}"
  case "$p1" in ''|0) echo "FAIL: strih-obs.service has no MainPID (${p1:-<unreadable>})"; return 1 ;; esac
  if [ "$p1" != "$p2" ] || [ "$r1" != "$r2" ]; then
    echo "FAIL: strih-obs.service restarted between polls (MainPID ${p1} -> ${p2:-<none>}, NRestarts ${r1:-?} -> ${r2:-?}) -- a crash-looping OBS"
    return 1
  fi
  echo "OK"
}

# strih_lx_is_ipv4 ADDR -> rc 0 for four dot-separated decimal octets 0..255. Pure.
strih_lx_is_ipv4() {
  local a="${1:-}" o
  [[ "$a" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
  local IFS=.
  for o in $a; do [ "$((10#$o))" -le 255 ] || return 1; done
  return 0
}

# --- execute arm ----------------------------------------------------------------------------------

_strih_lx_fail() {  # STEP RC EXIT MESSAGE -> prints the named error, returns EXIT
  echo "ERROR: [strih-lx $1] failed (rc=$2): $4" >&2
  return "$3"
}

# _strih_lx_ssh CMD -> run CMD on the box, bounded by STRIH_LX_SSH_TIMEOUT (stdin passes through).
# `timeout` sits INSIDE sshpass so the password prompt still reaches sshpass's pty.
_strih_lx_ssh() {
  local -a opts
  read -r -a opts <<< "$STRIH_LX_SSH_OPTS"
  sshpass -p "$STRIH_LX_PREP_PW" timeout "${STRIH_LX_SSH_TIMEOUT:-180}" ssh "${opts[@]}" \
    "${STRIH_LX_PREP_USER}@${STRIH_LX_PREP_HOST}" "$1"
}

_strih_lx_rsync() {  # SRC_DIR/ DEST_DIR/ -- --timeout=120 is rsync's own I/O-stall bound
  sshpass -p "$STRIH_LX_PREP_PW" rsync -a --delete --timeout=120 -e "ssh $STRIH_LX_SSH_OPTS" \
    "$1" "${STRIH_LX_PREP_USER}@${STRIH_LX_PREP_HOST}:$2"
}

_strih_lx_field() {  # KEY LINE -> the value of KEY=<value> in a read-back line
  printf ' %s\n' "$2" | sed -n "s/.*[[:space:]]$1=\([^ ]*\).*/\1/p" | head -n 1
}

_strih_lx_start_best_effort() {
  echo "# strih-lx: starting the installed OBS best-effort so the box is not left dark" >&2
  _strih_lx_ssh "$(strih_lx_remote_start_cmd "$STRIH_LX_PREP_STAGE")" || true
}

_strih_lx_installer_state() {  # -> alive | idle | unreachable (the ssh itself failed)
  local out
  out="$(_strih_lx_ssh "$(strih_lx_remote_installer_cmd)" 2>/dev/null)" || { echo unreachable; return 0; }
  case "$(printf '%s' "$out" | tr -d '[:space:]')" in
    idle) echo idle ;;
    alive) echo alive ;;
    *) echo unreachable ;;
  esac
}

# strih_lx_prepare SHA WORKDIR REPO GENLOCK_REPO -> everything that needs no box: resolve, download,
# the provisioning tree, the token. Sets the STRIH_LX_PREP_* globals strih_lx_apply reads. 0 or 3.
strih_lx_prepare() {
  local sha="$1" work="$2" repo="$3" gh_repo="$4"
  local w="$work/strih-lx" art lrun rc art_sha tree_rev tool f
  STRIH_LX_PREP_WORK="$w"
  STRIH_LX_PREP_USER="${STRIH_LX_USER:-newlevel}"
  STRIH_LX_PREP_PW="${STRIH_LX_PW:-newlevel}"
  STRIH_LX_PREP_HOST="$(fleet_box_ip strih-lx)" || { _strih_lx_fail resolve 2 3 "no strih-lx host (STRIH_LX_IP / obs-fleet row)"; return; }
  strih_lx_is_ipv4 "$STRIH_LX_PREP_HOST" \
    || { _strih_lx_fail resolve 2 3 "strih-lx host '$STRIH_LX_PREP_HOST' must be the box's dotted IPv4 -- setup-strih.sh pins STRIH_LX_IP as the box's own static address"; return; }
  STRIH_LX_PREP_STAGE="$(strih_lx_stage_dir "$sha")" || { _strih_lx_fail resolve 2 3 "canonical SHA '$sha' is not a hex commit id"; return; }
  local vp="${STRIH_LX_VERIFY_POLLS:-24}" vs="${STRIH_LX_VERIFY_POLL_SECS:-10}" st="${STRIH_LX_VERIFY_SETTLE_SECS:-90}"
  if [ "$st" -gt 0 ] && [ $((vp * vs)) -le "$st" ]; then
    _strih_lx_fail resolve 2 3 "STRIH_LX_VERIFY_SETTLE_SECS=$st cannot fit the read-back budget ${vp} x ${vs} s -- the deploy would always be refused"; return
  fi
  for tool in sshpass rsync curl tar jq timeout; do
    command -v "$tool" >/dev/null 2>&1 || { _strih_lx_fail resolve 127 3 "$tool is required for the strih-lx deploy"; return; }
  done
  art="$(fleet_linux_bundle_artifact_for strih-lx)"
  lrun="$(gh run list --repo "$gh_repo" --workflow linux-genlock.yml --json databaseId,headSha,conclusion --limit 50 2>/dev/null | fleet_pick_run_at_sha "$sha")" || true
  [ -n "$lrun" ] || { _strih_lx_fail resolve 1 3 "no successful linux-genlock.yml run at $sha -- build the strih artifact at that commit first (a tag dispatch)"; return; }

  mkdir -p "$w/bundle" "$w/repo" || { _strih_lx_fail download 1 3 "cannot create $w"; return; }
  gh run download "$lrun" --repo "$gh_repo" -n "$art" -D "$w/bundle"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail download "$rc" 3 "download of $art (run $lrun) failed"; return; }
  [ -f "$w/bundle/bin/obs" ] || { _strih_lx_fail download 1 3 "$art has no bin/obs"; return; }
  [ -d "$w/bundle/lib/x86_64-linux-gnu/obs-plugins" ] || { _strih_lx_fail download 1 3 "$art has no lib/x86_64-linux-gnu/obs-plugins"; return; }
  art_sha="$(tr -d '[:space:]' 2>/dev/null < "$w/bundle/GENLOCK_BUILD_SHA.txt")"
  [ "$art_sha" = "$sha" ] || { _strih_lx_fail download 1 3 "$art (run $lrun) carries GENLOCK_BUILD_SHA ${art_sha:-<none>}, not the canonical $sha -- refusing to stage a foreign build"; return; }
  STRIH_LX_PREP_LIBSHA="$(jq -r '.files[]? | select(.path == "lib/x86_64-linux-gnu/libobs.so.30") | .sha256' "$w/bundle/BUNDLE_MANIFEST.json" 2>/dev/null | head -n 1)"
  [ -n "$STRIH_LX_PREP_LIBSHA" ] || { _strih_lx_fail download 1 3 "$art BUNDLE_MANIFEST.json has no lib/x86_64-linux-gnu/libobs.so.30 sha256 -- the read-back could not prove the installed bytes"; return; }
  echo "# strih-lx: $art (run $lrun) downloaded, GENLOCK_BUILD_SHA=$art_sha"

  tree_rev="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)" || tree_rev=""
  [ -n "$tree_rev" ] || { _strih_lx_fail tree 1 3 "$repo is not a git checkout"; return; }
  git -C "$repo" archive --format=tar HEAD scripts systemd intercom | tar -x -C "$w/repo"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail tree "$rc" 3 "archiving scripts/ systemd/ intercom/ at $tree_rev failed"; return; }
  for f in scripts/setup-strih.sh scripts/obs-backup-retention.sh systemd/strih-obs.service intercom/intercom.strih-lx.toml; do
    [ -f "$w/repo/$f" ] || { _strih_lx_fail tree 1 3 "the provisioning tree at $tree_rev has no $f"; return; }
  done
  strih_lx_setup_runner "$STRIH_LX_PREP_STAGE" "$STRIH_LX_PREP_HOST" > "$w/repo/run-setup.sh" || { _strih_lx_fail tree 1 3 "cannot write run-setup.sh"; return; }
  STRIH_LX_PREP_TOKEN="${STRIH_LX_GH_TOKEN:-}"
  [ -n "$STRIH_LX_PREP_TOKEN" ] || STRIH_LX_PREP_TOKEN="$(gh auth token 2>/dev/null)" || STRIH_LX_PREP_TOKEN=""
  [ -n "$STRIH_LX_PREP_TOKEN" ] || { _strih_lx_fail tree 1 3 "no GH token (STRIH_LX_GH_TOKEN / gh auth token) -- setup-strih.sh needs GH_TOKEN for the bundle-state + bkshading fetch"; return; }
  echo "# strih-lx: provisioning tree = the committed scripts/ systemd/ intercom/ at $tree_rev; target ${STRIH_LX_PREP_USER}@${STRIH_LX_PREP_HOST}:$STRIH_LX_PREP_STAGE"
}

# strih_lx_apply SHA -> the box steps, after strih_lx_prepare (and every other box's resolution)
# succeeded. 0 only after the fail-closed read-back passed; 4 with a named step otherwise.
strih_lx_apply() {
  local sha="$1" w="$STRIH_LX_PREP_WORK" stage="$STRIH_LX_PREP_STAGE" rc out line

  # [preflight] never race a previous install (the sweep would delete the stage it is reading).
  out="$(_strih_lx_ssh "$(strih_lx_remote_installer_cmd)")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail preflight "$rc" 4 "cannot reach ${STRIH_LX_PREP_HOST} over ssh -- nothing changed"; return; }
  [ "$(printf '%s' "$out" | tr -d '[:space:]')" = "idle" ] || { _strih_lx_fail preflight 1 4 "a previous setup-strih.sh is still running on the box (pgrep -x setup-strih.sh) -- wait for it; nothing changed"; return; }

  # [sweep] stage created + touched newest FIRST, then the retention sweep keeps only it.
  _strih_lx_ssh "$(strih_lx_remote_prep_cmd "$stage")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "creating $stage on the box failed -- nothing staged, nothing stopped"; return; }
  _strih_lx_ssh "$(strih_lx_remote_sweep_cmd)" < "$w/repo/scripts/obs-backup-retention.sh"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "the obs-backup-retention --local-sweep of stale /tmp/genlock-stage-* failed -- nothing staged, nothing stopped"; return; }
  out="$(_strih_lx_ssh "$(strih_lx_remote_stage_check_cmd "$stage")")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "$stage is not intact after the sweep -- refusing to continue"; return; }
  while IFS= read -r line; do
    case "$line" in LEFTOVER\ *) echo "WARNING: [strih-lx sweep] could not remove ${line#LEFTOVER } -- it still counts against the /tmp quota" >&2 ;; esac
  done <<< "$out"

  # [stage] every byte on the box BEFORE anything is stopped.
  _strih_lx_rsync "$w/bundle/" "$stage/bundle/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the bundle to $stage/bundle failed (rc 11/23 = the /tmp quota) -- OBS NOT stopped, the old build keeps running"; return; }
  _strih_lx_rsync "$w/repo/" "$stage/repo/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the provisioning tree to $stage/repo failed -- OBS NOT stopped, the old build keeps running"; return; }
  echo "# strih-lx: staged $stage/{bundle,repo}"

  # [stop] delegated to the sanctioned stop code.
  _strih_lx_ssh "$(strih_lx_remote_stop_cmd)"; rc=$?
  if [ "$rc" != 0 ]; then
    _strih_lx_start_best_effort
    _strih_lx_fail stop "$rc" 4 "the graceful strih-obs stop did not complete -- nothing installed"; return
  fi

  # [setup] detached as root; the token rides the launch's stdin only.
  _strih_lx_ssh "$(strih_lx_remote_setup_launch_cmd "$stage")" < <(printf '%s\n' "$STRIH_LX_PREP_PW"; printf 'ghtoken:%s\n' "$STRIH_LX_PREP_TOKEN"); rc=$?
  STRIH_LX_PREP_TOKEN=""
  if [ "$rc" != 0 ]; then
    # The preflight saw no installer, so one running now is THIS deploy's (the ssh dropped after the
    # detach -- exactly what the detach is for): follow it through the rc poll below.
    case "$(_strih_lx_installer_state)" in
      idle)
        _strih_lx_start_best_effort
        _strih_lx_fail setup "$rc" 4 "launching setup-strih.sh failed (sudo / run-setup.sh) -- nothing installed"; return ;;
      unreachable)
        _strih_lx_fail setup "$rc" 4 "the launch ssh returned rc=$rc and the box is unreachable -- the installer state is unknown; NOT starting OBS: check $stage/setup-strih.rc once the box answers"; return ;;
    esac
    echo "WARNING: [strih-lx setup] the launch ssh returned rc=$rc but setup-strih.sh is running -- following it through the rc poll" >&2
  fi
  local polls="${STRIH_LX_SETUP_POLLS:-270}" secs="${STRIH_LX_SETUP_POLL_SECS:-10}" i=0 src=""
  while [ "$i" -lt "$polls" ]; do
    src="$(_strih_lx_ssh "$(strih_lx_remote_setup_rc_cmd "$stage")" 2>/dev/null | tr -d '[:space:]')"
    [ -n "$src" ] && break
    i=$((i + 1)); sleep "$secs"
  done
  if [ -z "$src" ]; then
    if [ "$(_strih_lx_installer_state)" != idle ]; then
      _strih_lx_fail setup 124 4 "setup-strih.sh wrote no rc after $polls polls x ${secs} s and is still running (or the box is unreachable) -- NOT starting OBS over it; wait for $stage/setup-strih.rc"; return
    fi
    echo "# strih-lx: setup-strih.log tail:"; _strih_lx_ssh "$(strih_lx_remote_setup_log_cmd "$stage")" || true
    _strih_lx_start_best_effort
    _strih_lx_fail setup 124 4 "setup-strih.sh wrote no rc and is no longer running (killed?)"; return
  fi
  if [ "$src" != 0 ]; then
    echo "# strih-lx: setup-strih.log tail:"; _strih_lx_ssh "$(strih_lx_remote_setup_log_cmd "$stage")" || true
    _strih_lx_start_best_effort
    _strih_lx_fail setup "$src" 4 "setup-strih.sh exited rc=$src (log above, $stage/setup-strih.log) -- the deploy is FAILED"; return
  fi
  echo "# strih-lx: setup-strih.sh rc=0"
  out="$(_strih_lx_ssh "$(strih_lx_remote_setup_notes_cmd "$stage")" 2>/dev/null)" || out=""
  while IFS= read -r line; do
    [ -n "$line" ] && echo "# strih-lx: NOTE from setup-strih.sh: $line"
  done <<< "$out"

  # [start]
  _strih_lx_ssh "$(strih_lx_remote_start_cmd "$stage")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail start "$rc" 4 "systemctl --user start strih-obs.service failed"; return; }

  _strih_lx_verify "$sha"
}

# [verify] fail-closed: every field must pass, and the SAME non-zero MainPID + NRestarts must hold from
# the first good poll for STRIH_LX_VERIFY_SETTLE_SECS (default 90 -- a crash loop can be slower than
# one poll interval: the CEF trap on this box fired every ~60 s). A bad poll restarts the window.
# Bash SECONDS follows the wall clock: dev1 is dantesync-disciplined (slews, a step is itself an
# alarm), and the settle is a lower bound on observed stability, so a step can only shift it by the
# step size -- accepted rather than a /proc/uptime dependency.
_strih_lx_verify() {
  local sha="$1" vpolls="${STRIH_LX_VERIFY_POLLS:-24}" vsecs="${STRIH_LX_VERIFY_POLL_SECS:-10}"
  local settle="${STRIH_LX_VERIFY_SETTLE_SECS:-90}"
  local i=0 have_first=0 first_pid="" first_r="" first_t=0 line pid restarts bs verdict="FAIL: no read-back" url
  url="$(strih_lx_bundle_state_url "$STRIH_LX_PREP_HOST")"
  while [ "$i" -lt "$vpolls" ]; do
    [ "$i" -gt 0 ] && sleep "$vsecs"
    i=$((i + 1))
    line="$(_strih_lx_ssh "$(strih_lx_remote_readback_cmd "$STRIH_LX_PREP_STAGE")" 2>/dev/null)" || line=""
    pid="$(_strih_lx_field pid "$line")"; restarts="$(_strih_lx_field restarts "$line")"
    bs="$(curl -s -m 8 "$url" 2>/dev/null | jq -r '.genlock_build_sha // empty' 2>/dev/null)" || bs=""
    if ! verdict="$(strih_lx_deploy_verdict "$sha" "$(_strih_lx_field installed "$line")" "$(_strih_lx_field active "$line")" "$bs" "$STRIH_LX_PREP_LIBSHA" "$(_strih_lx_field lib "$line")" "$(_strih_lx_field tick "$line")")"; then
      have_first=0; continue
    fi
    if [ "$have_first" = 1 ]; then
      if verdict="$(strih_lx_stable_verdict "$first_pid" "$first_r" "$pid" "$restarts")"; then
        if [ $((SECONDS - first_t)) -ge "$settle" ]; then
          echo "# strih-lx: VERIFIED -- marker + libobs bytes = $sha, strih-obs active (MainPID $pid stable for $((SECONDS - first_t)) s, NRestarts $restarts), render tick ENABLED, :8899=$bs"
          return 0
        fi
        verdict="FAIL: MainPID $pid stable for $((SECONDS - first_t)) s only (settle ${settle} s)"
        continue
      fi
    else
      verdict="FAIL: only one good read-back -- the MainPID/NRestarts stability needs a later poll"
    fi
    have_first=1; first_pid="$pid"; first_r="$restarts"; first_t="$SECONDS"
  done
  _strih_lx_fail verify 1 4 "post-deploy read-back refused after $vpolls polls: $(printf '%s' "$verdict" | tr '\n' ';') (see $url)"
}

# --- plan arm -------------------------------------------------------------------------------------

# strih_lx_plan_steps LOCAL_STAGE SHA HOST RUN_ID -> the strih-lx part of `--plan`: the SAME remote
# commands the execute arm runs (built by the builders above), every line a #-comment so a saved
# whole-plan .ps1 still parses (the issue-1295 file-mode rule). Pure.
strih_lx_plan_steps() {
  local lstage="$1" sha="$2" host="$3" run="$4" stage art u="${STRIH_LX_USER:-newlevel}"
  stage="$(strih_lx_stage_dir "$sha")" || return 2
  art="$(fleet_linux_bundle_artifact_for strih-lx)"
  cat <<PLAN
# EXECUTE mode runs every step below itself, fail-loud (a failed step exits 4 and names itself; the
#   old build keeps running until the whole stage is on the box; every requested box is resolved
#   before any box is changed):
#     scripts/deploy-genlock-fleet.sh --run-id ${run} --boxes strih-lx
#   strih-lx always gets the strih FULL build (${art}); --fast is Windows-only.
#   Every ssh below is 'sshpass -p \$PW timeout 180 ssh ${STRIH_LX_SSH_OPTS} ${u}@${host} <cmd>'.
# STEP 0 (resolve): the linux-genlock.yml run at ${sha}; download its '${art}'
#          artifact to ${lstage}/bundle -- REFUSED unless its GENLOCK_BUILD_SHA.txt is ${sha} and its
#          BUNDLE_MANIFEST.json carries lib/x86_64-linux-gnu/libobs.so.30.
# STEP 1 (tree): the committed scripts/ systemd/ intercom/ of this checkout (archive of HEAD) + the
#          generated run-setup.sh -> ${lstage}/repo.
# STEP 2 (preflight): refuse while a previous install runs:  $(strih_lx_remote_installer_cmd)
# STEP 3 (sweep): the stage is created + touched NEWEST, then the obs-backup-retention.sh
#          --local-sweep decision (what 'obs-backup-retention.sh --box strih-lx' runs), stage dirs
#          only, as the operator, keeps only it:
#            $(strih_lx_remote_prep_cmd "$stage")
#            $(strih_lx_remote_sweep_cmd)   < scripts/obs-backup-retention.sh (the committed tree)
#            $(strih_lx_remote_stage_check_cmd "$stage")
# STEP 4 (stage, OBS still running):
#            rsync -a --delete --timeout=120 -e "ssh ${STRIH_LX_SSH_OPTS}" ${lstage}/bundle/ ${u}@${host}:${stage}/bundle/
#            rsync -a --delete --timeout=120 -e "ssh ${STRIH_LX_SSH_OPTS}" ${lstage}/repo/ ${u}@${host}:${stage}/repo/
# STEP 5 (stop, delegated to the sanctioned stop code; the deploy sends no kill itself):
#            $(strih_lx_remote_stop_cmd)
# STEP 6 (setup, detached on the box): setup-strih.sh as root with STRIH_LX_BUNDLE_SRC=${stage}/bundle
#          STRIH_LX_IP=${host} and GH_TOKEN read from stdin (a 'ghtoken:<token>' line, never argv):
#            $(strih_lx_remote_setup_launch_cmd "$stage")   < (the sudo password line + ghtoken:<token>)
#          then poll: $(strih_lx_remote_setup_rc_cmd "$stage")  (0 = done; else the log tail + FAIL;
#          OBS is never started while setup-strih.sh still runs)
# STEP 7 (start): $(strih_lx_remote_start_cmd "$stage")
# STEP 8 (verify, fail-closed, held for ${STRIH_LX_VERIFY_SETTLE_SECS:-90} s): /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == ${sha},
#          /usr/lib/x86_64-linux-gnu/libobs.so.30 sha256 == the manifest's, strih-obs.service active with
#          the SAME MainPID + NRestarts, 'render tick ENABLED' in the OBS log written after the start,
#          and $(strih_lx_bundle_state_url "$host") genlock_build_sha == ${sha}.
# ACCEPTANCE (supervisor, after a green execute): 'bash ${stage}/repo/scripts/verify-strih.sh' ON the box,
#          and from dev1 'python3 scripts/obs_burn_filter.py check --host ${host}' (the WS filter-enum survives).
PLAN
}
