"""issue 1443 -- a healthy LOUD QPSK marker on the stream `mbc` input reads PRESENT, never POLLUTED.

Since cam2 was rewired the healthy marker drives the measurement mic's AGC to -16..-5 dB (green E2E
runs, issue 1443 body), above the old issue-1323 -20 dB level ceiling, so the watchdog logged
`verdict=POLLUTED` on every healthy pass and the development handover's `mic` item stayed UNKNOWN.
A level alone cannot tell the marker from a flood; the issue-1324 decodability preflight is the
pollution test. The watchdog therefore grades the level against the -60 dB silence floor only.

Tier-0: the watchdog's own `MEASUREMENT_AUDIO_FETCH_CMD` replay seam, a failing `sshpass` stub (the
rig-mode probe reads UNKNOWN = proceed as TEST), --dry-run, no rig.
"""
import os
import pathlib
import stat
import subprocess

_REPO = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _REPO / "scripts" / "measurement-audio-alert-watchdog.sh"


def _exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _run(tmp_path, peak_db):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _exe(bindir / "sshpass", "#!/usr/bin/env bash\nexit 255\n")
    fetch = _exe(tmp_path / "fetch.sh",
                 f"#!/usr/bin/env bash\nprintf 'meter_present=1\\npeak_db={peak_db}\\n'\n")
    env = dict(os.environ)
    env.update({
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "MEASUREMENT_AUDIO_FETCH_CMD": str(fetch),
        "MEASUREMENT_AUDIO_ALERT_STATE_DIR": str(tmp_path),
        "MEASUREMENT_AUDIO_WS_HOST": "192.0.2.1",
        "RIG_MODE_PROBE_TIMEOUT": "2",
    })
    return subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True,
                          env=env, timeout=60)


def test_healthy_loud_marker_reads_present(tmp_path):
    r = _run(tmp_path, -14.0)
    assert r.returncode == 0, r.stderr
    assert "verdict=PRESENT" in r.stderr, r.stderr
    assert "POLLUTED" not in r.stderr, r.stderr


def test_the_loudest_green_run_level_reads_present(tmp_path):
    # runs 36744684590 / 36676576964 measured max_volume -5.2 dB and passed the A/V gate
    r = _run(tmp_path, -5.2)
    assert "verdict=PRESENT" in r.stderr, r.stderr


def test_digital_silence_still_reads_silent(tmp_path):
    r = _run(tmp_path, -91.0)
    assert "verdict=SILENT" in r.stderr, r.stderr
