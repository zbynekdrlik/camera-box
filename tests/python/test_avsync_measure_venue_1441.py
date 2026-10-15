"""issue 1441 -- the dev2 SyncNet measurer runs one instance PER VENUE.

`scripts/avsync-measure-dev2.sh --venue <venue>` takes the grab URL from the venue table
(scripts/lib/avsync-venues.sh) and writes `~/.camera-box/avsync-watchdog-heartbeat-<venue>.txt`, its
own clip file and its own day log `~/avsync/measurements-<venue>-<day>.tsv`. SNV (the default venue,
and the venue of the old single unit) keeps the old heartbeat path as a compatibility symlink. Only
one SyncNet pass runs on dev2 at a time (a flock), so two live venues never share the GPU at once.
A systemd template `avsync-measure-dev2@<venue>.{service,timer}` replaces the single unit, and the
dev2 installer provisions the venue table's files and enables one timer per venue.

Every run here is hermetic: a fake ffmpeg/ffprobe/python on PATH, a temp HOME, no network.
"""

import fcntl
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MEASURER = _ROOT / "scripts" / "avsync-measure-dev2.sh"
_MEASURE_LIB = _ROOT / "scripts" / "lib" / "avsync-measure.sh"
_DEV2_INSTALL = _ROOT / "scripts" / "avsync-dev2-install.sh"
_FRESHNESS = _ROOT / "scripts" / "avsync_freshness.py"


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


def _source(script, body, env_extra=None):
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent-home-1441", "SCRIPT": str(script)}
    if env_extra:
        env.update(env_extra)
    harness = 'set -euo pipefail\n. "$SCRIPT"\n' + body
    proc = subprocess.run(["bash", "-c", harness], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, (
        f"harness exited {proc.returncode}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    )
    return proc.stdout


# ── the pure per-venue builders (scripts/lib/avsync-measure.sh) ──────────────────────────────────


def test_clip_path_is_per_venue():
    assert _source(_MEASURE_LIB, "avsync_measure_clip_path /d pp") == "/d/live-clip-pp.mp4"


def test_day_log_path_is_per_venue():
    out = _source(_MEASURE_LIB, "avsync_measure_log_path /b 2026-10-15 pp")
    assert out == "/b/avsync/measurements-pp-2026-10-15.tsv"


def test_heartbeat_path_is_per_venue_under_camera_box():
    out = _source(_MEASURE_LIB, "avsync_measure_heartbeat_path /h pp")
    assert out == "/h/.camera-box/avsync-watchdog-heartbeat-pp.txt"


def test_legacy_heartbeat_path_is_the_old_single_file():
    out = _source(_MEASURE_LIB, "avsync_measure_legacy_heartbeat_path /h")
    assert out == "/h/.camera-box/avsync-watchdog-heartbeat.txt"


def test_lock_path_sits_in_the_shared_clip_dir():
    out = _source(_MEASURE_LIB, "avsync_measure_lock_path", {"XDG_RUNTIME_DIR": "/nonexistent-xdg"})
    assert out == "/tmp/avsync-measure-dev2/syncnet.lock"


# ── one full measurer pass per venue, hermetic ───────────────────────────────────────────────────


def _pass(tmp_path, args, env_extra=None, pre=None):
    """Run one measurer pass. The fake ffmpeg logs its argv and fails the grab, so the pass ends in an
    honest no-signal heartbeat through the REAL freshness gate -- no network, no SyncNet."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    stub = tmp_path / "stub"
    stub.mkdir(exist_ok=True)
    log = tmp_path / "ffmpeg.log"
    _exe(stub / "ffmpeg", f'#!/bin/sh\nprintf "%s\\n" "$@" > "{log}"\nexit 1\n')
    _exe(stub / "ffprobe", "#!/bin/sh\nexit 1\n")
    if pre:
        pre(home)
    env = {
        "PATH": f"{stub}:/usr/bin:/bin",
        "HOME": str(home),
        "XDG_RUNTIME_DIR": str(tmp_path),
        "AVSYNC_MEASURE_FRESHNESS": str(_FRESHNESS),
    }
    if env_extra:
        env.update(env_extra)
    proc = subprocess.run(
        ["bash", str(_MEASURER), *args], env=env, capture_output=True, text=True, timeout=60
    )
    argv = log.read_text().splitlines() if log.exists() else []
    return proc, home, argv


def _arg_after(argv, flag):
    return argv[argv.index(flag) + 1]


def test_venue_pp_grabs_the_pp_stream_and_writes_its_own_heartbeat(tmp_path):
    proc, home, argv = _pass(tmp_path, ["--venue", "pp"])
    assert proc.returncode == 0, proc.stderr
    assert _arg_after(argv, "-i") == "rtmp://stream-pp.lan:1234/live/stream-pp"
    assert argv[-1].endswith("/live-clip-pp.mp4")
    hb = home / ".camera-box" / "avsync-watchdog-heartbeat-pp.txt"
    assert hb.read_text().split("\t", 1)[1].startswith("no-signal: ")
    # PP is not the legacy venue: the old single heartbeat path is never touched
    assert not os.path.lexists(home / ".camera-box" / "avsync-watchdog-heartbeat.txt")
    logs = sorted(p.name for p in (home / "avsync").iterdir())
    assert len(logs) == 1 and logs[0].startswith("measurements-pp-") and logs[0].endswith(".tsv")


def test_default_venue_is_snv_with_the_old_grab_url(tmp_path):
    proc, home, argv = _pass(tmp_path, [])
    assert proc.returncode == 0, proc.stderr
    assert _arg_after(argv, "-i") == "rtmp://10.77.9.204:1234/live/obs-e2e-test"
    assert (home / ".camera-box" / "avsync-watchdog-heartbeat-snv.txt").exists()


def test_snv_keeps_the_old_heartbeat_path_as_a_symlink(tmp_path):
    proc, home, _argv = _pass(tmp_path, ["--venue", "snv"])
    assert proc.returncode == 0, proc.stderr
    legacy = home / ".camera-box" / "avsync-watchdog-heartbeat.txt"
    assert legacy.is_symlink()
    assert os.readlink(legacy) == "avsync-watchdog-heartbeat-snv.txt"
    assert legacy.read_text() == (home / ".camera-box" / "avsync-watchdog-heartbeat-snv.txt").read_text()


def test_snv_replaces_an_old_unit_heartbeat_file_with_the_symlink(tmp_path):
    def pre(home):
        (home / ".camera-box").mkdir()
        (home / ".camera-box" / "avsync-watchdog-heartbeat.txt").write_text("1\told unit\n")

    proc, home, _argv = _pass(tmp_path, ["--venue", "snv"], pre=pre)
    assert proc.returncode == 0, proc.stderr
    legacy = home / ".camera-box" / "avsync-watchdog-heartbeat.txt"
    assert legacy.is_symlink()
    assert "old unit" not in legacy.read_text()


def test_unknown_venue_refuses_and_writes_nothing(tmp_path):
    proc, home, argv = _pass(tmp_path, ["--venue", "zz"])
    assert proc.returncode != 0
    assert "zz" in proc.stderr
    assert argv == []
    assert not (home / ".camera-box").exists()


def test_explicit_grab_url_and_heartbeat_override_the_table(tmp_path):
    hb = tmp_path / "hb.txt"
    proc, home, argv = _pass(
        tmp_path, ["--venue", "snv", "--grab-url", "rtmp://192.0.2.1:1/x/y", "--heartbeat", str(hb)]
    )
    assert proc.returncode == 0, proc.stderr
    assert _arg_after(argv, "-i") == "rtmp://192.0.2.1:1/x/y"
    assert hb.exists()
    # an explicit heartbeat path is the operator's: no compatibility symlink is created for it
    assert not os.path.lexists(home / ".camera-box" / "avsync-watchdog-heartbeat.txt")


def test_measurer_carries_no_hand_typed_address():
    src = _MEASURER.read_text()
    assert "10.77.9.204" not in src
    assert "obs-e2e-test" not in src
    assert "stream-pp" not in src


# ── one SyncNet pass at a time on dev2 (flock) ───────────────────────────────────────────────────


def _measure_harness(tmp_path, wait_s):
    lock = tmp_path / "syncnet.lock"
    log = tmp_path / "py.log"
    fake_py = _exe(
        tmp_path / "fake-python",
        "#!/bin/bash\n"
        f'if flock -n "{lock}" true; then echo FREE >> "{log}"; else echo HELD >> "{log}"; fi\n'
        "echo 'AV offset +0 fr (+0 ms) conf 8.0 :: A/V sync OK (offset 0 ms)'\n",
    )
    body = (
        f'PYTHON_BIN="{fake_py}"\nMEASURE_SCRIPT=/dev/null\n'
        f'LOCK_FILE="{lock}"\nLOCK_WAIT_S={wait_s}\n'
        'run_measure /dev/null\n'
    )
    return lock, log, body


def test_a_pass_holds_the_syncnet_lock_while_it_measures(tmp_path):
    _lock, log, body = _measure_harness(tmp_path, 5)
    out = _source(_MEASURER, body, {"HOME": str(tmp_path)})
    assert "A/V sync OK" in out
    assert log.read_text().split() == ["HELD"], "SyncNet must run while this pass holds the lock"


def test_a_pass_waits_for_the_other_venue_then_reports_busy(tmp_path):
    lock, log, body = _measure_harness(tmp_path, 1)
    with open(lock, "w") as held:
        fcntl.flock(held, fcntl.LOCK_EX)   # another venue's SyncNet pass is running
        out = _source(_MEASURER, body, {"HOME": str(tmp_path)})
    assert out.startswith("ERROR: ") and "lock" in out and "1s" in out
    assert not log.exists(), "SyncNet must never start while another venue holds the GPU lock"


# ── the systemd template (one instance per venue) ────────────────────────────────────────────────


def test_template_service_runs_the_measurer_for_its_venue():
    svc = (_ROOT / "systemd" / "avsync-measure-dev2@.service").read_text()
    assert "ExecStart=%h/avsync/scripts/avsync-measure-dev2.sh --venue %i" in svc
    assert "Type=oneshot" in svc
    # grab (~50 s) + a wait for the other venue's SyncNet (<= cap) + its own SyncNet (<= cap 180 s)
    timeout = int(svc.split("TimeoutStartSec=", 1)[1].split()[0])
    assert timeout >= 50 + 180 + 180


def test_template_timer_keeps_the_90s_cadence():
    tmr = (_ROOT / "systemd" / "avsync-measure-dev2@.timer").read_text()
    assert "OnUnitInactiveSec=90s" in tmr
    assert "WantedBy=timers.target" in tmr


def test_the_single_unit_is_retired_from_the_repo():
    assert not (_ROOT / "systemd" / "avsync-measure-dev2.service").exists()
    assert not (_ROOT / "systemd" / "avsync-measure-dev2.timer").exists()


# ── the dev2 installer: ships the venue table's files, one timer per venue, retires the old one ───


def test_installer_units_are_the_template_pair():
    out = _source(_DEV2_INSTALL, 'printf "%s\\n" "${UNITS[@]}"')
    assert out.split() == ["avsync-measure-dev2@.service", "avsync-measure-dev2@.timer"]


def test_installer_enables_one_timer_per_venue_and_retires_the_single_unit():
    out = _source(_DEV2_INSTALL, "dev2_enable_remote_cmd")
    assert "systemctl --user daemon-reload" in out
    assert "disable --now avsync-measure-dev2.timer" in out
    assert "enable --now avsync-measure-dev2@snv.timer avsync-measure-dev2@pp.timer" in out
    assert out.index("disable --now avsync-measure-dev2.timer") < out.index("enable --now")


def test_installer_ships_the_venue_table_libs_and_fact_files():
    out = _source(_DEV2_INSTALL, 'printf "%s\\n" "${LIB_FILES[@]}"')
    assert set(out.split()) == {
        "avsync-measure.sh", "avsync-venues.sh", "obs-fleet.sh", "strih-box-facts.sh",
    }
    src = _DEV2_INSTALL.read_text()
    assert "scripts/strih-boxes/" in src
