#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no top-level statements) -- the
# scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing executes it in the
# CALLER's shell (deploy-genlock-fleet.sh sets its own strict mode). Every step below checks its own
# rc explicitly and returns a named failure, because the caller runs it in an `||` context where
# errexit is off by design.
#
# scripts/lib/strih-lx-deploy.sh -- issue 1317 part 6: the strih-lx EXECUTE arm of
# scripts/deploy-genlock-fleet.sh, plus the builders its --plan arm prints (so plan == execute by
# construction).
#
# strih-lx (10.77.9.202, a linux-genlock box since the M4 cut-over) installs the strih FULL genlock
# artifact through scripts/setup-strih.sh (release-parity gate, runtime packages, /usr prefix,
# chrome-sandbox) and runs OBS under the strih-obs.service user unit. Before this lib the supervisor
# deployed it with an ad-hoc scratch script that twice failed half-silently: the box's /tmp quota was
# full (each staged bundle is ~2.2 GB on a 7.5 GB tmpfs), rsync died `Disk quota exceeded` (rc 11),
# and the old build simply kept running with nothing refusing. The arm here:
#
#   1. resolve  -- the linux-genlock.yml run at the anchor's SAME SHA (fleet_pick_run_at_sha);
#   2. download -- the strih FULL artifact; REFUSE one whose own GENLOCK_BUILD_SHA.txt is another SHA;
#   3. tree     -- the committed scripts/ systemd/ intercom/ (the version-control archive of HEAD of
#                  the checkout this runs from -- committed bytes only) + the generated run-setup.sh;
#   4. sweep    -- the stage being deployed is created + touched NEWEST, then the EXISTING
#                  obs-backup-retention.sh --local-sweep decision (keep the newest 1, days 0) runs on
#                  the box -- every older /tmp/genlock-stage-<sha> goes, the deploying one never does;
#   5. stage    -- rsync bundle/ + repo/ into /tmp/genlock-stage-<sha>/ WHILE THE OLD OBS KEEPS
#                  RUNNING: a failed rsync exits 4 naming the step before anything is stopped;
#   6. stop     -- the sanctioned stop code (/usr/local/bin/strih-obs-stop.sh, routed through
#                  systemctl --user stop) + a bounded wait; the deploy never force-kills anything;
#   7. setup    -- setup-strih.sh as root, DETACHED on the box (an ssh drop cannot kill it mid-apt),
#                  the GH token on the launch's STDIN only (never an argv, never a file), rc polled;
#   8. start    -- systemctl --user start strih-obs.service;
#   9. verify   -- REFUSE (exit 4) unless /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == the canonical
#                  SHA, strih-obs.service is active, and :8899 bundle-state reports that SHA.
#
# Exit codes (returned to deploy-genlock-fleet.sh main, which exits with them): 3 = resolution /
# download / local preparation failed (the box was never touched); 4 = a box step failed. Every
# failure prints `ERROR: [strih-lx <step>] ... (rc=N): <what>` on stderr.
#
# Test seams (tests/deploy_genlock_fleet_strih_lx_exec_1317.rs stubs gh/sshpass/ssh/rsync/curl on
# PATH): STRIH_LX_SETUP_POLLS / STRIH_LX_SETUP_POLL_SECS (setup rc poll, default 270 x 10 s = 45 min),
# STRIH_LX_VERIFY_POLLS / STRIH_LX_VERIFY_POLL_SECS (read-back poll, default 24 x 10 s = 4 min).
# Transport env: STRIH_LX_IP (dial override, via fleet_box_ip), STRIH_LX_USER / STRIH_LX_PW
# (default newlevel / newlevel -- the rig's shared Linux-box creds, targets.md).

STRIH_LX_STAGE_PARENT="/tmp"
STRIH_LX_RETENTION_ARGS="--local-sweep --backup-root /opt/obs-backup --stage-parent /tmp --keep-runs 1 --keep-days 0 --execute"

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

# create the stage dirs and touch the stage so it is the NEWEST stage dir -> the keep-newest-1 sweep
# can never delete the stage being deployed.
strih_lx_remote_prep_cmd() {
  printf "mkdir -p '%s/bundle' '%s/repo' && touch '%s'\n" "$1" "$1" "$1"
}

# the obs-backup-retention.sh --local-sweep leg (the same leg `obs-backup-retention.sh --box strih-lx`
# runs over ssh, dialled at the deploy's own host so a STRIH_LX_IP override sweeps the right box):
# stdin = the sudo password line, then the retention script itself as the program. `-k` makes sudo
# always consume the password line, so it can never be run as the first line of the program.
strih_lx_remote_sweep_cmd() {
  printf "sudo -k -S -p '' bash -s -- %s\n" "$STRIH_LX_RETENTION_ARGS"
}

strih_lx_remote_stage_present_cmd() {
  printf "test -d '%s/bundle' && test -d '%s/repo'\n" "$1" "$1"
}

# the graceful stop: the sanctioned strih-obs-stop.sh (plain mode routes through `systemctl --user
# stop`, so the unit's ExecStop runs and Restart=on-failure does not relaunch it), then a bounded wait
# for BOTH the unit and every obs process to be gone. A stop that does not complete is exit 5 (a
# refusal) -- never an escalation to a hard kill from here.
strih_lx_remote_stop_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf '%s\n' 'export XDG_RUNTIME_DIR=/run/user/$(id -u); if [ -x /usr/local/bin/strih-obs-stop.sh ]; then /usr/local/bin/strih-obs-stop.sh || exit 5; else systemctl --user stop strih-obs.service || exit 5; fi; i=0; while systemctl --user is-active --quiet strih-obs.service || pgrep -x obs >/dev/null; do i=$((i+1)); if [ "$i" -gt 30 ]; then echo "strih-obs/obs still running 30 s after the graceful stop" >&2; exit 5; fi; sleep 1; done; echo "strih-obs stopped (graceful)"'
}

# launch the staged run-setup.sh as root. stdin = the sudo password line, then `ghtoken:<token>`.
# `sudo -k` ignores any cached credential so the password line is always consumed by sudo.
strih_lx_remote_setup_launch_cmd() {
  printf "sudo -k -S -p '' bash '%s/repo/run-setup.sh'\n" "$1"
}

strih_lx_remote_setup_rc_cmd() {
  printf "cat '%s/setup-strih.rc' 2>/dev/null || true\n" "$1"
}

strih_lx_remote_setup_log_cmd() {
  printf "tail -n 60 '%s/setup-strih.log' 2>/dev/null || true\n" "$1"
}

strih_lx_remote_start_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf '%s\n' 'export XDG_RUNTIME_DIR=/run/user/$(id -u); systemctl --user reset-failed strih-obs.service 2>/dev/null; systemctl --user start strih-obs.service'
}

# one line `installed=<marker sha> active=<unit state>` for the read-back.
strih_lx_remote_readback_cmd() {
  # shellcheck disable=SC2016  # expanded on the box, not here
  printf '%s\n' 'export XDG_RUNTIME_DIR=/run/user/$(id -u); printf "installed=%s active=%s\n" "$(tr -d "[:space:]" < /opt/obs-genlock/GENLOCK_BUILD_SHA.txt 2>/dev/null)" "$(systemctl --user is-active strih-obs.service 2>/dev/null)"'
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

# strih_lx_deploy_verdict WANT INSTALLED ACTIVE BS_SHA -> `OK` (rc 0) or `FAIL: <why>` lines (rc 1).
# Fail-closed: an empty value anywhere is a FAIL, never "unknown = fine". Pure.
strih_lx_deploy_verdict() {
  local want="${1:-}" installed="${2:-}" active="${3:-}" bs="${4:-}" bad=0
  if [ -z "$want" ]; then echo "FAIL: no canonical SHA to compare against"; return 1; fi
  if [ "$installed" != "$want" ]; then
    echo "FAIL: /opt/obs-genlock/GENLOCK_BUILD_SHA.txt=${installed:-<unreadable>} != canonical ${want}"; bad=1
  fi
  if [ "$active" != "active" ]; then
    echo "FAIL: strih-obs.service is ${active:-<unreadable>}, not active"; bad=1
  fi
  if [ "$bs" != "$want" ]; then
    echo "FAIL: :8899 genlock_build_sha=${bs:-<unreadable>} != canonical ${want}"; bad=1
  fi
  [ "$bad" = 0 ] && { echo "OK"; return 0; }
  return 1
}

# --- execute arm ----------------------------------------------------------------------------------

_strih_lx_fail() {  # STEP RC EXIT MESSAGE -> prints the named error, returns EXIT
  echo "ERROR: [strih-lx $1] failed (rc=$2): $4" >&2
  return "$3"
}

# _strih_lx_ssh CMD -> run CMD on the box (stdin passes through). Uses the caller's
# _lx_host/_lx_user/_lx_pw locals.
_strih_lx_ssh() {
  sshpass -p "$_lx_pw" ssh -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no \
    -o LogLevel=ERROR -o ConnectTimeout=12 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 \
    "${_lx_user}@${_lx_host}" "$1"
}

_strih_lx_rsync() {  # SRC_DIR/ DEST_DIR/
  sshpass -p "$_lx_pw" rsync -a --delete \
    -e "ssh -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR -o ConnectTimeout=12 -o ServerAliveInterval=15 -o ServerAliveCountMax=4" \
    "$1" "${_lx_user}@${_lx_host}:$2"
}

# strih_lx_execute_deploy SHA WORKDIR REPO GENLOCK_REPO -> deploys the strih FULL build at SHA to
# strih-lx. REPO = the checkout root whose committed scripts/ systemd/ intercom/ are staged. Returns
# 0 only after the fail-closed read-back passed; 3/4 with a named error otherwise.
strih_lx_execute_deploy() {
  local sha="$1" work="$2" repo="$3" gh_repo="$4"
  local _lx_host _lx_user="${STRIH_LX_USER:-newlevel}" _lx_pw="${STRIH_LX_PW:-newlevel}"
  local stage art lrun rc tok w="$work/strih-lx"
  _lx_host="$(fleet_box_ip strih-lx)" || { _strih_lx_fail resolve 2 3 "no strih-lx host (STRIH_LX_IP / obs-fleet row)"; return; }
  stage="$(strih_lx_stage_dir "$sha")" || { _strih_lx_fail resolve 2 3 "canonical SHA '$sha' is not a hex commit id"; return; }
  art="$(fleet_linux_bundle_artifact_for strih-lx)"
  for tool in sshpass rsync curl tar; do
    command -v "$tool" >/dev/null 2>&1 || { _strih_lx_fail resolve 127 3 "$tool is required for the strih-lx deploy"; return; }
  done
  echo "# strih-lx: deploying the strih FULL build at $sha to ${_lx_user}@${_lx_host} (stage $stage)"

  # [1 resolve] the linux-genlock.yml run at the SAME SHA.
  lrun="$(gh run list --repo "$gh_repo" --workflow linux-genlock.yml --json databaseId,headSha,conclusion --limit 50 2>/dev/null | fleet_pick_run_at_sha "$sha")" || true
  [ -n "$lrun" ] || { _strih_lx_fail resolve 1 3 "no successful linux-genlock.yml run at $sha -- build the strih artifact at that commit first (a tag dispatch)"; return; }

  # [2 download] the strih FULL artifact, refused unless it is really the canonical SHA.
  mkdir -p "$w/bundle" "$w/repo" || { _strih_lx_fail download 1 3 "cannot create $w"; return; }
  gh run download "$lrun" --repo "$gh_repo" -n "$art" -D "$w/bundle"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail download "$rc" 3 "download of $art (run $lrun) failed"; return; }
  [ -f "$w/bundle/bin/obs" ] || { _strih_lx_fail download 1 3 "$art has no bin/obs"; return; }
  [ -d "$w/bundle/lib/x86_64-linux-gnu/obs-plugins" ] || { _strih_lx_fail download 1 3 "$art has no lib/x86_64-linux-gnu/obs-plugins"; return; }
  local art_sha; art_sha="$(tr -d '[:space:]' < "$w/bundle/GENLOCK_BUILD_SHA.txt" 2>/dev/null)"
  [ "$art_sha" = "$sha" ] || { _strih_lx_fail download 1 3 "$art (run $lrun) carries GENLOCK_BUILD_SHA ${art_sha:-<none>}, not the canonical $sha -- refusing to stage a foreign build"; return; }
  echo "# strih-lx: $art (run $lrun) downloaded, GENLOCK_BUILD_SHA=$art_sha"

  # [3 tree] the COMMITTED provisioning tree of this checkout + the generated on-box runner.
  local tree_rev; tree_rev="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)" || tree_rev=""
  [ -n "$tree_rev" ] || { _strih_lx_fail tree 1 3 "$repo is not a git checkout"; return; }
  git -C "$repo" archive --format=tar HEAD scripts systemd intercom | tar -x -C "$w/repo"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail tree "$rc" 3 "archiving scripts/ systemd/ intercom/ at $tree_rev failed"; return; }
  for f in scripts/setup-strih.sh systemd/strih-obs.service intercom/intercom.strih-lx.toml; do
    [ -f "$w/repo/$f" ] || { _strih_lx_fail tree 1 3 "the provisioning tree at $tree_rev has no $f"; return; }
  done
  strih_lx_setup_runner "$stage" "$_lx_host" > "$w/repo/run-setup.sh" || { _strih_lx_fail tree 1 3 "cannot write run-setup.sh"; return; }
  tok="$(gh auth token 2>/dev/null)" || tok=""
  [ -n "$tok" ] || { _strih_lx_fail tree 1 3 "gh auth token is empty -- setup-strih.sh needs GH_TOKEN (bundle-state + bkshading fetch)"; return; }
  echo "# strih-lx: provisioning tree = the committed scripts/ systemd/ intercom/ at $tree_rev"

  # [4 sweep] stage created + touched newest FIRST, then the retention sweep keeps only it.
  _strih_lx_ssh "$(strih_lx_remote_prep_cmd "$stage")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "creating $stage on the box failed (ssh reachable?)"; return; }
  _strih_lx_ssh "$(strih_lx_remote_sweep_cmd)" < <(printf '%s\n' "$_lx_pw"; cat "$repo/scripts/obs-backup-retention.sh"); rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "the obs-backup-retention --local-sweep of stale /tmp/genlock-stage-* failed -- nothing staged, nothing stopped"; return; }
  _strih_lx_ssh "$(strih_lx_remote_stage_present_cmd "$stage")"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail sweep "$rc" 4 "$stage is gone after the sweep -- refusing to continue"; return; }

  # [5 stage] every byte on the box BEFORE anything is stopped.
  _strih_lx_rsync "$w/bundle/" "$stage/bundle/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the bundle to $stage/bundle failed (rc 11/23 = the /tmp quota) -- OBS NOT stopped, the old build keeps running"; return; }
  _strih_lx_rsync "$w/repo/" "$stage/repo/"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stage "$rc" 4 "rsync of the provisioning tree to $stage/repo failed -- OBS NOT stopped, the old build keeps running"; return; }
  echo "# strih-lx: staged $stage/{bundle,repo}"

  # [6 stop] the graceful stop.
  _strih_lx_ssh "$(strih_lx_remote_stop_cmd)"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail stop "$rc" 4 "the graceful strih-obs stop did not complete -- nothing installed (no hard kill from the deploy)"; return; }

  # [7 setup] setup-strih.sh detached as root; the token rides the launch's stdin only.
  _strih_lx_ssh "$(strih_lx_remote_setup_launch_cmd "$stage")" < <(printf '%s\n' "$_lx_pw"; printf 'ghtoken:%s\n' "$tok"); rc=$?
  tok=""
  if [ "$rc" != 0 ]; then
    _strih_lx_ssh "$(strih_lx_remote_start_cmd)" || true
    _strih_lx_fail setup "$rc" 4 "launching setup-strih.sh failed (sudo / run-setup.sh) -- restarted the previously installed OBS best-effort"; return
  fi
  local polls="${STRIH_LX_SETUP_POLLS:-270}" secs="${STRIH_LX_SETUP_POLL_SECS:-10}" i=0 src=""
  while [ "$i" -lt "$polls" ]; do
    src="$(_strih_lx_ssh "$(strih_lx_remote_setup_rc_cmd "$stage")" 2>/dev/null | tr -d '[:space:]')"
    [ -n "$src" ] && break
    i=$((i + 1)); sleep "$secs"
  done
  if [ -z "$src" ] || [ "$src" != 0 ]; then
    echo "# strih-lx: setup-strih.log tail:"
    _strih_lx_ssh "$(strih_lx_remote_setup_log_cmd "$stage")" || true
    _strih_lx_ssh "$(strih_lx_remote_start_cmd)" || true
    if [ -z "$src" ]; then
      _strih_lx_fail setup 124 4 "setup-strih.sh wrote no rc after $polls polls x ${secs} s -- it may still be running (pgrep -x setup-strih.sh on the box); started strih-obs best-effort"; return
    fi
    _strih_lx_fail setup "$src" 4 "setup-strih.sh exited rc=$src (log above, $stage/setup-strih.log) -- restarted strih-obs best-effort; the deploy is FAILED"; return
  fi
  echo "# strih-lx: setup-strih.sh rc=0"

  # [8 start]
  _strih_lx_ssh "$(strih_lx_remote_start_cmd)"; rc=$?
  [ "$rc" = 0 ] || { _strih_lx_fail start "$rc" 4 "systemctl --user start strih-obs.service failed"; return; }

  # [9 verify] fail-closed read-back: marker, unit, :8899 -- all must equal the canonical SHA.
  local vpolls="${STRIH_LX_VERIFY_POLLS:-24}" vsecs="${STRIH_LX_VERIFY_POLL_SECS:-10}" line installed active bs verdict url
  url="$(strih_lx_bundle_state_url "$_lx_host")"
  i=0
  while :; do
    line="$(_strih_lx_ssh "$(strih_lx_remote_readback_cmd)" 2>/dev/null)" || line=""
    installed="$(printf '%s\n' "$line" | sed -n 's/.*installed=\([^ ]*\).*/\1/p' | head -n 1)"
    active="$(printf '%s\n' "$line" | sed -n 's/.*active=\([^ ]*\).*/\1/p' | head -n 1)"
    bs="$(curl -s -m 8 "$url" 2>/dev/null | jq -r '.genlock_build_sha // empty' 2>/dev/null)" || bs=""
    if verdict="$(strih_lx_deploy_verdict "$sha" "$installed" "$active" "$bs")"; then
      echo "# strih-lx: VERIFIED -- installed=$installed strih-obs=$active :8899=$bs (canonical $sha)"
      return 0
    fi
    i=$((i + 1))
    [ "$i" -lt "$vpolls" ] || break
    sleep "$vsecs"
  done
  _strih_lx_fail verify 1 4 "post-deploy read-back refused after $vpolls polls: $(printf '%s' "$verdict" | tr '\n' ';') (see $url)"
}

# --- plan arm -------------------------------------------------------------------------------------

# strih_lx_plan_steps LOCAL_STAGE SHA HOST RUN_ID -> the strih-lx part of `--plan`: the SAME commands
# the execute arm runs (built by the builders above), every line a #-comment so a saved whole-plan
# .ps1 still parses (the issue-1295 file-mode rule). Pure.
strih_lx_plan_steps() {
  local lstage="$1" sha="$2" host="$3" run="$4" stage art u="${STRIH_LX_USER:-newlevel}"
  stage="$(strih_lx_stage_dir "$sha")" || return 2
  art="$(fleet_linux_bundle_artifact_for strih-lx)"
  cat <<PLAN
# EXECUTE mode runs every step below itself, fail-loud (a failed step exits 4 and names itself; the
#   old build keeps running until the whole stage is on the box):
#     scripts/deploy-genlock-fleet.sh --run-id ${run} --boxes strih-lx
#   strih-lx always gets the strih FULL build (${art}); --fast is Windows-only.
# STEP 0 (resolve): the linux-genlock.yml run at ${sha}; download its '${art}'
#          artifact to ${lstage}/bundle -- REFUSED unless its GENLOCK_BUILD_SHA.txt is ${sha}.
# STEP 1 (tree): the committed scripts/ systemd/ intercom/ of this checkout (archive of HEAD) + the
#          generated run-setup.sh -> ${lstage}/repo.
# STEP 2 (sweep): the stage is created + touched NEWEST, then the obs-backup-retention.sh
#          --local-sweep decision (what 'obs-backup-retention.sh --box strih-lx' runs) keeps only it:
#            ssh ${u}@${host} "$(strih_lx_remote_prep_cmd "$stage")"
#            ssh ${u}@${host} "$(strih_lx_remote_sweep_cmd)"   < (the sudo password line + scripts/obs-backup-retention.sh)
# STEP 3 (stage, OBS still running):
#            rsync -a --delete ${lstage}/bundle/ ${u}@${host}:${stage}/bundle/
#            rsync -a --delete ${lstage}/repo/ ${u}@${host}:${stage}/repo/
# STEP 4 (graceful stop, never a hard kill):
#            ssh ${u}@${host} '$(strih_lx_remote_stop_cmd)'
# STEP 5 (setup, detached on the box): setup-strih.sh as root with STRIH_LX_BUNDLE_SRC=${stage}/bundle
#          STRIH_LX_IP=${host} and GH_TOKEN read from stdin (a 'ghtoken:<gh auth token>' line, never argv):
#            ssh ${u}@${host} "$(strih_lx_remote_setup_launch_cmd "$stage")"   < (the sudo password line + ghtoken:<token>)
#          then poll: ssh ${u}@${host} "$(strih_lx_remote_setup_rc_cmd "$stage")"  (0 = done; else the log tail + FAIL)
# STEP 6 (start): ssh ${u}@${host} '$(strih_lx_remote_start_cmd)'
# STEP 7 (verify, fail-closed): /opt/obs-genlock/GENLOCK_BUILD_SHA.txt == ${sha}, strih-obs.service active,
#          and $(strih_lx_bundle_state_url "$host") genlock_build_sha == ${sha}.
# ACCEPTANCE (supervisor, after a green execute): 'bash ${stage}/repo/scripts/verify-strih.sh' ON the box,
#          and from dev1 'python3 scripts/obs_burn_filter.py check --host ${host}' (the WS filter-enum survives).
PLAN
}
