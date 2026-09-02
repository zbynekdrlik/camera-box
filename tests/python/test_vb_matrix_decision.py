"""#1227 — PURE decision core for the dev1 VB-Matrix alert watchdog (scripts/vb_matrix_decision.py).

The #1199 strih-nic-selfheal / #1203 ndi-halving / #1226 audio-lag python-mirror precedent: the
decision RED->GREENs LOCALLY under Tier-0 (#557 kills cargo), with no ssh/OBS/rig. The watchdog curls
`:8899/bundle-state.json` from strih+stream (the `vb_matrix_running` facet #1227 adds to the gather)
and pages when a box that HAS a VB-Matrix install is not running its `VBAudioMatrix*` process — the
exact 2026-08-30→09-02 3-day outage (StartVBMatrix had no AtLogon trigger; the virtual VASIO-8 ASIO
driver had no host, so both stream OBS ASIO inputs starved for days with nothing alarming).

Verdicts (classify):
  SKIP     -- box could not be fetched (:8899 down / box down) -> #732/#1001 territory, never our page.
  UNKNOWN  -- box fetched OK but the vb_matrix_running facet is ABSENT (a box with no VB-Matrix
              install, e.g. imag -> the gather omits it; or an old bundle-state-server not serving
              the facet yet). No reading -> no page (never a false negative on a non-VB-Matrix box).
  RUNNING  -- vb_matrix_running == "1": the VBAudioMatrix* process is alive. Healthy.
  DOWN     -- vb_matrix_running == "0": the box HAS the install but the process is not running.
              The watchdog pages after a 2-pass confirm.
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import vb_matrix_decision as vmd  # noqa: E402

_DECIDE = _SCRIPTS / "vb_matrix_decision.py"


# ------------------------------------------------------------------ extract_vb_matrix
def test_extract_reads_all_fields():
    body = json.dumps({
        "vb_matrix_running": "1", "vb_matrix_name": "VBAudioMatrix_x64",
        "vb_matrix_pid": "8144", "vb_matrix_start": "2026-09-02T14:01:40",
        "obs_version": "32.1.2",
    })
    assert vmd.extract_vb_matrix(body) == ("1", "VBAudioMatrix_x64", "8144", "2026-09-02T14:01:40")


def test_extract_down_running_zero_no_pid():
    body = json.dumps({"vb_matrix_running": "0"})
    assert vmd.extract_vb_matrix(body) == ("0", None, None, None)


def test_extract_missing_facet_is_none():
    # imag / no-VB-Matrix box: the gather omits the facet entirely.
    assert vmd.extract_vb_matrix(json.dumps({"obs_version": "32.1.2"})) == (None, None, None, None)


def test_extract_empty_value_is_none():
    assert vmd.extract_vb_matrix(json.dumps({"vb_matrix_running": ""})) == (None, None, None, None)


def test_extract_bad_json_is_none():
    assert vmd.extract_vb_matrix("not json at all") == (None, None, None, None)
    assert vmd.extract_vb_matrix("") == (None, None, None, None)
    assert vmd.extract_vb_matrix(None) == (None, None, None, None)


def test_extract_json_array_top_level_is_none():
    assert vmd.extract_vb_matrix("[1, 2, 3]") == (None, None, None, None)


# ------------------------------------------------------------------ classify
def test_classify_unreachable_is_skip():
    assert vmd.classify("0", box_reachable=0) == "SKIP"
    assert vmd.classify(None, box_reachable=0) == "SKIP"


def test_classify_running_one_is_running():
    assert vmd.classify("1", box_reachable=1) == "RUNNING"


def test_classify_running_zero_is_down():
    assert vmd.classify("0", box_reachable=1) == "DOWN"


def test_classify_absent_facet_is_unknown():
    assert vmd.classify(None, box_reachable=1) == "UNKNOWN"


def test_classify_unexpected_value_is_unknown():
    # A junk value must never read as RUNNING or DOWN -- fail-safe to UNKNOWN (no page).
    assert vmd.classify("2", box_reachable=1) == "UNKNOWN"
    assert vmd.classify("yes", box_reachable=1) == "UNKNOWN"


# ------------------------------------------------------------------ analyze
def test_analyze_down_carries_fields():
    body = json.dumps({"vb_matrix_running": "0"})
    res = vmd.analyze(body, box_reachable=1)
    assert res["verdict"] == "DOWN"
    assert res["running"] == "0"


def test_analyze_running_carries_context():
    body = json.dumps({
        "vb_matrix_running": "1", "vb_matrix_name": "VBAudioMatrix_x64",
        "vb_matrix_pid": "8144", "vb_matrix_start": "2026-09-02T14:01:40",
    })
    res = vmd.analyze(body, box_reachable=1)
    assert res["verdict"] == "RUNNING"
    assert res["name"] == "VBAudioMatrix_x64"
    assert res["pid"] == "8144"
    assert res["start"] == "2026-09-02T14:01:40"


def test_analyze_unreachable_skips_without_parsing():
    # box_reachable=0 must SKIP without touching the (empty) body.
    res = vmd.analyze("", box_reachable=0)
    assert res == {"verdict": "SKIP", "running": None, "name": None, "pid": None, "start": None}


def test_analyze_absent_facet_is_unknown():
    res = vmd.analyze(json.dumps({"obs_version": "32.1.2"}), box_reachable=1)
    assert res["verdict"] == "UNKNOWN"


# ------------------------------------------------------------------ CLI (the shell contract)
def _run_cli(args, stdin_text):
    return subprocess.run(
        [sys.executable, str(_DECIDE), *args],
        input=stdin_text, capture_output=True, text=True,
    )


def _cli_kv(out):
    return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


def test_cli_down():
    body = json.dumps({"vb_matrix_running": "0"})
    r = _run_cli(["analyze", "--box-reachable", "1"], body)
    assert r.returncode == 0, r.stderr
    kv = _cli_kv(r.stdout)
    assert kv["verdict"] == "DOWN"
    assert kv["running"] == "0"


def test_cli_running():
    body = json.dumps({
        "vb_matrix_running": "1", "vb_matrix_name": "VBAudioMatrix_x64",
        "vb_matrix_pid": "8144", "vb_matrix_start": "2026-09-02T14:01:40",
    })
    r = _run_cli(["analyze", "--box-reachable", "1"], body)
    assert r.returncode == 0, r.stderr
    kv = _cli_kv(r.stdout)
    assert kv["verdict"] == "RUNNING"
    assert kv["name"] == "VBAudioMatrix_x64"
    assert kv["pid"] == "8144"
    assert kv["start"] == "2026-09-02T14:01:40"


def test_cli_skip_needs_no_stdin():
    r = _run_cli(["analyze", "--box-reachable", "0"], "")
    assert r.returncode == 0, r.stderr
    kv = _cli_kv(r.stdout)
    assert kv["verdict"] == "SKIP"


def test_cli_unknown_facet_absent():
    r = _run_cli(["analyze", "--box-reachable", "1"], json.dumps({"obs_version": "32.1.2"}))
    assert r.returncode == 0, r.stderr
    kv = _cli_kv(r.stdout)
    assert kv["verdict"] == "UNKNOWN"
