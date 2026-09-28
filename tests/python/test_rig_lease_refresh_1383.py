"""issue 1383 -- the rig-lease HOLDER keeps its lease truthful while it is alive.

Found 27.9.2026 22:15 UTC: a healthy release E2E read as "hung" on the dev1 :8890 lease JSON
(`ttl_s -273`, `heartbeat_age_s 2959`), because the lease heartbeat was touched only by the gate at
acquire and `expected_release_at` stayed acquire + 45 min for a ~60-70 min run.

The fix (the main's design, Approach 1): `rig_lease_refresh_if_mine <repo> <run_id>
[<lookahead_s>]` in scripts/lib/rig-lease.sh bumps the lease heartbeat and rolls
`expected_release_at` forward to max(current, now + look-ahead) -- ONLY when the current
holder.json names this run, never a foreign holder, never re-creating a released lease -- and the
issue-281 refresher `rig_heartbeat_start` (scripts/lib/rig-heartbeat.sh), which already runs for the
whole E2E and the av-soak, calls it on every beat.

Every test runs the REAL bash libs against a tmp RIG_LEASE_DIR + CAMERA_BOX_RIG_HEARTBEAT: the live
dev1 lease under /var/tmp/rig-lease is never read or written (an E2E or the av-soak may hold it
while this runs). Tier-0: bash + python only, no cargo.
"""
import json
import os
import subprocess
import threading
import time
from datetime import datetime, timedelta, timezone

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LEASE_LIB = os.path.join(REPO, "scripts", "lib", "rig-lease.sh")
HEARTBEAT_LIB = os.path.join(REPO, "scripts", "lib", "rig-heartbeat.sh")
AV_SOAK = os.path.join(REPO, "scripts", "av-soak.sh")

REPO_ID = "zbynekdrlik/camera-box"
RUN_ID = "36351718907"
ISO = "%Y-%m-%dT%H:%M:%SZ"


def _now():
    return datetime.now(timezone.utc).replace(microsecond=0)


def _iso(dt):
    return dt.strftime(ISO)


def _parse(s):
    return datetime.strptime(s, ISO).replace(tzinfo=timezone.utc)


@pytest.fixture
def lease(tmp_path):
    return tmp_path / "rig-lease"


def _write_holder(lease_dir, **overrides):
    lease_dir.mkdir(exist_ok=True)
    holder = {
        "repo": REPO_ID,
        "run_id": RUN_ID,
        "run_url": f"https://github.com/{REPO_ID}/actions/runs/{RUN_ID}",
        "job": "full-path",
        "acquired_at": _iso(_now() - timedelta(minutes=50)),
        "expected_release_at": _iso(_now() - timedelta(minutes=5)),
    }
    holder.update(overrides)
    (lease_dir / "holder.json").write_text(json.dumps(holder))
    return holder


def _age_heartbeat(lease_dir, age_s=3000):
    hb = lease_dir / "heartbeat"
    if not hb.exists():
        hb.write_text("")
    stamp = time.time() - age_s
    os.utime(hb, (stamp, stamp))
    return stamp


def _hb_age(lease_dir):
    return time.time() - (lease_dir / "heartbeat").stat().st_mtime


def _env(tmp_path, lease_dir, **extra):
    # A MINIMAL environment on purpose: CI exports GITHUB_REPOSITORY/GITHUB_RUN_ID, and the refresher
    # reads its lease identity from them, so no test may inherit the runner's own identity.
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(tmp_path),
        "LANG": "C.UTF-8",
        "RIG_LEASE_DIR": str(lease_dir),
        "CAMERA_BOX_RIG_HEARTBEAT": str(tmp_path / "rig-active"),
    }
    env.update(extra)
    return env


def _refresh(tmp_path, lease_dir, *args, **extra):
    quoted = " ".join("'" + a.replace("'", "'\\''") + "'" for a in args)
    script = f'set -uo pipefail\n. "$LEASE_LIB"\nrig_lease_refresh_if_mine {quoted}\n'
    return subprocess.run(["bash", "-c", script], env=_env(tmp_path, lease_dir, LEASE_LIB=LEASE_LIB,
                                                           **extra),
                          capture_output=True, text=True, timeout=30)


# --- the helper: rig_lease_refresh_if_mine -------------------------------------------------------


def test_own_holder_is_refreshed_heartbeat_and_release_time(tmp_path, lease):
    holder = _write_holder(lease)
    _age_heartbeat(lease)
    before = _now()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "RIG_LEASE_REFRESH=refreshed" in r.stdout
    assert _hb_age(lease) < 5, "the lease heartbeat is bumped"
    after = json.loads((lease / "holder.json").read_text())
    exp = _parse(after["expected_release_at"])
    assert before + timedelta(seconds=895) <= exp <= _now() + timedelta(seconds=905)
    for k in ("repo", "run_id", "run_url", "job", "acquired_at"):
        assert after[k] == holder[k], f"{k} is preserved"


def test_default_look_ahead_is_15_minutes_and_env_overrides_it(tmp_path, lease):
    _write_holder(lease)
    before = _now()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID)
    assert r.returncode == 0, r.stdout + r.stderr
    exp = _parse(json.loads((lease / "holder.json").read_text())["expected_release_at"])
    assert before + timedelta(seconds=895) <= exp <= _now() + timedelta(seconds=905)

    _write_holder(lease)
    before = _now()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, RIG_LEASE_LOOKAHEAD_SECS="120")
    assert r.returncode == 0, r.stdout + r.stderr
    exp = _parse(json.loads((lease / "holder.json").read_text())["expected_release_at"])
    assert before + timedelta(seconds=115) <= exp <= _now() + timedelta(seconds=125)


def test_a_later_declared_release_is_never_moved_backward(tmp_path, lease):
    # the av-soak declares its whole run length up front; the roll must never shrink it
    _write_holder(lease, expected_release_at="2099-01-01T00:00:00Z")
    _age_heartbeat(lease)
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 0, r.stdout + r.stderr
    assert json.loads((lease / "holder.json").read_text())["expected_release_at"] \
        == "2099-01-01T00:00:00Z"
    assert _hb_age(lease) < 5, "the heartbeat is still bumped"


def test_an_unparseable_release_time_on_our_own_holder_is_rolled(tmp_path, lease):
    _write_holder(lease, expected_release_at="not-a-time")
    before = _now()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 0, r.stdout + r.stderr
    exp = _parse(json.loads((lease / "holder.json").read_text())["expected_release_at"])
    assert exp >= before + timedelta(seconds=895)


def test_our_own_lease_with_a_missing_heartbeat_gets_one(tmp_path, lease):
    _write_holder(lease)
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 0, r.stdout + r.stderr
    assert (lease / "heartbeat").is_file() and _hb_age(lease) < 5


@pytest.mark.parametrize("holder_overrides", [
    {"run_id": "99999999999"},                  # another run of the same repo
    {"repo": "zbynekdrlik/restreamer"},         # the same run id in another repo
    {"repo": "camera-box-av-soak", "run_id": "av-soak-20260927T230634Z-959315"},
])
def test_a_foreign_holder_is_left_untouched(tmp_path, lease, holder_overrides):
    _write_holder(lease, **holder_overrides)
    stamp = _age_heartbeat(lease)
    raw = (lease / "holder.json").read_bytes()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "RIG_LEASE_REFRESH=not-mine" in r.stdout
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1, "a foreign heartbeat never moves"
    assert sorted(p.name for p in lease.iterdir()) == ["heartbeat", "holder.json"]


def test_a_missing_lease_dir_is_a_no_op_and_creates_nothing(tmp_path, lease):
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "RIG_LEASE_REFRESH=no-lease" in r.stdout
    assert not lease.exists(), "a released lease is never re-created"


def test_a_lease_dir_without_holder_json_is_left_alone(tmp_path, lease):
    lease.mkdir()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 1, r.stdout + r.stderr
    assert list(lease.iterdir()) == [], "no heartbeat and no holder.json appear"


def test_a_corrupt_holder_json_is_left_untouched(tmp_path, lease):
    lease.mkdir()
    (lease / "holder.json").write_text('{"repo": "zbynekdrlik/camera-box", "run_id": ')
    stamp = _age_heartbeat(lease)
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 1, r.stdout + r.stderr
    assert (lease / "holder.json").read_text() == '{"repo": "zbynekdrlik/camera-box", "run_id": '
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_an_empty_identity_never_matches_even_an_empty_holder(tmp_path, lease):
    _write_holder(lease, repo="", run_id="")
    raw = (lease / "holder.json").read_bytes()
    stamp = _age_heartbeat(lease)
    r = _refresh(tmp_path, lease, "", "", "900")
    assert r.returncode == 1, r.stdout + r.stderr
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_the_rewrite_is_an_atomic_rename_never_an_in_place_write(tmp_path, lease):
    _write_holder(lease)
    ino_before = (lease / "holder.json").stat().st_ino
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 0, r.stdout + r.stderr
    assert (lease / "holder.json").stat().st_ino != ino_before, \
        "holder.json is replaced by rename (temp + rename), never truncated and rewritten in place"
    assert sorted(p.name for p in lease.iterdir()) == ["heartbeat", "holder.json"], \
        "no temp file is left behind"


def test_a_concurrent_reader_never_sees_a_partial_holder_json(tmp_path, lease):
    # the :8890 server and every peer read holder.json at any moment; each refresh that moves the
    # release time must hand them either the old or the new complete file
    _write_holder(lease)
    bad, reads, stop = [], [0], threading.Event()

    def reader():
        while not stop.is_set():
            try:
                with open(lease / "holder.json") as f:
                    json.load(f)
                reads[0] += 1
            except FileNotFoundError:
                bad.append("missing")
            except ValueError as e:
                bad.append(repr(e))

    t = threading.Thread(target=reader)
    t.start()
    try:
        # a growing look-ahead, so EVERY refresh moves the release time forward and rewrites
        for k in range(15):
            r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, str(1000 + 60 * k))
            assert r.returncode == 0, r.stdout + r.stderr
    finally:
        stop.set()
        t.join()
    assert reads[0] > 0
    assert bad == [], bad[:3]


# --- the issue-281 refresher calls it ------------------------------------------------------------


def _start_refresher(tmp_path, lease, hold_s, args="", **extra):
    script = (
        'set -uo pipefail\n. "$HEARTBEAT_LIB"\n'
        f'rig_heartbeat_start {args}\n'
        f'sleep {hold_s}\n'
        'rig_heartbeat_stop\n'
    )
    env = _env(tmp_path, lease, HEARTBEAT_LIB=HEARTBEAT_LIB, **extra)
    return subprocess.Popen(["bash", "-c", script], env=env, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True)


def _wait_for(pred, timeout_s):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(0.1)
    return pred()


def _release_time(lease):
    try:
        return _parse(json.loads((lease / "holder.json").read_text())["expected_release_at"])
    except (ValueError, KeyError, FileNotFoundError):
        return None


def test_the_refresher_start_beat_refreshes_the_e2e_own_lease(tmp_path, lease):
    # recording-e2e.sh calls `rig_heartbeat_start "recording-e2e"` unchanged: the identity is the
    # GitHub run the gate took the lease for (GITHUB_REPOSITORY / GITHUB_RUN_ID of the E2E step)
    _write_holder(lease)
    _age_heartbeat(lease)
    p = _start_refresher(tmp_path, lease, 3, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="60",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID)
    try:
        assert _wait_for(lambda: _hb_age(lease) < 5, 2.5), "the start beat bumps the lease heartbeat"
        exp = _release_time(lease)
        assert exp is not None and exp >= _now() + timedelta(seconds=880)
    finally:
        out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err


def test_every_refresher_beat_keeps_the_lease_fresh(tmp_path, lease):
    _write_holder(lease)
    p = _start_refresher(tmp_path, lease, 6, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="1",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID)
    try:
        assert _wait_for(lambda: (lease / "heartbeat").exists() and _hb_age(lease) < 5, 2.5)
        # age the lease again AFTER the start beat: only the loop can refresh it now
        _write_holder(lease)
        _age_heartbeat(lease)
        assert _wait_for(lambda: _hb_age(lease) < 5, 3.5), "a loop beat bumps the lease heartbeat"
        assert _wait_for(lambda: (_release_time(lease) or _now()) >= _now()
                         + timedelta(seconds=880), 3.5), "a loop beat rolls expected_release_at"
    finally:
        out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err


def test_the_soak_passes_its_own_lease_identity_explicitly(tmp_path, lease):
    soak_repo, soak_run = "camera-box-av-soak", "av-soak-20260928T000000Z-4242"
    _write_holder(lease, repo=soak_repo, run_id=soak_run, run_url="", job="av-soak")
    _age_heartbeat(lease)
    p = _start_refresher(tmp_path, lease, 3, f'av-soak "{soak_repo}" "{soak_run}"',
                         RIG_HEARTBEAT_REFRESH_SEC="60")
    try:
        assert _wait_for(lambda: _hb_age(lease) < 5, 2.5)
    finally:
        out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err


def test_the_refresher_never_touches_a_foreign_holder(tmp_path, lease):
    _write_holder(lease, repo="camera-box-av-soak", run_id="av-soak-20260927T230634Z-959315")
    raw = (lease / "holder.json").read_bytes()
    stamp = _age_heartbeat(lease)
    p = _start_refresher(tmp_path, lease, 3, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="1",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID)
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_no_identity_means_the_refresher_leaves_the_lease_alone(tmp_path, lease):
    # a local run (no GitHub env, no explicit identity) never holds the CI lease
    _write_holder(lease)
    raw = (lease / "holder.json").read_bytes()
    stamp = _age_heartbeat(lease)
    p = _start_refresher(tmp_path, lease, 3, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="1")
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_rig_lease_run_id_takes_precedence_like_the_gate(tmp_path, lease):
    # rig-busy-gate.sh: RIG_LEASE_RUN_ID wins over GITHUB_RUN_ID; the refresher resolves the same way
    _write_holder(lease)
    raw = (lease / "holder.json").read_bytes()
    stamp = _age_heartbeat(lease)
    p = _start_refresher(tmp_path, lease, 3, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="1",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID,
                         RIG_LEASE_RUN_ID="some-other-run")
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_a_dead_owner_s_lease_is_never_kept_alive(tmp_path, lease):
    # the refresher outlives a SIGKILLed harness by one tick at most; it must stop beating the lease
    # the moment its owner is gone, so a dead run's lease still goes stale and is reclaimable
    _write_holder(lease)
    dead = subprocess.Popen(["true"])
    dead.wait()
    p = _start_refresher(tmp_path, lease, 4, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="1",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID,
                         RIG_HEARTBEAT_OWNER_PID=str(dead.pid))
    try:
        assert _wait_for(lambda: (lease / "heartbeat").exists() and _hb_age(lease) < 5, 2.5)
        stamp = _age_heartbeat(lease)
        time.sleep(2.5)
        assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1
    finally:
        out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err


# --- the av-soak uses the one helper -------------------------------------------------------------


def test_the_av_soak_keeps_its_lease_through_the_one_helper():
    src = open(AV_SOAK).read()
    assert 'rig_heartbeat_start av-soak "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS"' in src
    assert 'rig_lease_refresh_if_mine "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS"' in src
    assert "rig_lease_heartbeat_touch" not in src, \
        "a bare heartbeat touch would bump a foreign holder's heartbeat too"


# --- review round: the hold ceiling, the gate bridge, a visible start beat -----------------------

RIG_BUSY_GATE = os.path.join(REPO, "scripts", "rig-busy-gate.sh")
FULL_PATH_WORKFLOW = os.path.join(REPO, ".github", "workflows", "full-path-e2e.yml")


def test_past_the_hold_ceiling_the_lease_is_no_longer_beaten(tmp_path, lease):
    # a still-running but stuck holder must age into the heartbeat-stale reclaim again: the keep-alive
    # stops at acquired_at + RIG_LEASE_MAX_HOLD_SECS (default 4500 s = the full-path job timeout)
    _write_holder(lease, acquired_at=_iso(_now() - timedelta(seconds=4600)))
    raw = (lease / "holder.json").read_bytes()
    stamp = _age_heartbeat(lease)
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 3, r.stdout + r.stderr
    assert "RIG_LEASE_REFRESH=over-hold" in r.stdout
    assert (lease / "holder.json").read_bytes() == raw
    assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1


def test_a_holder_may_declare_a_longer_hold_ceiling(tmp_path, lease):
    _write_holder(lease, acquired_at=_iso(_now() - timedelta(seconds=4600)))
    _age_heartbeat(lease)
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900", RIG_LEASE_MAX_HOLD_SECS="30000")
    assert r.returncode == 0, r.stdout + r.stderr
    assert _hb_age(lease) < 5


def test_an_unanchored_hold_is_refused_never_beaten_forever(tmp_path, lease):
    _write_holder(lease, acquired_at="garbage")
    raw = (lease / "holder.json").read_bytes()
    r = _refresh(tmp_path, lease, REPO_ID, RUN_ID, "900")
    assert r.returncode == 3, r.stdout + r.stderr
    assert (lease / "holder.json").read_bytes() == raw


def test_the_default_hold_ceiling_matches_the_full_path_job_timeout():
    import re
    lib = open(LEASE_LIB).read()
    m = re.search(r"RIG_LEASE_MAX_HOLD_SECS:-(\d+)", lib)
    assert m, "rig-lease.sh names the default hold ceiling"
    timeouts = [int(t) for t in re.findall(r"^\s+timeout-minutes:\s*(\d+)\s*$",
                                           open(FULL_PATH_WORKFLOW).read(), re.M)]
    assert timeouts, "full-path-e2e.yml declares a job timeout"
    assert int(m.group(1)) == max(timeouts) * 60, (m.group(1), timeouts)


def test_the_start_beat_reports_the_lease_identity_once_on_stderr(tmp_path, lease):
    _write_holder(lease)
    p = _start_refresher(tmp_path, lease, 1, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="60",
                         GITHUB_REPOSITORY=REPO_ID, GITHUB_RUN_ID=RUN_ID)
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err
    lines = [ln for ln in err.splitlines() if "lease keep-alive" in ln]
    assert len(lines) == 1, err
    assert f"{REPO_ID}#{RUN_ID}" in lines[0] and "RIG_LEASE_REFRESH=refreshed" in lines[0]
    assert out == ""


def test_no_identity_means_no_lease_line_either(tmp_path, lease):
    _write_holder(lease)
    p = _start_refresher(tmp_path, lease, 1, '"recording-e2e"', RIG_HEARTBEAT_REFRESH_SEC="60")
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, out + err
    assert "lease keep-alive" not in err and out == ""


def _pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def _spawn_keepalive(tmp_path, lease, repo=REPO_ID, run_id=RUN_ID, **extra):
    script = f'set -uo pipefail\n. "$LEASE_LIB"\nrig_lease_keepalive_spawn "{repo}" "{run_id}"\n'
    r = subprocess.run(["bash", "-c", script],
                       env=_env(tmp_path, lease, LEASE_LIB=LEASE_LIB, **extra),
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stdout + r.stderr
    line = [ln for ln in r.stdout.splitlines() if ln.startswith("RIG_LEASE_KEEPALIVE=started")]
    assert len(line) == 1, r.stdout
    return int(line[0].split("pid=")[1].split()[0])


def _release(tmp_path, lease):
    return subprocess.run(["bash", "-c", f'. "$LEASE_LIB"\nrig_lease_release "{RUN_ID}"\n'],
                          env=_env(tmp_path, lease, LEASE_LIB=LEASE_LIB),
                          capture_output=True, text=True, timeout=30)


def test_the_keepalive_outlives_its_spawner_and_stops_on_release(tmp_path, lease):
    # rig-busy-gate.sh exits right after the acquire; the lease must stay beaten until the E2E's own
    # refresher takes over (the verdict-exe fetch step + [0/8] ran ~18 min unbeaten, 27.9.2026)
    _write_holder(lease)
    pid = _spawn_keepalive(tmp_path, lease, RIG_LEASE_KEEPALIVE_SEC="1")
    try:
        assert _pid_alive(pid), "the spawner returned, the keep-alive runs on"
        _age_heartbeat(lease)
        assert _wait_for(lambda: _hb_age(lease) < 5, 3.5), "the keep-alive beats the lease"
        assert _wait_for(lambda: (_release_time(lease) or _now()) >= _now()
                         + timedelta(seconds=880), 3.5)
        r = _release(tmp_path, lease)
        assert r.returncode == 0, r.stdout + r.stderr
        assert _wait_for(lambda: not _pid_alive(pid), 4), "a released lease ends the keep-alive"
        assert not lease.exists(), "and it is never re-created"
    finally:
        if _pid_alive(pid):
            os.kill(pid, 15)


def test_the_keepalive_ends_when_the_lease_goes_foreign(tmp_path, lease):
    _write_holder(lease)
    pid = _spawn_keepalive(tmp_path, lease, RIG_LEASE_KEEPALIVE_SEC="1")
    try:
        _write_holder(lease, repo="zbynekdrlik/restreamer", run_id="888")
        stamp = _age_heartbeat(lease)
        assert _wait_for(lambda: not _pid_alive(pid), 4)
        assert abs((lease / "heartbeat").stat().st_mtime - stamp) < 1
    finally:
        if _pid_alive(pid):
            os.kill(pid, 15)


def test_the_keepalive_ends_at_the_hold_ceiling(tmp_path, lease):
    _write_holder(lease)
    pid = _spawn_keepalive(tmp_path, lease, RIG_LEASE_KEEPALIVE_SEC="1")
    try:
        _write_holder(lease, acquired_at=_iso(_now() - timedelta(seconds=5000)))
        assert _wait_for(lambda: not _pid_alive(pid), 4)
    finally:
        if _pid_alive(pid):
            os.kill(pid, 15)


def test_the_keepalive_gives_up_after_repeated_errors(tmp_path, lease):
    # rc 2 (a filesystem error, e.g. a vanished script or an unreadable holder.json) is retried, but
    # never forever outside GitHub Actions: RIG_LEASE_KEEPALIVE_MAX_ERRORS consecutive errors end it
    _write_holder(lease)
    pid = _spawn_keepalive(tmp_path, lease, RIG_LEASE_KEEPALIVE_SEC="1",
                           RIG_LEASE_KEEPALIVE_MAX_ERRORS="2")
    try:
        (lease / "holder.json").unlink()
        (lease / "holder.json").mkdir()  # reading it is an OSError -> rc 2, also as root
        assert _wait_for(lambda: not _pid_alive(pid), 6), "consecutive errors end the keep-alive"
    finally:
        if _pid_alive(pid):
            os.kill(pid, 15)


def test_the_soak_s_error_warning_carries_the_reason():
    src = open(AV_SOAK).read()
    assert 'rig_lease_refresh_if_mine "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS" 2>&1' in src


def test_the_keepalive_needs_an_identity(tmp_path, lease):
    _write_holder(lease)
    script = '. "$LEASE_LIB"\nrig_lease_keepalive_spawn "" ""\n'
    r = subprocess.run(["bash", "-c", script], env=_env(tmp_path, lease, LEASE_LIB=LEASE_LIB),
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and "RIG_LEASE_KEEPALIVE=started" not in r.stdout, r.stdout


def test_the_gate_exits_promptly_and_leaves_the_lease_beaten(tmp_path, lease):
    fake = tmp_path / "fake_obs_phase2.py"
    fake.write_text('import sys\nprint(\'{"busy": false, "reasons": []}\')\nsys.exit(0)\n')
    env = _env(tmp_path, lease, OBS_PHASE2_PY=str(fake), RIG_BUSY_GATE_ITERATIONS="3",
               RIG_BUSY_GATE_SLEEP_SECS="0", RIG_LEASE_REPO=REPO_ID, RIG_LEASE_RUN_ID=RUN_ID,
               RIG_LEASE_RUN_URL="https://example/run", RIG_LEASE_JOB="full-path",
               RIG_LEASE_KEEPALIVE_SEC="1")
    pid = None
    try:
        # capture_output + a timeout: a keep-alive that kept the step's pipes open would hang here
        r = subprocess.run(["bash", RIG_BUSY_GATE], env=env, capture_output=True, text=True,
                           timeout=60)
        assert r.returncode == 0, r.stdout + r.stderr
        assert "OUTCOME=RIG_FREE" in r.stdout and "RIG_LEASE_ACQUIRED" in r.stdout
        line = [ln for ln in r.stdout.splitlines() if ln.startswith("RIG_LEASE_KEEPALIVE=started")]
        assert len(line) == 1, r.stdout
        pid = int(line[0].split("pid=")[1].split()[0])
        _age_heartbeat(lease)
        assert _wait_for(lambda: _hb_age(lease) < 5, 3.5), "the lease is beaten after the gate exit"
    finally:
        _release(tmp_path, lease)
        if pid is not None:
            assert _wait_for(lambda: not _pid_alive(pid), 4)


def test_the_gate_keeps_its_lease_through_the_one_helper():
    src = open(RIG_BUSY_GATE).read()
    assert "rig_lease_heartbeat_touch" not in src
    assert 'rig_lease_refresh_if_mine "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID"' in src
    proceed = src.index("RIG_LEASE_PROCEEDING=1\n")
    spawn = src.index('rig_lease_keepalive_spawn "$RIG_LEASE_REPO" "$RIG_LEASE_RUN_ID"')
    assert proceed < spawn < src.index('report_outcome "OUTCOME=RIG_FREE"'), \
        "the bridge starts only on the success path, where the lease stays held across the exit"


def test_the_av_soak_declares_its_hold_ceiling_and_tells_the_abort_reasons_apart():
    src = open(AV_SOAK).read()
    assert 'RIG_LEASE_MAX_HOLD_SECS="$LEASE_HOLD_S"' in src
    assert src.index('RIG_LEASE_MAX_HOLD_SECS="$LEASE_HOLD_S"') \
        < src.index('rig_heartbeat_start av-soak "$RIG_LEASE_REPO_NAME" "$RIG_LEASE_OURS"'), \
        "the refresher subshell inherits the ceiling only when it is set before the start"
    assert "ran past its declared lease window" in src
    assert "could not refresh the rig lease" in src
