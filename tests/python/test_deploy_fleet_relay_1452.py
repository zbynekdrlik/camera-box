"""Issue 1452 (process fix) -- the fleet upgrade also upgrades the bkshading relay.

Owner, 29.10.2026 (verbatim): "nechapem ako tam moze byt nejaka stara verzia ved vcera pred eventom
som ziadal aby sa spravili upgrady!". `scripts/deploy-fleet.sh` updated camera-box (and the cam2
painter) but never the relay: every cambox ran an old relay (dev.755-810) and cam7's shading did not
set values until the relay was redeployed by hand.

Now one fleet upgrade brings camera-box AND the relay to the SAME CI run on every cambox it touches:
  * the relay comes from the same ci.yml run (`bkshading-linux-amd64`), or `--relay-binary` next to
    `--binary`; it is swapped by `scripts/bkshading-deploy-relay.sh` itself (rig-busy gate, relay
    stop/restore, byte-verify, the verified ro close), never a second copy of the swap;
  * a box with no relay is reported "not installed", never a failure;
  * a relay failure is named in FAILED (record, continue, fold at the end: the issue-868 pattern);
  * the final summary line names each box's camera-box and relay version;
  * `--no-relay` exists for the [0/8] parity align (an E2E that holds the lease and pauses the relay).
And the readiness check: rig-dev-handover-check's shading item grades the relay version against the
box's camera-box version (RELAY-STALE = forgot).

Every deploy runs WHOLE on the issue-1407 fake read-only-root box (tests/python/ro_window_fakes_1407.py)
behind a fake `sshpass`. Tier-0: no rig, no network, no cargo.
"""
import json
import os
import pathlib
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import log, make_box, root, starts_on_rw  # noqa: E402
from test_ro_window_sites_1407 import (  # noqa: E402
    _DEPLOY, _OLD_CAMERA_BOX, _camera_box_artifact, _dev1_bin, _log_file_tools,
)

ROOT = pathlib.Path(__file__).resolve().parents[2]
LIB = ROOT / "scripts" / "lib" / "bkshading-relay-version.sh"
DECISION = ROOT / "scripts" / "rig_dev_handover_decision.py"
HANDOVER = ROOT / "scripts" / "rig-dev-handover-check.sh"
CI = ROOT / ".github" / "workflows" / "ci.yml"
ALIGN = ROOT / "scripts" / "lib" / "camera-box-parity-align.sh"
DEPLOY_RELAY = ROOT / "scripts" / "bkshading-deploy-relay.sh"

_OLD_RELAY = b"#!/bin/bash\necho 'bkshading-relay 1.7.0-dev.810'\n"


def _relay_artifact(tmp_path, version="9.9.9-test"):
    a = tmp_path / "relay-artifact"
    a.write_text(f"#!/bin/bash\necho 'bkshading-relay {version}'\n")
    a.chmod(0o755)
    return a


def _obs_idle(tmp_path):
    d = tmp_path / "obs-phase2"
    d.mkdir()
    (d / "obs_phase2.py").write_text(
        "import json, sys\n"
        "if len(sys.argv) > 1 and sys.argv[1] == 'rig-busy-check':\n"
        "    print(json.dumps({'busy': False, 'diagnostics': []}))\n")
    return d


def _relay_env(tmp_path, **extra):
    env = {"BKSHADING_DEPLOY_OBS_PHASE2_DIR": str(_obs_idle(tmp_path)),
           "RIG_LEASE_DIR": str(tmp_path / "no-rig-lease")}
    env.update(extra)
    return env


def _live(box, name):
    return box["state"] / "fs" / "usr" / "local" / "bin" / name


def _deploy_with_relay(tmp_path, args, relay_live=_OLD_RELAY, relay_active=False, gh_dir=None, **extra):
    """Run the REAL deploy-fleet.sh over CAMERA_SET=cam2 against the issue-1407 fake box (the
    _deploy harness of test_ro_window_sites_1407.py), with an old camera-box and, unless relay_live
    is None, an old relay already on the box. GH_DIR goes first on PATH (a fake gh)."""
    box = make_box(tmp_path, root="ro")
    _log_file_tools(box)
    st = box["state"]
    fs = st / "fs" / "usr" / "local" / "bin"
    fs.mkdir(parents=True)
    (fs / "camera-box").write_bytes(_OLD_CAMERA_BOX)
    (fs / "camera-box").chmod(0o755)
    if relay_live is not None:
        (fs / "bkshading-relay").write_bytes(relay_live)
        (fs / "bkshading-relay").chmod(0o755)
    if relay_active:
        (st / "active-bkshading-relay.service").write_text("active\n")
    dev1 = _dev1_bin(tmp_path, box)
    path = f"{dev1}:/usr/bin:/bin"
    if gh_dir is not None:
        path = f"{gh_dir}:{path}"
    env = {"PATH": path, "FAKE_STATE": str(st), "FAKE_BOX_PATH": str(box["stub"]),
           "CAMERA_SET": "cam2", "SSH_PASS": "x", "GENLOCK_WAIT_TRIES": "1", "GENLOCK_WAIT_SECS": "0",
           "HOME": str(tmp_path)}
    env.update(_relay_env(tmp_path))
    env.update({k: v for k, v in extra.items() if v is not None})
    proc = subprocess.run(["bash", str(_DEPLOY), *args], env=env, capture_output=True, text=True, timeout=180)
    return proc, box


def _summary(proc):
    lines = [ln for ln in proc.stdout.splitlines() + proc.stderr.splitlines()
             if "ALIGNED" in ln]
    assert lines, proc.stdout + proc.stderr
    return lines[-1]


# ---- deploy-fleet.sh: the relay comes along ----------------------------------------------------- #


def test_fleet_upgrade_swaps_an_old_relay_to_the_same_build_and_names_both_versions(tmp_path):
    cb = _camera_box_artifact(tmp_path)
    rl = _relay_artifact(tmp_path)
    proc, box = _deploy_with_relay(tmp_path, ["--binary", str(cb), "--relay-binary", str(rl)])
    out = proc.stdout + proc.stderr
    calls = log(box)
    assert proc.returncode == 0, out + "\n".join(calls)
    assert _live(box, "bkshading-relay").read_bytes() == rl.read_bytes(), out
    assert _live(box, "camera-box").read_bytes() == cb.read_bytes(), out
    # the relay swap is bkshading-deploy-relay.sh's own (staged copy + atomic mv + verified close)
    assert "[bkshading-deploy-relay]" in out, out
    assert any("mv -f /usr/local/bin/bkshading-relay.deploy." in c for c in calls), "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)
    assert root(box) == "ro"
    line = _summary(proc)
    assert "FLEET ALIGNED" in line, line
    assert "cam2 camera-box=9.9.9-test relay=9.9.9-test" in line, line


def test_an_active_relay_is_stopped_for_the_swap_and_started_again_on_a_read_only_root(tmp_path):
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--relay-binary", str(_relay_artifact(tmp_path))],
        relay_active=True)
    calls = log(box)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert any(c.startswith("systemctl stop bkshading-relay.service") for c in calls), "\n".join(calls)
    assert any(c.startswith("systemctl start bkshading-relay.service root=ro") for c in calls), "\n".join(calls)
    assert starts_on_rw(calls) == [], "\n".join(calls)


def test_a_box_without_a_relay_is_reported_not_installed_never_a_failure(tmp_path):
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--relay-binary", str(_relay_artifact(tmp_path))],
        relay_live=None)
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert not _live(box, "bkshading-relay").exists(), "a relay is never installed where none was"
    assert "[bkshading-deploy-relay]" not in out, out
    line = _summary(proc)
    assert "FLEET ALIGNED" in line and "cam2 camera-box=9.9.9-test relay=not installed" in line, line


def test_binary_without_a_relay_binary_fails_a_box_whose_relay_is_installed_by_name(tmp_path):
    proc, box = _deploy_with_relay(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))])
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    assert "cam2-relay(no relay binary" in out, out
    assert "FLEET NOT FULLY ALIGNED" in out, out
    # camera-box itself still went through, and the old relay is untouched
    assert _live(box, "camera-box").read_bytes() == _camera_box_artifact(tmp_path).read_bytes()
    assert _live(box, "bkshading-relay").read_bytes() == _OLD_RELAY
    assert "relay=1.7.0-dev.810" in _summary(proc)


def test_no_relay_leaves_the_relay_alone_and_still_names_its_version(tmp_path):
    proc, box = _deploy_with_relay(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--no-relay"])
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert _live(box, "bkshading-relay").read_bytes() == _OLD_RELAY
    assert "[bkshading-deploy-relay]" not in out, out
    line = _summary(proc)
    assert "FLEET ALIGNED" in line and "relay=1.7.0-dev.810 (not deployed: --no-relay)" in line, line


def test_a_relay_binary_of_another_version_is_refused_before_any_box_is_touched(tmp_path):
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)),
                   "--relay-binary", str(_relay_artifact(tmp_path, version="1.0.0-other"))])
    out = proc.stdout + proc.stderr
    assert proc.returncode == 2, out
    assert "not from the same build" in out, out
    assert log(box) == [], "nothing may touch a box: " + "\n".join(log(box))


def _write_lease(lease_dir, repo, run_id):
    lease_dir.mkdir()
    (lease_dir / "holder.json").write_text(json.dumps({
        "repo": repo, "run_id": run_id, "run_url": "", "job": "e2e",
        "acquired_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "expected_release_at": ""}))
    (lease_dir / "heartbeat").write_text("")


def test_a_relay_refused_by_a_foreign_rig_lease_is_named_and_camera_box_still_aligns(tmp_path):
    lease = tmp_path / "lease"
    _write_lease(lease, "zbynekdrlik/restreamer", "4242")
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--relay-binary", str(_relay_artifact(tmp_path))],
        RIG_LEASE_DIR=str(lease), RIG_LEASE_REPO="", RIG_LEASE_RUN_ID="", GITHUB_REPOSITORY="", GITHUB_RUN_ID="")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    assert "cam2-relay(rig lease held" in out, out
    assert _live(box, "bkshading-relay").read_bytes() == _OLD_RELAY
    assert _live(box, "camera-box").read_bytes() == _camera_box_artifact(tmp_path).read_bytes()


def test_the_callers_own_rig_lease_lets_the_relay_deploy_through(tmp_path):
    # the post-merge ci.yml deploy job takes the lease itself (rig-busy-gate.sh) before deploy-fleet
    lease = tmp_path / "lease"
    _write_lease(lease, "zbynekdrlik/camera-box", "777")
    rl = _relay_artifact(tmp_path)
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--relay-binary", str(rl)],
        RIG_LEASE_DIR=str(lease), GITHUB_REPOSITORY="zbynekdrlik/camera-box", GITHUB_RUN_ID="777")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert _live(box, "bkshading-relay").read_bytes() == rl.read_bytes()
    assert "held by this run itself" in out, out


def test_a_box_whose_camera_box_close_left_the_root_writable_gets_no_relay_deploy(tmp_path):
    proc, box = _deploy_with_relay(
        tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--relay-binary", str(_relay_artifact(tmp_path))],
        FAKE_RO_FAIL="1")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    assert "[bkshading-deploy-relay]" not in out, out
    assert _live(box, "bkshading-relay").read_bytes() == _OLD_RELAY
    assert "relay not deployed: the root is not verified read-only" in out, out


def _fake_gh(tmp_path, cb, rl):
    d = tmp_path / "gh-bin"
    d.mkdir()
    logf = tmp_path / "gh.log"
    (d / "gh").write_text(
        "#!/bin/bash\n"
        f"echo \"$*\" >> '{logf}'\n"
        "case \"$1 $2\" in\n"
        "  'run view') echo 0123456789abcdef0123456789abcdef01234567 ;;\n"
        "  'run download')\n"
        "    dir=''; name=''\n"
        "    while [ $# -gt 0 ]; do case \"$1\" in --dir) dir=$2; shift 2;; -n) name=$2; shift 2;; *) shift;; esac; done\n"
        "    mkdir -p \"$dir\"\n"
        f"    if [ \"$name\" = camera-box-linux-amd64 ]; then cp '{cb}' \"$dir/camera-box\"; "
        f"elif [ \"$name\" = bkshading-linux-amd64 ]; then cp '{rl}' \"$dir/bkshading-relay\"; else exit 1; fi ;;\n"
        "  *) exit 1 ;;\n"
        "esac\n")
    (d / "gh").chmod(0o755)
    return d, logf


def test_a_run_deploy_fetches_the_relay_from_the_same_ci_run(tmp_path):
    cb = _camera_box_artifact(tmp_path)
    rl = _relay_artifact(tmp_path)
    ghdir, logf = _fake_gh(tmp_path, cb, rl)
    proc, box = _deploy_with_relay(tmp_path, ["--run", "5555"], gh_dir=ghdir)
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    downloads = [ln for ln in logf.read_text().splitlines() if ln.startswith("run download")]
    assert any("5555" in d and "bkshading-linux-amd64" in d for d in downloads), downloads
    assert any("5555" in d and "camera-box-linux-amd64" in d for d in downloads), downloads
    assert _live(box, "bkshading-relay").read_bytes() == rl.read_bytes()


# ---- the pure lib -------------------------------------------------------------------------------- #


def _lib(body, *args):
    return subprocess.run(["bash", "-c", f"set -euo pipefail\n. '{LIB}'\n{body}", "harness", *args],
                          capture_output=True, text=True, timeout=30)


def test_version_verdict_grades_the_relay_against_the_boxs_camera_box():
    cases = {
        ("1.8.1", "1.8.1"): "RELAY-CURRENT",
        ("1.8.1", "1.7.0-dev.810"): "RELAY-STALE",
        ("1.8.1", "absent"): "RELAY-NONE",
        ("1.8.1", "missing-binary"): "RELAY-MISSING",
        ("1.8.1", "unreadable"): "RELAY-UNKNOWN",
        ("unreadable", "1.8.1"): "RELAY-UNKNOWN",
        ("absent", "1.8.1"): "RELAY-UNKNOWN",
        ("", ""): "RELAY-UNKNOWN",
    }
    for (cbv, rlv), want in cases.items():
        r = _lib('bkshading_relay_version_verdict "$1" "$2"', cbv, rlv)
        assert r.returncode == 0 and r.stdout.strip() == want, (cbv, rlv, r.stdout, r.stderr)


def test_versions_field_reads_the_last_line_of_its_key():
    text = "noise\ncamera-box=1.8.1\nrelay=1.7.0-dev.810\n"
    r = _lib('bkshading_relay_versions_field "$1" relay; bkshading_relay_versions_field "$1" camera-box', text)
    assert r.stdout.split() == ["1.7.0-dev.810", "1.8.1"], r.stdout + r.stderr
    r = _lib('bkshading_relay_versions_field "" relay')
    assert r.returncode == 0 and r.stdout.strip() == "", r.stdout


# ---- the readiness row (rig-dev-handover-check shading item) ------------------------------------- #


def _decision():
    import importlib.util
    spec = importlib.util.spec_from_file_location("rdh_decision_1452", DECISION)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def test_handover_shading_item_forgets_a_stale_relay_even_while_shading_is_off():
    d = _decision()
    item = next(i for i in d.ITEMS if i.key == "shading")
    stale = ("cam2 (10.77.9.62): enabled=disabled active=inactive online=? -> verdict=SHADING-OFF\n"
             "cam2 (10.77.9.62): camera-box=1.8.1 relay=1.7.0-dev.810 -> verdict=RELAY-STALE\n")
    e = item.decide({"shading": (stale, 0)})
    assert e["status"] == d.FORGOT, e
    assert "deploy-fleet.sh" in e["message"] and "1452" in e["message"], e
    # a current relay changes nothing: TEST mode (all off) stays UNKNOWN, a live relay stays OK
    cur = ("cam2 (10.77.9.62): enabled=disabled active=inactive online=? -> verdict=SHADING-OFF\n"
           "cam2 (10.77.9.62): camera-box=1.8.1 relay=1.8.1 -> verdict=RELAY-CURRENT\n")
    assert item.decide({"shading": (cur, 0)})["status"] == d.UNKNOWN
    on = ("cam1 (10.77.9.61): enabled=enabled active=active online=true -> verdict=SHADING-ON\n"
          "cam1 (10.77.9.61): camera-box=1.8.1 relay=1.8.1 -> verdict=RELAY-CURRENT\n")
    assert item.decide({"shading": (on, 0)})["status"] == d.OK
    # the crashed-relay message is still its own when only SHADING-DEAD is there
    dead = "cam1 (10.77.9.61): enabled=enabled active=inactive online=? -> verdict=SHADING-DEAD\n"
    assert "SHADING-DEAD" in item.decide({"shading": (dead, 0)})["message"]
    missing = "cam1 (10.77.9.61): camera-box=1.8.1 relay=missing-binary -> verdict=RELAY-MISSING\n"
    assert item.decide({"shading": (missing, 0)})["status"] == d.FORGOT


def test_handover_probe_reads_both_versions_through_the_shared_lib():
    s = HANDOVER.read_text()
    assert s.count("bkshading_relay_versions_cmd") == 1, "the probe reads the versions with the lib's remote text"
    assert "bkshading_relay_version_verdict" in s
    assert 'lib/bkshading-relay-version.sh"' in s


# ---- the callers ---------------------------------------------------------------------------------- #


def _job(text, name):
    start = text.index(f"\n  {name}:\n")
    nxt = text.find("\n  ", start + 1)
    while nxt != -1 and text[nxt + 3] == " ":
        nxt = text.find("\n  ", nxt + 1)
    return text[start:nxt if nxt != -1 else len(text)]


def test_the_post_merge_deploy_job_ships_the_relay_from_this_run():
    import yaml
    wf = yaml.safe_load(CI.read_text())
    job = wf["jobs"]["deploy-fleet"]
    assert "bkshading" in job["needs"], job["needs"]
    steps = job["steps"]
    dl = [s for s in steps if s.get("with", {}).get("name") == "bkshading-linux-amd64"]
    assert len(dl) == 1, "the relay artifact of THIS run is downloaded once"
    path = dl[0]["with"]["path"]
    runs = [s["run"] for s in steps if "scripts/deploy-fleet.sh" in s.get("run", "")]
    assert len(runs) == 2, runs
    for r in runs:
        assert f"--relay-binary {path}/bkshading-relay" in r, r
        assert "--no-relay" not in r, r


def test_the_e2e_parity_align_deploys_camera_box_only():
    s = ALIGN.read_text()
    assert '--binary "$bin" --no-relay' in s, "the [0/8] align holds the E2E lease and the relay is paused"


def test_deploy_relay_accepts_only_the_callers_own_lease():
    s = DEPLOY_RELAY.read_text()
    assert "rig_lease_held_by" in s, "the lease preflight lets the holder's own run through"
