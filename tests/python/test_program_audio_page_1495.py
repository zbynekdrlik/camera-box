"""issue 1495 (owner ROZHODNUTÉ 8.11.2026): missing program audio during a production pages the owner.

On 8.11.2026 stream-snv lost all program audio for 58 min (the USB VBAN NIC vanished). The
measurement-audio watchdog read `peak_db=-100.0` but its pings were suppressed by the owner policy,
and in EVENT mode it does not look at all. The production arm: while stream OBS STREAMS and the `mbc`
input reads digital silence (< -90 dB), confirmed over 2 passes, notify with the airuleset `page:`
class (the only class the owner policy lets through, max one per 10 min).

Tier-0: the pure decision, plus the watchdog run in --dry-run through its MEASUREMENT_AUDIO_FETCH_CMD
replay seam and a stub `sshpass` that prints a cam2 painter snapshot (EVENT = no painter).
"""
import importlib.util
import os
import pathlib
import stat
import subprocess

_REPO = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _REPO / "scripts" / "measurement-audio-alert-watchdog.sh"
_DECIDE = _REPO / "scripts" / "measurement_audio_decision.py"

_spec = importlib.util.spec_from_file_location("measurement_audio_decision_1495", _DECIDE)
mad = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mad)

EVENT_SNAPSHOT = "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0\n"


# --- the pure decision --------------------------------------------------------------------------------

def test_streaming_and_digital_silence_pages():
    assert mad.production_page(-100.0, 1, 1, 1) == "PAGE"
    assert mad.production_page(-90.5, 1, 1, 1) == "PAGE"


def test_a_quiet_live_program_never_pages():
    # a pause in a service still carries room/mic noise, far above digital silence
    assert mad.production_page(-75.0, 1, 1, 1) == "OK"
    assert mad.production_page(-90.0, 1, 1, 1) == "OK"


def test_not_streaming_never_pages():
    assert mad.production_page(-100.0, 1, 0, 1) == "OK"


def test_no_reading_or_unreachable_never_pages():
    assert mad.production_page(None, 1, 1, 1) == "OK"
    assert mad.production_page(-100.0, 0, 1, 1) == "OK"
    assert mad.production_page(-100.0, 1, 1, 0) == "SKIP"


def test_stream_active_is_parsed_and_absent_reads_not_streaming():
    assert mad.stream_active_from_probe("meter_present=1\npeak_db=-100.0\nstream_active=1\n") == 1
    assert mad.stream_active_from_probe("meter_present=1\npeak_db=-100.0\nstream_active=0\n") == 0
    assert mad.stream_active_from_probe("meter_present=1\npeak_db=-100.0\n") == 0


def test_cli_production_prints_the_page_verdict():
    r = subprocess.run(["python3", str(_DECIDE), "production", "--box-reachable", "1"],
                       input="meter_present=1\npeak_db=-100.0\nstream_active=1\n",
                       capture_output=True, text=True, check=True)
    assert "page=PAGE" in r.stdout, r.stdout


# --- the watchdog --------------------------------------------------------------------------------------

def _exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _passes(tmp_path, body, snapshot=EVENT_SNAPSHOT, n=2):
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    snap = tmp_path / "snapshot.txt"
    snap.write_text(snapshot)
    _exe(bindir / "sshpass", f"#!/usr/bin/env bash\ncat '{snap}'\n")
    fetch = _exe(tmp_path / "fetch.sh", "#!/usr/bin/env bash\nprintf '%s' '" + body + "'\n")
    env = dict(os.environ)
    env.update({
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "MEASUREMENT_AUDIO_FETCH_CMD": str(fetch),
        "MEASUREMENT_AUDIO_ALERT_STATE_DIR": str(tmp_path),
        "MEASUREMENT_AUDIO_WS_HOST": "192.0.2.1",
        "RIG_MODE_PROBE_TIMEOUT": "2",
    })
    out = []
    for _ in range(n):
        r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True,
                           env=env, timeout=60)
        assert r.returncode == 0, r.stderr
        out.append(r.stdout + r.stderr)
    return out


def test_event_mode_streaming_silence_pages_with_the_page_key(tmp_path):
    first, second = _passes(tmp_path, "meter_present=1\npeak_db=-100.0\nstream_active=1\n")
    assert "rig mode" in first and "EVENT" in first, first
    assert "WOULD page" not in first, first
    assert "WOULD page" in second, second
    assert "page:stream-snv-program-audio" in second, second


def test_event_mode_not_streaming_does_not_page(tmp_path):
    first, second = _passes(tmp_path, "meter_present=1\npeak_db=-100.0\nstream_active=0\n")
    assert "WOULD page" not in second, second


def test_event_mode_live_program_does_not_page(tmp_path):
    first, second = _passes(tmp_path, "meter_present=1\npeak_db=-30.0\nstream_active=1\n")
    assert "WOULD page" not in second, second


def test_the_page_notify_line_carries_the_page_class_and_the_bucket_helper():
    text = _WATCHDOG.read_text()
    line = [ln for ln in text.splitlines() if "page:stream-snv-program-audio" in ln and "dedup-key" in ln]
    assert len(line) == 1, line
    assert "watchdog_notify_key" in line[0], line[0]
