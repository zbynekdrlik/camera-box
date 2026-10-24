"""issue 1474 item 2 -- the 8 h A/V soak (scripts/av-soak.sh, and through it the restart matrix) on the
LINUX stream box stream-snv. Its stream leg used win_ssh_upload / win_ssh_run (PowerShell) and the
Windows decode recording-verdict-on-stream.sh; on stream-snv it must take the ONE Linux path of issue
1462 instead, by the box's obs-fleet class: the pushes through stream_lx_push_file (plain ssh mkdir +
scp), the decode through recording-verdict-on-stream-snv.sh (the strih-snv on-box decode), the decode
stop through stream_box_exec + a pkill scoped to this run's output names, and the cleanup plan lines
through the shared exact-path rm helper. A Windows stream box (an OBS_FLEET override, the whole
test_av_soak_orchestrator_1367.py suite) keeps the PowerShell text byte for byte.

The rig is the orchestrator suite's fully faked one (fake OBS, sshpass, curl, decodes, verdict;
a tmp lease). Tier-0: no rig, no cargo, no network.
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from test_av_soak_orchestrator_1367 import (  # noqa: E402,F401  (rig is a fixture)
    REPO, _assert_rig_restored, _csv_rows, _soak, rig)

MATRIX = os.path.join(REPO, "scripts", "av-restart-matrix.sh")


def _linux(env, **extra):
    """The default obs-fleet list: stream-snv (10.77.9.204) is linux-genlock."""
    e = {k: v for k, v in env.items() if k != "OBS_FLEET"}
    e.update(extra)
    return e


def _sshpass(p):
    return [ln for ln in p["log"].read_text().splitlines() if ln.startswith("sshpass")]


def test_a_linux_stream_window_pushes_decodes_and_merges_with_no_powershell(rig):
    env, p = rig
    r = _soak(_linux(env), "--run")
    assert r.returncode in (0, 1, 2), r.stdout + r.stderr
    rows = _csv_rows(p["run"])
    assert rows and rows[0]["outcome"] == "ok", rows
    calls = _sshpass(p)
    assert not [c for c in calls if "EncodedCommand" in c], "no PowerShell is sent to the Linux stream box"
    # the two pushes: plain ssh mkdir + scp of the marker log and the schedule into verdict-out/
    mk = [c for c in calls if "u@10.77.9.204" in c and "mkdir" in c]
    assert mk, calls
    ups = [c for c in calls if " scp " in c and "u@10.77.9.204:verdict-out/av-soak-" in c]
    assert any("markers.csv" in c for c in ups) and any("switch-schedule.json" in c for c in ups), ups
    # the decode: the Linux stream entry point's argument shape (issue 1462 run_stream_extract)
    dec = [c for c in p["log"].read_text().splitlines() if c.startswith("decode stream-decode.sh")]
    assert len(dec) == 1, dec
    d = dec[0]
    assert "--verdict-bin" in d and "--out-dir verdict-out" in d and "--stream-rec" in d, d
    assert "--extract-partial stream" in d and "--av-marker-log verdict-out/av-soak-" in d, d
    assert "--switch-schedule verdict-out/av-soak-" in d and "--out verdict-out/av-soak-" in d, d
    # (the fake OBS hands back a Windows-shaped recording path; the soak's OWN paths are Linux ones)
    assert "--verdict-exe-local" not in d and "camera-box\\\\verdict-out" not in d and "--execute" not in d, d
    _assert_rig_restored(p)


def test_a_linux_stream_box_needs_no_windows_verdict_exe(rig):
    env, p = rig
    e = _linux(env)
    e.pop("WIN_VERDICT_EXE_LOCAL")
    plan = _soak(e)
    assert "WIN_VERDICT_EXE_LOCAL" not in plan.stdout, plan.stdout
    r = _soak(e, "--run")
    assert "WIN_VERDICT_EXE_LOCAL" not in r.stdout + r.stderr, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "ok"


def test_a_windows_stream_box_still_needs_the_windows_verdict_exe(rig):
    env, p = rig
    e = dict(env)
    e.pop("WIN_VERDICT_EXE_LOCAL")
    r = _soak(e, "--run")
    assert r.returncode == 4 and "WIN_VERDICT_EXE_LOCAL" in r.stdout + r.stderr, r.stdout + r.stderr


def test_the_linux_cleanup_plan_is_exact_path_rm_over_ssh(rig):
    env, p = rig
    _soak(_linux(env), "--run")
    plan = (p["run"] / "cleanup-plan.txt").read_text()
    assert "Remove-Item" not in plan and "win-stream-snv" not in plan, plan
    assert "stream-snv ssh:" in plan and "ssh u@10.77.9.204" in plan, plan
    onbox = [ln for ln in plan.splitlines() if "rm -rf -- verdict-out/av-soak-" in ln]
    assert any("su@10.77.9.202" in ln or "<STRIH_USER>@10.77.9.202" in ln for ln in onbox), plan
    assert any("10.77.9.204" in ln for ln in onbox), plan


def test_a_linux_stream_decode_timeout_stops_this_runs_decode_over_plain_ssh(rig):
    env, p = rig
    r = _soak(_linux(env, AV_SOAK_MIN_DECODE_S="0", AV_SOAK_DECODE_TIMEOUT_S="2", FAKE_DECODE_SLEEP_STREAM="10"),
              "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert _csv_rows(p["run"])[0]["outcome"] == "no_verdict:decode_failed"
    kills = [c for c in _sshpass(p) if "pkill" in c and "u@10.77.9.204" in c]
    assert kills, _sshpass(p)
    k = kills[0].replace("\\", "")
    assert "recording-verdic[t] --extract-partial stream" in k and "av-soak-" in k, k
    assert not [c for c in _sshpass(p) if "EncodedCommand" in c]
    _assert_rig_restored(p)


def test_the_linux_plan_names_the_linux_stream_steps(rig):
    env, _p = rig
    out = _soak(_linux(env)).stdout
    assert "win_ssh_upload" not in out and "C:\\camera-box" not in out, out
    assert "push to the stream box: stream_lx_push_file markers -> verdict-out/av-soak-" in out, out
    assert "STREAM_LX_BOX=10.77.9.204" in out and "--out-dir verdict-out" in out, out
    # with no override the plan names the Linux entry point, never the Windows decode
    e = _linux(env)
    e.pop("AV_SOAK_STREAM_DECODE")
    out = _soak(e).stdout
    assert "recording-verdict-on-stream-snv.sh" in out and "recording-verdict-on-stream.sh " not in out, out


def test_the_windows_plan_text_is_unchanged(rig):
    env, _p = rig
    out = _soak(env).stdout
    assert "push to the stream box: win_ssh_upload markers -> C:\\camera-box\\verdict-out\\av-soak-" in out, out


def test_the_restart_matrix_needs_no_windows_exe_for_a_linux_stream_box():
    base = {k: v for k, v in os.environ.items()
            if k not in ("OBS_FLEET", "WIN_VERDICT_EXE_LOCAL", "STREAM_HOST", "STRIH_HOST")}
    lx = subprocess.run(["bash", MATRIX], env=base, capture_output=True, text=True, timeout=60)
    assert "WIN_VERDICT_EXE_LOCAL" not in lx.stdout, lx.stdout
    win = dict(base, OBS_FLEET="strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always")
    w = subprocess.run(["bash", MATRIX], env=win, capture_output=True, text=True, timeout=60)
    assert "WIN_VERDICT_EXE_LOCAL" in w.stdout, w.stdout
