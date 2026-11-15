"""Issue 1505, last line: the PP Windows OBS boxes resolume-pp and grading-pp in the genlock tooling.

`scripts/lib/obs-fleet.sh` carries a grading-pp row (windows-genlock, traveling, dialled at
10.76.8.62 -- no DNS name resolves from dev1 -- with NOTEBOOKASUS declared as its computer name) beside
the existing resolume-pp row, and `scripts/lib/genlock-fleet-boxes.sh` registers both boxes (MCP,
address, AHK mode, title profile, deploy order). So `deploy-genlock-fleet.sh --boxes resolume-pp` /
`--boxes grading-pp` plan the pinned Windows zip for them and `launch-obs-genlock.sh --box <box>`
prints their relaunch program. Both stay explicit-only and in no watchdog facet.
"""
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_obs_release_windows_1505 as winrel  # noqa: E402  (the fake pinned Windows release)

REPO = HERE.parent.parent
FLEET = REPO / "scripts" / "lib" / "obs-fleet.sh"
BOXES = REPO / "scripts" / "lib" / "genlock-fleet-boxes.sh"
LAUNCH = REPO / "scripts" / "launch-obs-genlock.sh"
_FLEET_ENV = ("OBS_FLEET", "OBS_FLEET_IDENTITY", "OBS_FLEET_HOME", "OBS_FLEET_ALIASES")


def _clean_env(**extra):
    e = {k: v for k, v in os.environ.items() if k not in _FLEET_ENV}
    e.update(extra)
    return e


def _boxes(body, *args):
    """Run BODY with the per-box lib sourced under the callers' strict mode; args are $1..$n."""
    return subprocess.run(["bash", "-c", f"set -euo pipefail\n. \"$0\"\n{body}", str(BOXES), *args],
                          capture_output=True, text=True, timeout=60, env=_clean_env())


def _launch(*args):
    return subprocess.run(["bash", str(LAUNCH), *args], capture_output=True, text=True, timeout=60,
                          env=_clean_env())


# --- the per-box table ------------------------------------------------------------------------------

@pytest.mark.parametrize("box,mcp,host,ahk,profile", [
    ("resolume-pp", "win-resolume-pp", "resolume-pp.lan", "guard", "cg"),
    ("grading-pp", "win-grading-pp", "10.76.8.62", "0", ""),
])
def test_each_pp_windows_box_has_its_mcp_address_ahk_mode_and_profile(box, mcp, host, ahk, profile):
    r = _boxes('fleet_box_mcp "$1"; fleet_box_ip "$1"; fleet_box_ahk_mode "$1"; '
               'p="$(fleet_box_obs_profile "$1")" || p=""; echo "profile=$p"; '
               'obs_fleet_class "$1"; echo; obs_fleet_home_check "$1"; echo', box)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:6] == [mcp, host, ahk, f"profile={profile}", "windows-genlock", "traveling"]


def test_grading_pp_declares_its_live_computer_name_and_has_no_keepalive_or_ahk_identity():
    r = _boxes('obs_fleet_expected_identity grading-pp; echo; echo "ka=[$(fleet_box_keepalive_tasks grading-pp)]"; '
               'echo "has_ahk=$(fleet_box_has_ahk grading-pp)"; '
               'fleet_box_ahk_script grading-pp || echo "script rc=$?"; '
               'echo "res_has_ahk=$(fleet_box_has_ahk resolume-pp)"')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:5] == ["NOTEBOOKASUS", "ka=[]", "has_ahk=0", "script rc=2", "res_has_ahk=0"], \
        "resolume-pp gets guard mode without claiming an installed watcher"


def test_an_unknown_box_still_names_no_mcp_and_is_refused_by_the_planner():
    r = _boxes('fleet_box_mcp grading || echo "mcp rc=$?"; fleet_box_ip grading || echo "ip rc=$?"; '
               'fleet_normalize_boxes "$1" || echo "norm rc=$?"', "grading-zz")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["mcp", "rc=2", "ip", "rc=2", "norm", "rc=2"]
    assert "unknown box 'grading-zz'" in r.stderr and "grading-pp" in r.stderr, \
        "the refusal lists the valid boxes, the new ones included"


def test_neither_pp_windows_box_is_in_the_default_fleet_but_both_are_explicitly_valid():
    r = _boxes('fleet_normalize_boxes ""; fleet_normalize_boxes grading-pp,resolume-pp,stream-snv; '
               'fleet_deploy_order | tail -n 3 | tr "\\n" " "')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:3] == ["strih-snv,stream-snv", "stream-snv,resolume-pp,grading-pp",
                                        "resolume-snv resolume-pp grading-pp "]


def test_both_pp_windows_boxes_route_to_the_pinned_zip():
    r = _boxes('for b in resolume-pp grading-pp; do echo "$b=$(fleet_boxes_swap_obs_dll "$b")"; done')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["resolume-pp=1", "grading-pp=1"]


# --- the fleet list ---------------------------------------------------------------------------------

FACETS = ("audio-lag av-step vb-matrix bundle-state network-reach obs-liveness genlock-lock render-freeze "
          "ndi-portmap obs-session burn-reconcile rig-restore ndi-sender win-baseline audio-mixer obs-handles "
          "build-parity").split()


def test_the_new_row_joins_no_watchdog_facet():
    """A grading-pp row must never page or be polled: it is in no facet roster."""
    for facet in FACETS:
        r = subprocess.run(["bash", "-c", '. "$0"; obs_fleet_boxes "$1"', str(FLEET), facet],
                           capture_output=True, text=True, timeout=30, env=_clean_env())
        assert r.returncode == 0, (facet, r.stderr)
        assert "grading-pp" not in r.stdout and "resolume-pp" not in r.stdout, (facet, r.stdout)


def test_the_python_fleet_twin_reads_the_grading_pp_row(monkeypatch):
    sys.path.insert(0, str(REPO / "scripts"))
    import obs_fleet_table as t
    for k in _FLEET_ENV:
        monkeypatch.delenv(k, raising=False)
    assert t.fleet_host("grading-pp") == "10.76.8.62"
    assert t.fleet_class("grading-pp") == "windows-genlock"
    assert t.fleet_home_check("grading-pp") == "traveling"
    assert t.fleet_expected_identity("grading-pp") == "NOTEBOOKASUS"
    assert t.fleet_name_for_host("10.76.8.62", resolve=lambda h: "") == "grading-pp"


# --- the deploy planner -----------------------------------------------------------------------------

@pytest.mark.parametrize("box,mcp,host,ident", [
    ("grading-pp", "win-grading-pp", "10.76.8.62", "NOTEBOOKASUS"),
    ("resolume-pp", "win-resolume-pp", "resolume-pp.lan", "resolume-pp"),
])
def test_the_deploy_plan_names_the_box_mcp_host_identity_and_the_pinned_zip(tmp_path, box, mcp, host, ident):
    env = winrel._release(tmp_path)
    r, tree = winrel._bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    r = winrel._deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", box)
    assert r.returncode == 0, r.stdout + r.stderr
    out = r.stdout
    assert f"FLEET PLAN: box={box} ({mcp}, {host}) mode=full" in out
    assert f"STEP -1 ({box} -- box IDENTITY confirm, issue 1505)" in out
    assert f"$env:COMPUTERNAME is {ident}" in out
    assert "resolume.lan" not in out, "never the SNV resolume's identity note"
    assert winrel.ZIP in out and f"C:\\stage-genlock-{winrel.COMMIT}" in out
    assert f"run the program below in the {mcp} MCP Shell" in out
    assert f"launch-obs-genlock.sh --box {box} --force" in out
    assert f"\tobs-{winrel.TAG}\t{winrel.COMMIT}\t{box}\tfull" in out, "the fleet log names the box"
    assert out.count("FLEET PLAN: box=") == 1


def test_the_resolume_snv_plan_keeps_its_own_identity_note(tmp_path):
    env = winrel._release(tmp_path)
    r, tree = winrel._bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    r = winrel._deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", "resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "# STEP -1 (resolume ONLY -- box IDENTITY confirm, issue 1295)" in r.stdout
    assert "issue 1505): resolume-snv" not in r.stdout


def test_grading_pp_ahk_steps_are_noops_and_resolume_pp_ahk_is_guarded(tmp_path):
    env = winrel._release(tmp_path)
    r, tree = winrel._bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    g = winrel._deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", "grading-pp").stdout
    assert "# (1) AutoHotkey64 watchdog: no-op" in g and "#1372 AHK REPORT" not in g
    rp = winrel._deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", "resolume-pp").stdout
    assert "# (1) AutoHotkey64 guard -- stopped only if running" in rp and "#1372 AHK REPORT" in rp


# --- the launch planner -----------------------------------------------------------------------------

@pytest.mark.parametrize("box,mcp,host", [("grading-pp", "win-grading-pp", "10.76.8.62"),
                                          ("resolume-pp", "win-resolume-pp", "resolume-pp.lan")])
def test_the_launch_plan_names_the_box_mcp_and_host(box, mcp, host):
    r = _launch("--box", box, "--force")
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith(f"# ===== #257 genlock OBS (re)launch plan — box={box} ({mcp}, {host}) =====")
    assert f"STEP 1: paste the following PowerShell program into:  {mcp} Shell" in r.stdout
    assert f"obs_burn_filter.py sweep-off --host {host}" in r.stdout


def test_grading_pp_launch_has_no_title_check_and_names_its_missing_pin_baseline():
    out = _launch("--box", "grading-pp").stdout
    assert "$wantProfile" not in out, "its window title reads the projector: no title check"
    assert "NOTE (issue 1505): role 'grading' has no latency-pins baseline" in out


def test_resolume_pp_launch_checks_the_cg_title_and_uses_the_resolume_pin_baseline():
    out = _launch("--box", "resolume-pp").stdout
    assert "$wantProfile = 'cg'" in out
    assert "latency_pins_verify.py --box resolume --host resolume-pp.lan" in out
    assert "NOTE (issue 1505)" not in out


@pytest.mark.parametrize("bad", ["grading", "resolume-zz", "notebookasus"])
def test_the_launch_planner_still_refuses_an_unknown_box(bad):
    r = _launch("--box", bad)
    assert r.returncode == 2 and r.stdout == ""
    assert f"(got '{bad}')" in r.stderr and "'grading-pp'" in r.stderr
