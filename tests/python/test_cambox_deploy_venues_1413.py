"""Issue 1413 (the remaining part) -- the camera-box BINARY deploy and version read for the camboxes
of every OTHER venue (Poprad: cam1/4/6/7 since 11.10.2026).

Owner (verbatim, 12.10.2026): "chcem aby si sa staral ... aj o dantesync upgrady v pp sieti a tiez o
obs". The first slice put the PP camboxes into the dantesync fleet as `reach` rows at their dev1
address (10.76.9.<camera_resolve octet>, the venue fact STRIH_CAMERA_DEV1_NET). This part reuses
that ONE address resolution (`_dantesync_fleet_camera_rows`, never a second copy) for:
  * `deploy-fleet.sh --venue <name>` -- the venue's cameras at their dev1 address, the unchanged
    per-box swap (the issue-1407 sidecar + verified ro close); a camera that does not answer ssh :22
    is SKIPPED by name (exit 0), a reachable one that fails is FAILED (exit 1);
  * the ci.yml deploy-fleet job -- one more step that loops over every away venue (no venue literal:
    a third venue is a fact-file edit) and gates the job;
  * `camera-box-version-gate.sh --other-venues` -- report-only rows for every away-venue camera
    (never the gate's exit), wired into the E2E [0/8] call.

Tier-0: no cargo, no box, no network. Reachability is decided by the OBS_FLEET_HOME force list,
the fake box is tests/python/ro_window_fakes_1407.py, and every dialled host is logged.
"""
import os
import pathlib
import re
import stat
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import log, make_box, root, starts_on_rw  # noqa: E402
from test_ro_window_sites_1407 import (  # noqa: E402
    _OLD_CAMERA_BOX, _SSHPASS, _camera_box_artifact, _log_file_tools,
)

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_DEPLOY = _ROOT / "scripts" / "deploy-fleet.sh"
_GATE = _ROOT / "scripts" / "camera-box-version-gate.sh"
_CI = _ROOT / ".github" / "workflows" / "ci.yml"
_E2E = _ROOT / "scripts" / "recording-e2e.sh"
_BOXES = _ROOT / "scripts" / "strih-boxes"

_CLEAN_PREFIXES = ("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "CAMERA_", "FRAME_PROBE_")

PP_CAMS = ["cam1", "cam4", "cam6", "cam7"]


def _env(**over):
    env = {k: v for k, v in os.environ.items() if not k.startswith(_CLEAN_PREFIXES)}
    env.update(over)
    return env


def _bash(tmp_path, body, **env):
    """Source the fleet lib under the callers' strict mode and run BODY."""
    script = tmp_path / "run.sh"
    script.write_text(f"set -euo pipefail\n. '{_LIB}'\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def _boxes_copy(tmp_path, edits):
    """A copy of the real strih-boxes dir with per-file (old, new) line edits."""
    d = tmp_path / "strih-boxes"
    d.mkdir()
    for f in _BOXES.glob("*.env"):
        body = f.read_text()
        for old, new in edits.get(f.name, []):
            assert body.count(old + "\n") == 1, (f.name, old)
            body = body.replace(old + "\n", new + "\n")
        (d / f.name).write_text(body)
    return d


# ---------------------------------------------------------------------------------------------
# the shared venue-camera rows (a filter over the first slice's camera rows)
# ---------------------------------------------------------------------------------------------

def test_venue_camera_rows_name_each_camera_at_its_dev1_address(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_venue_camera_rows pp; echo ---; dantesync_fleet_venue_camera_rows snv")
    assert r.returncode == 0, r.stderr
    pp, snv = r.stdout.split("---\n")
    assert pp.splitlines() == [f"{c}|10.76.9.6{c[3:]}|reach" for c in PP_CAMS]
    assert snv.splitlines() == [f"{c}|10.77.9.6{c[3:]}|always" for c in ("cam2", "cam3", "cam5")]


def test_away_venues_are_every_other_venue_that_lists_cameras(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_away_venues")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["pp"]
    # a third venue is one fact file: it joins the away list with no code edit
    boxes = _boxes_copy(tmp_path, {})
    third = (boxes / "strih-pp.env").read_text()
    third = third.replace("STRIH_VENUE=pp\n", "STRIH_VENUE=ke\n").replace("STRIH_CAMERAS=1 4 6 7\n", "STRIH_CAMERAS=9\n")
    (boxes / "strih-ke.env").write_text(third)
    r = _bash(tmp_path, "dantesync_fleet_away_venues", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(boxes))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["ke", "pp"]


def test_venue_camera_rows_of_an_unknown_venue_are_empty(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_venue_camera_rows nowhere; echo rc=$?")
    assert r.stdout.strip() == "rc=0", r.stdout + r.stderr


def test_venue_camera_rows_fail_loudly_on_a_broken_fact_file(tmp_path):
    boxes = _boxes_copy(tmp_path, {"strih-lx.env": [("STRIH_CAMERAS=2 3 5", "STRIH_CAMERAS=2 3 5 7")]})
    r = _bash(tmp_path, "rc=0; dantesync_fleet_venue_camera_rows pp || rc=$?; echo rc=$rc",
              DANTESYNC_FLEET_STRIH_BOXES_DIR=str(boxes))
    assert "rc=1" in r.stdout, r.stdout + r.stderr
    assert "cam7" in r.stderr and "two venue fact files" in r.stderr, r.stderr
    assert "cam7|" not in r.stdout, "a camera listed twice must never be deployed to"


# ---------------------------------------------------------------------------------------------
# deploy-fleet.sh --venue
# ---------------------------------------------------------------------------------------------

# A dev1-side sshpass that LOGS the host it dials (state/hosts), then runs the issue-1407 fake: scp
# copies into the one fake box, ssh RUNS the remote text on it.
_HOST_LOG = r'''
import os, sys
_a = sys.argv[3:]
with open(os.path.join(os.environ["FAKE_STATE"], "hosts"), "a") as _f:
    _f.write((_a[-1].split(":", 1)[0] if _a[0] == "scp" else _a[-2]) + "\n")
if os.environ.get("FAKE_SSH_DEAD"):  # the box answered the reach probe, then refuses ssh
    sys.exit(255)
'''


def _sub(tmp_path, name):
    d = tmp_path / name
    d.mkdir()
    return d


def _venue_deploy(tmp_path, args, home, live=_OLD_CAMERA_BOX, extra_env=None, **fake):
    box = make_box(tmp_path, root="ro")
    _log_file_tools(box)
    st = box["state"]
    (st / "fs" / "usr" / "local" / "bin").mkdir(parents=True)
    if live is not None:
        (st / "fs" / "usr" / "local" / "bin" / "camera-box").write_bytes(live)
    dev1 = tmp_path / "dev1-bin"
    dev1.mkdir()
    (dev1 / "sshpass").write_text(f"#!{sys.executable}\n{_HOST_LOG}{_SSHPASS}")
    (dev1 / "gh").write_text("#!/bin/sh\necho GH-CALLED-UNEXPECTEDLY >&2\nexit 1\n")
    for f in dev1.iterdir():
        f.chmod(0o755)
    env = _env(PATH=f"{dev1}:/usr/bin:/bin", FAKE_STATE=str(st), FAKE_BOX_PATH=str(box["stub"]),
               SSH_PASS="x", GENLOCK_WAIT_TRIES="1", GENLOCK_WAIT_SECS="0", HOME=str(tmp_path),
               OBS_FLEET_HOME=home)
    env.update(extra_env or {})
    env.update({k: v for k, v in fake.items() if v is not None})
    proc = subprocess.run(["bash", str(_DEPLOY), *args], env=env, capture_output=True, text=True, timeout=180)
    hosts = (st / "hosts").read_text().split() if (st / "hosts").exists() else []
    return proc, box, hosts


def test_deploy_venue_skips_the_away_cameras_and_deploys_the_reachable_one_at_its_pp_address(tmp_path):
    artifact = _camera_box_artifact(tmp_path)
    proc, box, hosts = _venue_deploy(tmp_path, ["--binary", str(artifact), "--venue", "pp"], home="cam7")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out + "\n".join(log(box))
    for cam in ("cam1", "cam4", "cam6"):
        assert re.search(rf"\[{cam}\] .*SKIPPED", out), out
    assert "cam1(" not in out and "FLEET NOT FULLY ALIGNED" not in out, out
    # every call went to cam7's PP address -- never an SNV address, never an away camera
    assert hosts and set(hosts) == {"root@10.76.9.67"}, hosts
    assert (box["state"] / "fs" / "usr" / "local" / "bin" / "camera-box").read_bytes() == artifact.read_bytes()
    # the per-box swap is the unchanged issue-1407 one: nothing started on a writable root
    assert starts_on_rw(log(box)) == [], "\n".join(log(box))
    assert root(box) == "ro"
    assert "VENUE pp" in proc.stdout and "cam7" in proc.stdout, proc.stdout
    assert "skipped (unreachable): cam1 cam4 cam6" in proc.stdout, proc.stdout


def test_deploy_venue_with_every_camera_reachable_deploys_them_all(tmp_path):
    proc, box, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "pp"],
                                     home="cam1 cam4 cam6 cam7")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert sorted(set(hosts)) == [f"root@10.76.9.6{n}" for n in (1, 4, 6, 7)], hosts
    assert "VENUE pp ALIGNED: [cam1 cam4 cam6 cam7]" in proc.stdout, proc.stdout
    assert "skipped (unreachable)" not in proc.stdout, proc.stdout
    assert starts_on_rw(log(box)) == [], "\n".join(log(box))


def test_deploy_venue_fails_a_reachable_camera_whose_swap_fails(tmp_path):
    proc, box, _ = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "pp"],
                                 home="cam7", FAKE_SCP_PARTIAL="1")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    assert "cam7(scp-failed)" in out, out
    assert (box["state"] / "fs" / "usr" / "local" / "bin" / "camera-box").read_bytes() == _OLD_CAMERA_BOX


def test_deploy_venue_with_every_camera_away_dials_nothing_and_passes(tmp_path):
    proc, box, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "pp"],
                                     home="strih-lx")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 0, out
    assert hosts == [], hosts
    assert "skipped (unreachable): cam1 cam4 cam6 cam7" in proc.stdout, proc.stdout
    assert "no camera of venue pp was reachable" in proc.stdout, proc.stdout


def test_deploy_venue_refuses_camera_set_and_the_painter(tmp_path):
    art = str(_camera_box_artifact(tmp_path))
    proc, _, hosts = _venue_deploy(_sub(tmp_path, "a"), ["--binary", art, "--venue", "pp"], home="cam7",
                                   extra_env={"CAMERA_SET": "cam7"})
    assert proc.returncode == 2 and hosts == [], proc.stdout + proc.stderr
    assert "CAMERA_SET" in proc.stderr
    probe = tmp_path / "frame-probe"
    probe.write_text("x")
    proc, _, hosts = _venue_deploy(_sub(tmp_path, "b"), ["--binary", art, "--venue", "pp", "--frame-probe",
                                                         str(probe)], home="cam7")
    assert proc.returncode == 2 and hosts == [], proc.stdout + proc.stderr
    assert "frame-probe" in proc.stderr


def test_deploy_venue_refuses_an_unknown_venue_and_a_broken_fact_file(tmp_path):
    art = str(_camera_box_artifact(tmp_path))
    proc, _, hosts = _venue_deploy(_sub(tmp_path, "a"), ["--binary", art, "--venue", "nowhere"], home="cam7")
    assert proc.returncode == 1 and hosts == [], proc.stdout + proc.stderr
    assert "venue 'nowhere'" in proc.stderr
    boxes = _boxes_copy(tmp_path, {"strih-lx.env": [("STRIH_CAMERAS=2 3 5", "STRIH_CAMERAS=2 3 5 7")]})
    proc, _, hosts = _venue_deploy(_sub(tmp_path, "b"), ["--binary", art, "--venue", "pp"], home="cam7",
                                   extra_env={"DANTESYNC_FLEET_STRIH_BOXES_DIR": str(boxes)})
    assert proc.returncode == 1 and hosts == [], proc.stdout + proc.stderr
    assert "two venue fact files" in proc.stderr, proc.stderr


def test_deploy_default_path_still_dials_camera_resolve(tmp_path):
    proc, box, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path))], home="cam7",
                                     extra_env={"CAMERA_SET": "cam2"})
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert set(hosts) == {"root@10.77.9.62"}, hosts
    assert "FLEET ALIGNED: all of [cam2]" in proc.stdout, proc.stdout


def test_deploy_venue_skips_an_acked_offline_camera_without_dialling_it(tmp_path):
    # a staged PP box (historical-build bisect, a repair) is acked offline exactly like an SNV box:
    # the main-merge venue deploy must not overwrite it
    proc, _, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "pp"],
                                   home="cam7", extra_env={"CAMBOX_OFFLINE_ACK": "cam7:staged for a bisect"})
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert hosts == [], hosts
    assert "skipped (acked offline): cam7" in proc.stdout, proc.stdout
    assert "staged for a bisect" in proc.stdout + proc.stderr


def test_deploy_venue_refuses_the_home_venue(tmp_path):
    proc, _, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "snv"],
                                   home="cam7")
    assert proc.returncode == 2 and hosts == [], proc.stdout + proc.stderr
    assert "home rig" in proc.stderr, proc.stderr


def test_deploy_venue_fails_a_camera_that_answers_the_probe_but_refuses_ssh(tmp_path):
    proc, _, hosts = _venue_deploy(tmp_path, ["--binary", str(_camera_box_artifact(tmp_path)), "--venue", "pp"],
                                   home="cam7", FAKE_SSH_DEAD="1")
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    assert "cam7(unreachable)" in out, out
    assert set(hosts) == {"root@10.76.9.67"}, hosts


def test_away_venues_drop_a_malformed_venue_name_loudly(tmp_path):
    boxes = _boxes_copy(tmp_path, {"strih-pp.env": [("STRIH_VENUE=pp", "STRIH_VENUE=P*P")]})
    r = _bash(tmp_path, "dantesync_fleet_away_venues", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(boxes))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == [], r.stdout
    assert "P*P" in r.stderr and "not a venue name" in r.stderr, r.stderr


def test_away_venues_use_the_row_builders_grammar_a_leading_digit_is_dropped(tmp_path):
    # the row builder refuses a venue that starts with a digit; the away list must agree, or CI
    # would loop over a venue whose cameras have no row
    boxes = _boxes_copy(tmp_path, {"strih-pp.env": [("STRIH_VENUE=pp", "STRIH_VENUE=9pp")]})
    r = _bash(tmp_path, "dantesync_fleet_away_venues", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(boxes))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == [], r.stdout
    assert "9pp" in r.stderr and "not a venue name" in r.stderr, r.stderr


# ---------------------------------------------------------------------------------------------
# the ci.yml deploy-fleet job
# ---------------------------------------------------------------------------------------------

def _deploy_job():
    text = _CI.read_text()
    start = text.index("\n  deploy-fleet:\n")
    end = text.index("\n  notify-on-failure:\n", start)
    return text[start:end]


def test_ci_deploy_job_also_deploys_every_away_venue_after_the_snv_step():
    job = _deploy_job()
    snv = job.index("name: Deploy to the active cam fleet")
    venue = job.index("name: Deploy to every other venue's camboxes")
    assert snv < venue, job
    step = job[venue:]
    assert "dantesync_fleet_away_venues" in step, step
    assert re.search(r'scripts/deploy-fleet\.sh --binary dist/camera-box --venue "\$v"', step), step
    assert "if: ${{ !cancelled() }}" in step, "a failed SNV deploy must not stop the other venues' deploy"
    assert "continue-on-error" not in job, "CI steps are binary (no-continue-on-error.md)"
    assert not re.search(r"--venue\s+pp\b", step), "no venue literal: a third venue is a fact-file edit"
    assert "exit \"$rc\"" in step, "a reachable camera that fails must red the job"


def _ci_step_run(tmp_path, fake_rc):
    """Run the away-venue step's own `run:` text in a scratch tree: the real fleet lib + fact files,
    a fake scripts/deploy-fleet.sh that logs its argv and exits FAKE_RC."""
    import yaml
    job = yaml.safe_load(_CI.read_text())["jobs"]["deploy-fleet"]["steps"]
    run = next(st["run"] for st in job if "other venue" in (st.get("name") or ""))
    tree = tmp_path / "tree"
    (tree / "scripts").mkdir(parents=True)
    for name in ("lib", "strih-boxes", "camera-set.sh"):
        (tree / "scripts" / name).symlink_to(_ROOT / "scripts" / name)
    fake = tree / "scripts" / "deploy-fleet.sh"
    fake.write_text(f'#!/usr/bin/env bash\necho "$*" >> "{tmp_path}/argv.log"\nexit {fake_rc}\n')
    fake.chmod(0o755)
    (tree / "dist").mkdir()
    (tree / "dist" / "camera-box").write_text("#!/bin/sh\necho camera-box 9.9.9-test\n")
    script = tmp_path / "step.sh"
    script.write_text(run)
    r = subprocess.run(["bash", "-e", str(script)], cwd=tree, capture_output=True, text=True, env=_env())
    argv = (tmp_path / "argv.log").read_text().splitlines() if (tmp_path / "argv.log").exists() else []
    return r, argv


def test_ci_step_deploys_each_away_venue_and_passes_when_they_pass(tmp_path):
    r, argv = _ci_step_run(tmp_path, 0)
    assert r.returncode == 0, r.stdout + r.stderr
    assert argv == ["--binary dist/camera-box --venue pp"], argv


def test_ci_step_reds_the_job_when_a_venue_deploy_fails_and_names_its_exit(tmp_path):
    r, argv = _ci_step_run(tmp_path, 1)
    assert r.returncode == 1, r.stdout + r.stderr
    assert argv == ["--binary dist/camera-box --venue pp"], argv
    err = next(ln for ln in r.stdout.splitlines() if ln.startswith("::error::"))
    assert "venue pp" in err and "exited 1" in err, err


# ---------------------------------------------------------------------------------------------
# camera-box-version-gate.sh --other-venues (report-only)
# ---------------------------------------------------------------------------------------------

def _gate(tmp_path, args, versions, home, extra_env=None):
    """VERSIONS: host -> camera-box version the stub sshpass answers (any other host: no answer)."""
    tmp_path.mkdir(exist_ok=True)
    b = tmp_path / "gbin"
    b.mkdir()
    arms = "".join(f'    *@{host}) echo "camera-box {v}" ;;\n' for host, v in versions.items())
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\nshift 2\ntarget=''\n"
        'for a in "$@"; do case "$a" in *@*) target="$a"; break ;; esac; done\n'
        f'printf "%s\\n" "$target" >> "{tmp_path}/dials.log"\n'
        'case "$target" in\n' + arms + "    *) exit 255 ;;\nesac\n")
    (b / "sshpass").chmod((b / "sshpass").stat().st_mode | stat.S_IEXEC)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    extra = dict(extra_env or {})
    first = extra.pop("TIMEOUT_DIR", None)
    env = _env(PATH=(f"{first}:" if first else "") + f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=home)
    env.update(extra)
    r = subprocess.run(["bash", str(_GATE), "--fleet-file", str(fleet), *args], capture_output=True, text=True,
                       env=env, timeout=120)
    dials = (tmp_path / "dials.log").read_text().split() if (tmp_path / "dials.log").exists() else []
    return r, dials


_SNV_ARGS = ["--main-pin", "1.7.0-dev.764", "--linux", "cam2=root@192.0.2.62"]


def test_gate_other_venues_names_a_stale_pp_camera_without_changing_the_exit(tmp_path):
    r, dials = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"],
                     {"192.0.2.62": "1.7.0-dev.764", "10.76.9.67": "1.7.0-dev.700"}, home="cam7")
    assert r.returncode == 0, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "1.7.0-dev.700" in row and "PIN-DRIFT" in row and "report-only" in row and "venue pp" in row, row
    for cam in ("cam1", "cam4", "cam6"):
        row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith(cam))
        assert "SKIPPED (unreachable" in row, row
    assert "deploy-fleet.sh --venue pp" in r.stderr, r.stderr
    assert "root@10.76.9.67" in dials
    assert not any(d.endswith(("@10.76.9.61", "@10.76.9.64", "@10.76.9.66", "@10.77.9.67")) for d in dials), dials


def test_gate_other_venues_on_main_reads_ok_and_never_rescues_a_failing_snv_fleet(tmp_path):
    r, _ = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"],
                 {"192.0.2.62": "1.7.0-dev.700", "10.76.9.67": "1.7.0-dev.764"}, home="cam7")
    assert r.returncode == 20, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "OK" in row and "report-only" in row, row


def test_gate_other_venues_unread_reachable_camera_is_unknown_report_only(tmp_path):
    r, _ = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"], {"192.0.2.62": "1.7.0-dev.764"}, home="cam7")
    assert r.returncode == 0, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "UNKNOWN" in row and "report-only" in row, row


def test_gate_without_other_venues_reads_no_other_venue(tmp_path):
    r, dials = _gate(tmp_path, _SNV_ARGS, {"192.0.2.62": "1.7.0-dev.764", "10.76.9.67": "1.7.0-dev.700"},
                     home="cam7")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "venue pp" not in r.stdout and not any("10.76.9." in d for d in dials), r.stdout


def test_gate_other_venues_bounds_a_camera_whose_read_hangs(tmp_path):
    import time
    t0 = time.monotonic()
    r, _ = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"], {"192.0.2.62": "1.7.0-dev.764",
                                                            # the box hangs BEFORE it answers
                                                            "10.76.9.67": "\"; sleep 60; echo \"camera-box 1.7.0-dev.764"},
                 home="cam7", extra_env={"CAMERA_BOX_VERSION_GATE_VENUE_READ_TIMEOUT": "2"})
    assert time.monotonic() - t0 < 30, "a hung PP read must never stall the home E2E"
    assert r.returncode == 0, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "UNKNOWN" in row and "report-only" in row, row


def test_gate_other_venues_read_bound_cannot_be_switched_off(tmp_path):
    # GNU `timeout 0` means NO bound: a 0 / non-numeric override falls back to the 20 s default,
    # so the read still ends (this fixture answers at once; the pinned point is the argv)
    b = tmp_path / "tbin"
    b.mkdir()
    log = tmp_path / "timeout.log"
    (b / "timeout").write_text(f'#!/usr/bin/env bash\necho "$1" >> "{log}"\nshift\nexec "$@"\n')
    (b / "timeout").chmod(0o755)
    for i, bad in enumerate(("0", "abc", "-5")):
        r, _ = _gate(tmp_path / f"g{i}", [*_SNV_ARGS, "--other-venues"],
                     {"192.0.2.62": "1.7.0-dev.764", "10.76.9.67": "1.7.0-dev.764"}, home="cam7",
                     extra_env={"CAMERA_BOX_VERSION_GATE_VENUE_READ_TIMEOUT": bad, "TIMEOUT_DIR": str(b)})
        assert r.returncode == 0, r.stdout + r.stderr
    assert log.read_text().split() == ["20", "20", "20"], log.read_text()


def test_gate_other_venues_excludes_an_acked_camera_without_reading_it(tmp_path):
    r, dials = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"],
                     {"192.0.2.62": "1.7.0-dev.764", "10.76.9.67": "1.7.0-dev.700"}, home="cam7",
                     extra_env={"CAMBOX_OFFLINE_ACK": "cam7:staged"})
    assert r.returncode == 0, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "EXCLUDED" in row and "staged" in row, row
    assert "root@10.76.9.67" not in dials, dials


def test_gate_other_venues_under_no_main_pin_names_the_version_read(tmp_path):
    r, _ = _gate(tmp_path, ["--no-main-pin", "--linux", "cam2=root@192.0.2.62", "--other-venues"],
                 {"192.0.2.62": "1.7.0-dev.764", "10.76.9.67": "1.7.0-dev.700"}, home="cam7")
    assert r.returncode == 0, r.stdout + r.stderr
    row = next(ln for ln in r.stdout.splitlines() if ln.strip().startswith("cam7"))
    assert "READ" in row and "1.7.0-dev.700" in row and "report-only" in row, row


def test_gate_other_venues_with_a_broken_fact_file_keeps_the_exit(tmp_path):
    boxes = _boxes_copy(tmp_path, {"strih-lx.env": [("STRIH_CAMERAS=2 3 5", "STRIH_CAMERAS=2 3 5 7")]})
    r, _ = _gate(tmp_path, [*_SNV_ARGS, "--other-venues"], {"192.0.2.62": "1.7.0-dev.764"}, home="cam7",
                 extra_env={"DANTESYNC_FLEET_STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stdout + r.stderr
    assert "camera list is incomplete" in r.stderr, r.stderr


def test_gate_refuses_other_venues_with_frame_probe_only(tmp_path):
    r, dials = _gate(tmp_path, ["--linux", "cam2=root@192.0.2.62", "--frame-probe-only", "--other-venues"],
                     {}, home="cam7")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "--other-venues" in r.stderr, r.stderr
    assert dials == [], dials


def test_e2e_version_gate_call_reports_the_other_venues():
    s = _E2E.read_text()
    call = s.index('"$HERE/camera-box-version-gate.sh" \\\n  --linux "$CAMBOX_VERSION_LINUX"')
    block = s[call:s.index("\n\n", call)]
    assert "--other-venues" in block, block
