"""issue 1426 (design 6213361899): the fleet deploy planner deploys the PP Linux OBS boxes too.

`scripts/deploy-genlock-fleet.sh` knew only strih-snv, stream-snv, imag and resolume-snv, so the
13.10.2026 PP roll was done by hand and strih-pp went back to an older build the next day with no tool
noticing until the fleet-parity watchdog. Now every obs-fleet `linux-genlock` box with a
`scripts/strih-boxes/<box>.env` fact file runs the SAME Linux execute arm
(`scripts/lib/strih-snv-deploy.sh`) with its own box name:

* the ssh host is the obs-fleet row's NAME (`strih-pp.lan` / `stream-pp.lan`, reached from dev1
  through companion-pp), never the fact file's venue STRIH_IP (10.77.8.x is not routable from dev1);
* a `traveling` box that does not answer is reported AWAY by name and skipped, never failed;
* a failed acceptance on one box stops only that box (exit 5 naming it);
* the strih-snv plan stays byte-identical to the one before this change (the golden).

The execute tests drive the REAL script with gh / sshpass / ssh / rsync / curl stubbed on PATH and a
fake obs_phase2.py on the STRIH_LX_OBS_PHASE2_DIR seam, so no rig and no network is touched.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
FLEET = REPO / "scripts" / "deploy-genlock-fleet.sh"
GOLDEN = REPO / "tests" / "fixtures" / "deploy_genlock_fleet_plan_strih_snv_1426.txt"
SHA = "abc123def4567890"
LIBSHA = "libobs-bytes-good"
DATE_RE = re.compile(r"^#   \d{4}-\d{2}-\d{2}T[^\t]*\t", re.M)


def _env(extra=None, home="/home/newlevel"):
    env = {"PATH": os.environ["PATH"], "HOME": home, "LC_ALL": "C.UTF-8"}
    env.update(extra or {})
    return env


def _plan(boxes, mode="--full", extra=None):
    r = subprocess.run(
        ["bash", str(FLEET), "--plan", "--run-id", "4242", "--sha", SHA, "--stage", "/stage", mode,
         "--boxes", boxes],
        capture_output=True, text=True, cwd=REPO, env=_env(extra), timeout=120)
    return r.returncode, DATE_RE.sub("#   <DATE>\t", r.stdout), r.stderr


def _sourced(body):
    script = 'set -euo pipefail\n. "%s"\n%s\n' % (FLEET, body)
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, cwd=REPO,
                          env=_env(), timeout=60)


# --- plan ------------------------------------------------------------------------------------------

def test_the_strih_snv_plan_is_byte_identical_to_the_golden():
    rc, out, err = _plan("strih-snv")
    assert rc == 0, err
    assert out == GOLDEN.read_text(), "the strih-snv plan changed (issue 1426 must keep it byte-identical)"


def test_the_pp_plan_runs_the_setup_strih_arm_per_box_dialling_the_fleet_name():
    rc, out, err = _plan("stream-pp,strih-pp")
    assert rc == 0, err
    assert "boxes=strih-pp,stream-pp" in out, "canonical order: strih-pp before stream-pp"
    head_strih = out.index("# ================= FLEET PLAN: box=strih-pp (ssh newlevel@strih-pp.lan, linux-genlock)")
    head_stream = out.index("# ================= FLEET PLAN: box=stream-pp (ssh newlevel@stream-pp.lan, linux-genlock)")
    assert head_strih < head_stream
    strih, stream = out[head_strih:head_stream], out[head_stream:]
    for box, part in (("strih-pp", strih), ("stream-pp", stream)):
        host = box + ".lan"
        assert "scripts/deploy-genlock-fleet.sh --run-id 4242 --boxes %s" % box in part
        assert "'setup-strih.sh --box %s'" % box in part
        assert "repo/scripts/verify-strih.sh' --box %s" % box in part
        assert "newlevel@%s:/tmp/genlock-stage-%s/bundle/" % (host, SHA) in part
        assert "http://%s:8899/bundle-state.json" % host in part
        assert "obs-genlock-linux-x86_64-strih" in part, "a setup-strih box installs the strih FULL build"
        assert "audit %s's NDI-input forced table" % box in part
        assert "genlock_forced_table_audit %s'" % box.split("-")[0] in part
        assert "AWAY" in part, "a traveling box's plan states the AWAY skip"
        # the venue's rig-busy guard reads the PP strih, never the SNV one
        assert "reads strih strih-pp.lan" in part
        assert "--box strih-snv" not in part and "10.77.9.202" not in part
    assert "10.77.8." not in out, "never the fact file's venue STRIH_IP (not routable from dev1)"
    lines = [ln for ln in out.splitlines() if ln.strip()]
    assert lines[-1] == "exit 0"
    assert all(ln.startswith("#") for ln in lines[:-1]), "every plan line is #-comment guidance"


def test_a_pp_box_rides_along_the_snv_boxes_in_canonical_order():
    rc, out, err = _plan("resolume-snv,stream-pp,stream-snv,strih-snv,strih-pp")
    assert rc == 0, err
    assert "boxes=strih-snv,strih-pp,stream-pp,stream-snv,resolume-snv" in out
    order = [out.index("FLEET PLAN: box=%s " % b) for b in
             ("strih-snv", "strih-pp", "stream-pp", "stream-snv", "resolume-snv")]
    assert order == sorted(order)


def test_the_default_box_set_is_unchanged():
    r = _sourced("fleet_normalize_boxes ''")
    assert r.returncode == 0 and r.stdout.strip() == "strih-snv,stream-snv"


@pytest.mark.parametrize("bad", ["bogus", "resolume-pp", "strih-pp.lan"])
def test_an_unknown_or_undeployable_box_is_refused(bad):
    r = _sourced("fleet_normalize_boxes %s" % bad)
    assert r.returncode == 2 and bad in r.stderr


def test_the_dial_host_is_the_fleet_name_for_a_pp_box():
    r = _sourced("fleet_box_ip strih-pp; fleet_box_ip stream-pp; fleet_linux_bundle_artifact_for stream-pp")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["strih-pp.lan", "stream-pp.lan", "obs-genlock-linux-x86_64-strih"]


# --- execute (stubbed) -----------------------------------------------------------------------------

GH = r'''#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "$STUB_DIR/calls.log"
case "$1 $2" in
  "run view") echo "$STUB_SHA" ;;
  "run list") printf '[{"databaseId":777,"headSha":"%s","conclusion":"success"}]\n' "$STUB_SHA" ;;
  "run download")
    d=""; while [ "$#" -gt 0 ]; do [ "$1" = "-D" ] && d="$2"; shift; done
    mkdir -p "$d/bin" "$d/lib/x86_64-linux-gnu/obs-plugins"
    : > "$d/bin/obs"
    echo "$STUB_SHA" > "$d/GENLOCK_BUILD_SHA.txt"
    printf '{"files":[{"path":"lib/x86_64-linux-gnu/libobs.so.30","sha256":"%s"}]}\n' "$STUB_LIBSHA" > "$d/BUNDLE_MANIFEST.json" ;;
  "auth token") echo "tok-SECRET" ;;
  *) echo "gh stub: unhandled $*" >&2; exit 9 ;;
esac
'''

SSHPASS = r'''#!/usr/bin/env bash
printf 'sshpass %s\n' "$*" >> "$STUB_DIR/calls.log"
[ "$1" = "-p" ] && shift 2
exec "$@"
'''

# host-aware: the box answers as its own short name; STUB_SSH_DOWN lists hosts that never answer;
# STUB_ACCEPT_FAIL lists hosts whose verify-strih acceptance fails.
SSH = r'''#!/usr/bin/env bash
printf 'ssh %s\n' "$*" >> "$STUB_DIR/calls.log"
cmd="${@: -1}"; dest="${@: -2:1}"; host="${dest#*@}"
case " ${STUB_SSH_DOWN:-} " in *" $host "*) echo "ssh: Could not resolve hostname $host" >&2; exit 255 ;; esac
case " ${STUB_SSH_HUNG:-} " in *" $host "*) exit 124 ;; esac
case " ${STUB_SSH_DENIED:-} " in *" $host "*) echo "Permission denied" >&2; exit 5 ;; esac
case "$cmd" in
  hostname) h="${host%.lan}"; [ "$h" = 10.77.9.202 ] && h=strih-snv; echo "$h"; exit 0 ;;
  *"acceptance gate did not pass"*) echo 1; exit 0 ;;
  *"repo/scripts/verify-strih.sh"*)
    cat > /dev/null
    case " ${STUB_ACCEPT_FAIL:-} " in *" $host "*) echo "  FAIL dantesync offset unstable"; exit 1 ;; esac
    echo "ALL CLEAR"; exit 0 ;;
  *"pgrep -x setup-strih.sh"*) echo idle ;;
  *--local-sweep*) cat > /dev/null; exit 0 ;;
  *LEFTOVER*) exit 0 ;;
  *setup-strih.rc*) echo 1; exit 0 ;;
  *run-setup.sh*) cat > "$STUB_DIR/setup.$host.stdin"; exit 0 ;;
  *", then run verify-strih"*) exit 0 ;;
  *setup-strih.log*) echo "setup log tail"; exit 0 ;;
  *strih-obs-stop.sh*) exit 0 ;;
  *GENLOCK_BUILD_SHA.txt*)
    printf 'installed=%s active=active pid=4242 restarts=0 lib=%s tick=1\n' "$STUB_SHA" "$STUB_LIBSHA"; exit 0 ;;
  *"--user start"*) exit 0 ;;
  *) exit 0 ;;
esac
'''

RSYNC = r'''#!/usr/bin/env bash
printf 'rsync %s\n' "$*" >> "$STUB_DIR/calls.log"
exit 0
'''

CURL = r'''#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$STUB_DIR/calls.log"
printf '{"genlock_build_sha":"%s"}\n' "$STUB_SHA"
'''

OBS_PHASE2 = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.path.join(os.environ["STUB_DIR"], "calls.log"), "a") as f:
    f.write("obs_phase2 " + " ".join(sys.argv[1:]) + "\n")
if sys.argv[1:2] == ["rig-busy-check"]:
    print(json.dumps({"busy": False, "reasons": [], "diagnostics": []}))
'''


def _write_exec(path, body):
    path.write_text(body)
    path.chmod(0o755)


def _execute(tmp_path, boxes, extra=None):
    bin_dir = tmp_path / "bin"
    obs_dir = tmp_path / "obs"
    home = tmp_path / "home"
    for d in (bin_dir, obs_dir, home):
        d.mkdir()
    for name, body in (("gh", GH), ("sshpass", SSHPASS), ("ssh", SSH), ("rsync", RSYNC), ("curl", CURL)):
        _write_exec(bin_dir / name, body)
    _write_exec(obs_dir / "obs_phase2.py", OBS_PHASE2)
    env = _env({
        "PATH": "%s:%s" % (bin_dir, os.environ["PATH"]),
        "STUB_DIR": str(tmp_path), "STUB_SHA": SHA, "STUB_LIBSHA": LIBSHA,
        "STRIH_LX_SETUP_POLLS": "2", "STRIH_LX_SETUP_POLL_SECS": "0",
        "STRIH_LX_VERIFY_POLLS": "3", "STRIH_LX_VERIFY_POLL_SECS": "0", "STRIH_LX_VERIFY_SETTLE_SECS": "0",
        "STRIH_LX_OBS_PHASE2_DIR": str(obs_dir),
    }, home=str(home))
    env.update(extra or {})
    r = subprocess.run(["bash", str(FLEET), "--run-id", "R1", "--boxes", boxes], capture_output=True,
                       text=True, cwd=REPO, env=env, timeout=300)
    calls = (tmp_path / "calls.log").read_text() if (tmp_path / "calls.log").exists() else ""
    log = home / ".camera-box" / "genlock-fleet-deploy.log"
    return r, calls, (log.read_text() if log.exists() else "")


def _ssh_hosts(calls):
    return [m.group(1) for m in re.finditer(r"^ssh .* newlevel@(\S+) ", calls, re.M)]


def test_execute_deploys_both_pp_boxes_through_their_fleet_names(tmp_path):
    r, calls, log = _execute(tmp_path, "strih-pp,stream-pp")
    assert r.returncode == 0, r.stdout + r.stderr + calls
    hosts = set(_ssh_hosts(calls))
    assert hosts == {"strih-pp.lan", "stream-pp.lan"}, hosts
    assert "10.77.8." not in calls and "10.77.9." not in calls
    # each box launched its own setup and ran the acceptance with its OWN box name
    assert (tmp_path / "setup.strih-pp.lan.stdin").exists() and (tmp_path / "setup.stream-pp.lan.stdin").exists()
    assert re.search(r"newlevel@strih-pp\.lan .*verify-strih\.sh' --box strih-pp$", calls, re.M)
    assert re.search(r"newlevel@stream-pp\.lan .*verify-strih\.sh' --box stream-pp$", calls, re.M)
    # the rig-busy guard reads the PP venue (strih-pp + stream-pp) for both boxes
    guards = [ln for ln in calls.splitlines() if ln.startswith("obs_phase2 rig-busy-check")]
    assert len(guards) == 4, guards
    assert all("--strih-host strih-pp.lan --stream-host stream-pp.lan" in g for g in guards), guards
    # one fleet-log line per verified box, the strih-pp one first
    boxes_logged = [ln.split("\t")[3] for ln in log.splitlines()]
    assert boxes_logged == ["strih-pp", "stream-pp"], log
    assert "[strih-pp" not in r.stderr and "[stream-pp" not in r.stderr, r.stderr


def test_an_unreachable_traveling_box_reads_away_and_never_fails_the_run(tmp_path):
    r, calls, log = _execute(tmp_path, "strih-pp,stream-pp", {"STUB_SSH_DOWN": "strih-pp.lan"})
    assert r.returncode == 0, r.stdout + r.stderr + calls
    both = r.stdout + r.stderr
    assert re.search(r"AWAY.*strih-pp", both), both
    # strih-pp was only probed (the identity read), never staged/stopped
    assert [h for h in _ssh_hosts(calls) if h == "strih-pp.lan"] == ["strih-pp.lan"]
    assert "strih-pp.lan:/tmp/genlock-stage" not in calls
    assert not (tmp_path / "setup.strih-pp.lan.stdin").exists()
    # the other box went on and is logged; the AWAY box is not
    assert (tmp_path / "setup.stream-pp.lan.stdin").exists()
    assert [ln.split("\t")[3] for ln in log.splitlines()] == ["stream-pp"], log


def test_an_unreachable_always_box_still_fails_loud(tmp_path):
    r, calls, log = _execute(tmp_path, "strih-snv", {"STUB_SSH_DOWN": "10.77.9.202"})
    assert r.returncode == 4, r.stdout + r.stderr
    assert "ERROR: [strih-snv preflight]" in r.stderr
    assert "AWAY" not in r.stdout + r.stderr
    assert log == ""


def test_a_failed_acceptance_stops_only_that_box_and_names_it(tmp_path):
    r, calls, log = _execute(tmp_path, "strih-pp,stream-pp", {"STUB_ACCEPT_FAIL": "strih-pp.lan"})
    assert r.returncode == 5, r.stdout + r.stderr
    assert "ERROR: [strih-pp accept]" in r.stderr
    assert (tmp_path / "setup.stream-pp.lan.stdin").exists(), "the other box still deploys"
    assert [ln.split("\t")[3] for ln in log.splitlines()] == ["strih-pp:accept-failed", "stream-pp"], log


# --- review round 1 ---------------------------------------------------------------------------------

def test_a_traveling_box_bound_by_the_ssh_timeout_reads_away_too(tmp_path):
    """rc 124 (the per-command bound) is a box that does not answer, like rc 255."""
    r, calls, log = _execute(tmp_path, "strih-pp,stream-pp", {"STUB_SSH_HUNG": "stream-pp.lan"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert re.search(r"AWAY.*stream-pp", r.stdout + r.stderr)
    assert not (tmp_path / "setup.stream-pp.lan.stdin").exists()
    assert [ln.split("\t")[3] for ln in log.splitlines()] == ["strih-pp"], log


def test_a_traveling_box_that_answers_but_refuses_the_login_is_a_failure_not_away(tmp_path):
    """A wrong password (sshpass rc 5) means the box is there: a named exit 4, never AWAY."""
    r, calls, log = _execute(tmp_path, "strih-pp", {"STUB_SSH_DENIED": "strih-pp.lan"})
    assert r.returncode == 4, r.stdout + r.stderr
    assert "ERROR: [strih-pp preflight]" in r.stderr
    assert "AWAY" not in r.stdout + r.stderr
    assert log == ""


PP_IP_FLEET = ("strih-snv|10.77.9.202|linux-genlock|always\n"
               "stream-snv|10.77.9.204|windows-genlock|always\n"
               "imag|10.77.9.182|linux-genlock|retired\n"
               "resolume-snv|resolume.lan|windows-genlock|traveling\n"
               "strih-pp|10.77.8.202|linux-genlock|traveling\n"
               "stream-pp|stream-pp.lan|linux-genlock|traveling\n"
               "resolume-pp|resolume-pp.lan|windows-genlock|traveling")


def test_a_traveling_row_that_holds_an_address_is_refused_before_any_box_is_touched(tmp_path):
    """The venue STRIH_IP (10.77.8.x) is never dialled: a traveling row must carry the fleet NAME."""
    r, calls, log = _execute(tmp_path, "strih-pp,stream-pp", {"OBS_FLEET": PP_IP_FLEET})
    assert r.returncode == 3, r.stdout + r.stderr
    assert "ERROR: [strih-pp resolve]" in r.stderr and "NAME" in r.stderr
    assert "ssh " not in calls and "rsync " not in calls and "gh run download" not in calls


def test_the_venue_guard_hosts_come_from_the_fact_files_and_the_venue_table():
    r = _sourced('strih_lx_guard_hosts strih-snv 10.77.9.202; strih_lx_guard_hosts strih-pp strih-pp.lan; '
                 'strih_lx_guard_hosts stream-pp stream-pp.lan; strih_lx_venue_strih_box pp')
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["10.77.9.202 10.77.9.204", "strih-pp.lan stream-pp.lan",
                                     "strih-pp.lan stream-pp.lan", "strih-pp"]


def test_the_plan_and_execute_route_a_box_by_the_same_predicate():
    s = FLEET.read_text()
    plan = s[s.index("main() {"):s.index("# --- execute mode")]
    assert 'if fleet_box_runs_setup_strih "$b"; then emit_strih_lx_plan' in plan
    r = _sourced("fleet_deploy_order")
    order = r.stdout.split()
    assert order == ["strih-snv", "strih-pp", "stream-pp", "stream-snv", "imag", "resolume-snv"]
    assert len(order) == len(set(order))


def test_an_unprepared_box_reaches_the_named_error_under_set_u(tmp_path):
    r = _sourced('STRIH_LX_PREP_WORK=%s; STRIH_LX_PREP_STAGE=/tmp/genlock-stage-abc123; rc=0; strih_lx_apply abc123 strih-pp 2>&1 || rc=$?; echo "rc=$rc"' % tmp_path)
    assert "rc=4" in r.stdout, r.stdout + r.stderr
    assert "[strih-pp preflight]" in r.stdout and "not resolved" in r.stdout
