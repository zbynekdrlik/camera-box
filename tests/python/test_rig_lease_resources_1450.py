"""issue 1450 slice A -- a rig lease holder DECLARES the boxes it touches.

On 17.10.2026 SongPlayer's win-resolume deploy waited on camera-box's av-soak, which touches only the
SNV cameras, strih-snv and stream-snv: the lease was one mutex with one holder, so every reader had to
treat ANY hold as "the whole rig". Since issue 1450 a holder names its resources (atoms or groups of
`scripts/rig-lease-resources.txt`) in `RIG_LEASE_RESOURCES`; `rig_lease_acquire` resolves them and
writes the resolved atom list into holder.json, so the :8890 view can tell a reader which boxes are
held (test_rig_lease_view_1450.py).

These tests drive the REAL bash lib (scripts/lib/rig-lease.sh) and the REAL gate with the
RIG_LEASE_DIR seam (never /var/tmp/rig-lease, a live run may hold it); every value goes to bash as an
ARGUMENT, never inside the script text.
"""
from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import sys

import pytest

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[1]
SCRIPTS = ROOT / "scripts"
LIB = SCRIPTS / "lib" / "rig-lease.sh"
GATE = SCRIPTS / "rig-busy-gate.sh"
TABLE = SCRIPTS / "rig-lease-resources.txt"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

ATOMS = ["snv-cams", "strih-snv", "stream-snv", "imag", "resolume-snv", "pp-cams", "strih-pp", "stream-pp",
         "resolume-pp"]
SNV_VIDEO = ["snv-cams", "strih-snv", "stream-snv", "imag"]
SIX = {"repo", "run_id", "run_url", "job", "acquired_at", "expected_release_at"}


def _rlr():
    import rig_lease_resources
    return rig_lease_resources


def _bash(lease_dir, script, *args, env=None):
    e = {k: v for k, v in os.environ.items() if not k.startswith("RIG_LEASE_")}
    e["RIG_LEASE_DIR"] = str(lease_dir)
    e.update(env or {})
    return subprocess.run(["bash", "-c", 'set -uo pipefail; . "$0"; ' + script, str(LIB), *args],
                          capture_output=True, text=True, env=e, timeout=60)


def _acquire(lease_dir, resources=None, run_id="555", repo="zbynekdrlik/camera-box"):
    env = {} if resources is None else {"RIG_LEASE_RESOURCES": resources}
    return _bash(lease_dir, 'rig_lease_acquire "$@"; echo "rc=$?"', repo, run_id, "https://x", "full-path-e2e",
                 "2099-01-01T00:00:00Z", env=env)


def _holder(lease_dir):
    return json.loads((lease_dir / "holder.json").read_text())


# --- the table ------------------------------------------------------------------------------------

def test_the_table_declares_the_atoms_in_canonical_order_and_the_groups():
    t = _rlr().load_table()
    assert list(t.atoms) == ATOMS
    assert {g: list(m) for g, m in t.groups.items()} == {
        "snv-video": SNV_VIDEO,
        "snv": SNV_VIDEO + ["resolume-snv"],
        "pp-video": ["pp-cams", "strih-pp", "stream-pp"],
        "pp": ["pp-cams", "strih-pp", "stream-pp", "resolume-pp"],
        "all": ATOMS,
    }


def test_resolve_takes_atoms_and_groups_and_returns_canonical_unique_atoms():
    rlr = _rlr()
    t = rlr.load_table()
    assert rlr.resolve(["resolume-snv", "snv-video", "imag"], t) == SNV_VIDEO + ["resolume-snv"]
    assert rlr.resolve(["all"], t) == ATOMS
    assert rlr.split_names(" snv-video , ,resolume-snv ") == ["snv-video", "resolume-snv"]
    with pytest.raises(rlr.UnknownResources) as e:
        rlr.resolve(["snv-video", "snv-vidoe", "Imag"], t)
    assert e.value.names == ["snv-vidoe", "Imag"]


@pytest.mark.parametrize("bad, why", [
    ("atom|snv-cams|a\natom|snv-cams|b\n", "twice"),
    ("atom|all|a\n", "reserved"),
    ("atom|a|x\ngroup|g|a nope\n", "nope"),
    ("atom|a|x\ngroup|g|later\ngroup|later|a\n", "later"),
    ("atom|Bad_Name|x\n", "Bad_Name"),
    ("thing|a|x\n", "line 1"),
    ("# only a comment\n", "no atom"),
])
def test_a_broken_table_fails_loud_by_name(tmp_path, bad, why):
    p = tmp_path / "t.txt"
    p.write_text(bad)
    rlr = _rlr()
    with pytest.raises(rlr.TableError) as e:
        rlr.load_table(str(p))
    assert why in str(e.value)


def test_the_cli_resolves_and_names_unknown_names():
    cli = [sys.executable, str(SCRIPTS / "rig_lease_resources.py"), "resolve"]
    ok = subprocess.run(cli + ["snv-video,resolume-snv"], capture_output=True, text=True, timeout=30)
    assert (ok.returncode, ok.stdout.strip()) == (0, ",".join(SNV_VIDEO + ["resolume-snv"]))
    bad = subprocess.run(cli + ["snv-video,snv-vidoe"], capture_output=True, text=True, timeout=30)
    assert (bad.returncode, bad.stdout.strip()) == (2, "snv-vidoe")
    blank = subprocess.run(cli + [" , "], capture_output=True, text=True, timeout=30)
    assert (blank.returncode, blank.stdout.strip()) == (0, "")


# --- rig_lease_acquire ------------------------------------------------------------------------------

def test_a_declared_acquire_writes_the_resolved_atoms_into_holder_json(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _acquire(lease, "snv-video")
    assert r.stdout.splitlines() == ["RIG_LEASE_ACQUIRED", "rc=0"], r.stdout + r.stderr
    h = _holder(lease)
    assert h["resources"] == SNV_VIDEO
    assert SIX <= set(h)


def test_an_explicit_all_declares_every_atom(tmp_path):
    lease = tmp_path / "rig-lease"
    _acquire(lease, "all")
    assert _holder(lease)["resources"] == ATOMS


def test_an_undeclared_acquire_keeps_todays_six_keys(tmp_path):
    for n, value in enumerate((None, "", " , ")):
        lease = tmp_path / ("rig-lease-%d" % n)
        r = _acquire(lease, value)
        assert "RIG_LEASE_ACQUIRED" in r.stdout, r.stderr
        assert set(_holder(lease)) == SIX, value


def test_an_unknown_name_refuses_with_rc_2_and_writes_nothing(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _acquire(lease, "snv-video,snv-vidoe")
    assert r.stdout.splitlines() == ["RIG_LEASE_BAD_RESOURCES=snv-vidoe", "rc=2"], r.stdout + r.stderr
    assert "scripts/rig-lease-resources.txt" in r.stderr
    assert not lease.exists()
    assert not list(tmp_path.iterdir())


def test_an_unknown_name_never_reclaims_a_stale_holder(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    old = {"repo": "zbynekdrlik/restreamer", "run_id": "1", "run_url": "u", "job": "j",
           "acquired_at": "2026-01-01T00:00:00Z", "expected_release_at": "2026-01-01T00:00:00Z"}
    (lease / "holder.json").write_text(json.dumps(old))
    (lease / "heartbeat").touch()
    os.utime(lease / "heartbeat", (1, 1))
    r = _acquire(lease, "resolume-snvv")
    assert "rc=2" in r.stdout, r.stdout
    assert _holder(lease) == old


def test_a_reclaim_writes_the_declared_atoms(tmp_path):
    lease = tmp_path / "rig-lease"
    _acquire(lease, None, run_id="1")
    os.utime(lease / "heartbeat", (1, 1))
    r = _acquire(lease, "snv-cams", run_id="2")
    assert "RIG_LEASE_RECLAIMED" in r.stdout, r.stdout + r.stderr
    assert _holder(lease)["run_id"] == "2" and _holder(lease)["resources"] == ["snv-cams"]


def test_a_refused_scoped_acquire_keeps_the_one_held_by_line(tmp_path):
    lease = tmp_path / "rig-lease"
    _acquire(lease, "snv-video", run_id="111", repo="zbynekdrlik/restreamer")
    r = _acquire(lease, "resolume-snv", run_id="222")
    assert r.stdout.splitlines() == [
        "RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#111 run_url=https://x job=full-path-e2e "
        "expected_release_at=2099-01-01T00:00:00Z", "rc=1"], r.stdout
    assert _holder(lease)["run_id"] == "111"


def test_the_refresh_keeps_the_declared_atoms(tmp_path):
    lease = tmp_path / "rig-lease"
    _bash(lease, 'rig_lease_acquire "$@"', "camera-box-av-soak", "s1", "", "av-soak", "2026-01-01T00:00:00Z",
          env={"RIG_LEASE_RESOURCES": "snv-video"})
    assert _holder(lease)["expected_release_at"] == "2026-01-01T00:00:00Z"
    r = _bash(lease, 'rig_lease_refresh_if_mine "$@"', "camera-box-av-soak", "s1")
    assert r.returncode == 0, r.stdout + r.stderr
    h = _holder(lease)
    assert h["expected_release_at"] != "2026-01-01T00:00:00Z", "the beat rewrote holder.json"
    assert h["resources"] == SNV_VIDEO


# --- rig-busy-gate.sh validates the declaration ONCE, before anything else ---------------------------

_FAKE_OBS = r'''import json, os, sys
open(os.environ["FAKE_LOG"], "a").write(" ".join(sys.argv[1:]) + "\n")
if sys.argv[1:2] == ["rig-busy-check"]:
    print(json.dumps({"busy": False, "reasons": [], "diagnostics": []}))
'''


def _gate(tmp_path, resources):
    fake = tmp_path / "obs_phase2.py"
    fake.write_text(_FAKE_OBS)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("RIG_LEASE_", "RIG_BUSY_GATE_", "GITHUB_"))}
    env.update({"OBS_PHASE2_PY": str(fake), "FAKE_LOG": str(tmp_path / "obs.log"),
                "RIG_LEASE_DIR": str(tmp_path / "lease"), "RIG_LEASE_REPO": "zbynekdrlik/camera-box",
                "RIG_LEASE_RUN_ID": "run-1450", "RIG_LEASE_JOB": "deploy-fleet",
                "RIG_BUSY_GATE_ITERATIONS": "1", "RIG_BUSY_GATE_SLEEP_SECS": "0",
                "RIG_BUSY_GATE_IDLE_HOLD_SECS": "0", "RIG_LEASE_RESOURCES": resources})
    return subprocess.run(["bash", str(GATE)], env=env, capture_output=True, text=True, timeout=120)


def test_the_gate_refuses_an_unknown_declaration_before_any_lease_or_obs_read(tmp_path):
    r = _gate(tmp_path, "snv-camz")
    out = r.stdout + r.stderr
    assert r.returncode == 45, out
    assert "OUTCOME=RIG_LEASE_BAD_RESOURCES RIG_LEASE_BAD_RESOURCES=snv-camz" in r.stdout, out
    assert not (tmp_path / "lease").exists()
    assert not (tmp_path / "obs.log").exists(), "an OBS read ran before the refusal"
    assert out.count("RIG_LEASE_BAD_RESOURCES=") == 1, "a typo must never loop"


def test_the_gate_takes_the_declared_resources(tmp_path):
    r = _gate(tmp_path, "snv-cams")
    out = r.stdout + r.stderr
    assert r.returncode == 0 and "OUTCOME=RIG_FREE" in out, out
    assert "[rig-busy-gate] rig lease resources: snv-cams -> snv-cams" in r.stdout, out
    assert _holder(tmp_path / "lease")["resources"] == ["snv-cams"]


# --- every camera-box holder declares what it touches ------------------------------------------------

def _yml_step(path, name_start):
    text = path.read_text()
    i = text.index("- name: " + name_start)
    j = text.find("\n      - name: ", i + 1)
    return text[i:j if j != -1 else len(text)]


def test_the_full_path_e2e_gate_declares_snv_video_and_resolume_only_with_cg_chain():
    wf = ROOT / ".github" / "workflows" / "full-path-e2e.yml"
    step = _yml_step(wf, "Rig-busy gate")
    assert ("RIG_LEASE_RESOURCES: ${{ (github.event_name == 'pull_request' && vars.CG_CHAIN == '1') "
            "&& 'snv-video,resolume-snv' || 'snv-video' }}") in step, step
    # the same condition the recording step arms the CG profile with (cg-chain-e2e.sh: CG_CHAIN = "1")
    assert "CG_CHAIN: ${{ github.event_name == 'pull_request' && vars.CG_CHAIN || '0' }}" in wf.read_text()


def test_the_fleet_deploy_declares_the_snv_cams():
    step = _yml_step(ROOT / ".github" / "workflows" / "ci.yml", "Take the shared rig lease")
    assert re.search(r"(?m)^\s+RIG_LEASE_RESOURCES: snv-cams$", step), step


@pytest.mark.parametrize("script, declared", [
    ("av-soak.sh", "snv-video"),
    ("av-restart-matrix.sh", "snv-video"),
    ("cambox-ro-units-apply.sh", "snv-cams"),
])
def test_each_holder_script_declares_its_resources_on_its_acquire(script, declared):
    text = (SCRIPTS / script).read_text()
    assert re.search(r'(?m)^RIG_LEASE_DECLARED="%s"' % re.escape(declared), text), script
    calls = [ln for ln in text.splitlines() if "rig_lease_acquire " in ln and not ln.lstrip().startswith("#")
             and "echo" not in ln and "rig_lease_acquire repo=" not in ln]
    assert calls and all('RIG_LEASE_RESOURCES="$RIG_LEASE_DECLARED" rig_lease_acquire ' in ln for ln in calls), calls


def test_no_script_takes_the_lease_without_a_declaration():
    """A NEW holder must say which boxes it touches (an undeclared acquire holds the whole rig, the
    17.10.2026 SongPlayer wait). rig-busy-gate.sh reads RIG_LEASE_RESOURCES from its caller's env
    (each workflow step declares it, pinned above); rig_lease_write.py passes the POST body's."""
    offenders = []
    for p in sorted(SCRIPTS.rglob("*.sh")):
        if p.name in ("rig-lease.sh", "rig-busy-gate.sh"):
            continue
        for ln in p.read_text().splitlines():
            code = ln.split("#", 1)[0]
            if re.search(r"\brig_lease_acquire\s", code) and "RIG_LEASE_RESOURCES=" not in code \
                    and "echo" not in code:
                offenders.append(f"{p.relative_to(ROOT)}: {ln.strip()}")
    assert not offenders, offenders


def test_every_declaration_resolves_against_the_table():
    rlr = _rlr()
    t = rlr.load_table()
    for value in ("snv-video", "snv-video,resolume-snv", "snv-cams"):
        assert rlr.resolve(rlr.split_names(value), t)


def test_the_ro_units_apply_holds_only_the_snv_cams(tmp_path):
    sys.path.insert(0, str(HERE))
    import test_cambox_ro_units_1394 as ro  # noqa: E402  (its CLI harness: fake boxes, fake OBS)
    r, sshdir = ro._cli(tmp_path, "--apply", "--box", "cam3", env={"FAKE_RECORD_LEASE": "1"})
    assert r.returncode == 0, r.stdout + r.stderr
    assert json.loads((sshdir / "lease-0").read_text())["resources"] == ["snv-cams"]
    assert not (tmp_path / "rig-lease").exists()
