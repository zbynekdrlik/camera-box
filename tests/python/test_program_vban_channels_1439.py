"""issue 1439 -- verify item 9 FAILs a program VBAN leg with one silent channel.

Owner 25.10.2026 (verbatim): "na strih-snv obs zvuk beha iba v jednom kanaly". The console sent strih-snv's
`strih-obs` VBAN with the right channel empty (decoded on the box: L -45.4 dBFS, R -111.7 dBFS; OBS 'ASIO zvuk'
meters L -35.1 dB, R -105.2 dB) and verify item 9 passed. These tests pin the pure per-channel decision, the
probe's per-channel read of an InputVolumeMeters event and the grader row through its seam.
"""
import importlib.util
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"


def _probe():
    spec = importlib.util.spec_from_file_location("meter_probe_1439", SCRIPTS / "measurement_audio_meter_probe.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def test_the_live_strih_snv_reading_is_a_silent_channel():
    v, detail = _probe().channel_balance([-35.1, -105.2])
    assert v == "silent-channel" and "channel 2 silent" in detail


def test_a_stereo_program_passes_even_when_unbalanced():
    p = _probe()
    assert p.channel_balance([-51.6, -52.6])[0] == "ok"       # stream-snv's mbc feed, read the same day
    assert p.channel_balance([-20.0, -70.0])[0] == "ok"       # hard-panned material, not an empty channel


def test_nothing_playing_is_quiet_and_no_meter_is_unknown():
    p = _probe()
    assert p.channel_balance([-100.0, -100.0])[0] == "quiet"
    assert p.channel_balance([])[0] == "unknown"
    assert p.channel_balance(None)[0] == "unknown"


def test_the_probe_reads_each_channel_of_the_input():
    p = _probe()
    event = {"inputs": [{"inputName": "other", "inputLevelsMul": [[1.0, 1.0, 1.0]]},
                        {"inputName": "ASIO zvuk", "inputLevelsMul": [[0.01, 0.017, 0.02], [0.0, 0.0, 0.0]]}]}
    assert p._channel_peaks_for_input(event, "ASIO zvuk") == [0.02, 0.0]
    assert p._channel_peaks_for_input(event, "missing") is None


def _grade_row(tmp_path, probe_out):
    f = tmp_path / "channels"
    f.write_text(probe_out)
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_box_load strih-snv\nstrih_program_vban_channels_row 127.0.0.1 "ASIO zvuk"\n')
    env = dict(os.environ, STRIH_PROGRAM_VBAN_CHANNELS_FILE=str(f))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\n. "$2"\n' + body, "h", str(FACTS), str(PROVISION)],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_grader_fails_a_silent_channel_and_names_it(tmp_path):
    row = _grade_row(tmp_path, "channel_verdict=silent-channel\nchannel_detail=channel 2 silent (ch1 -35.1 dB ch2 -105.2 dB)\n")
    assert row.startswith("FAIL (program-vban) OBS 'ASIO zvuk': channel 2 silent"), row


def test_the_grader_passes_ok_and_notes_quiet_or_unread(tmp_path):
    assert _grade_row(tmp_path, "channel_verdict=ok\nchannel_detail=ch1 -40.0 dB ch2 -41.0 dB\n").startswith("PASS")
    assert _grade_row(tmp_path, "channel_verdict=quiet\nchannel_detail=ch1 -100.0 dB\n").startswith("NOTE")
    assert _grade_row(tmp_path, "").startswith("NOTE")


def test_the_grade_runs_the_channel_row():
    lib = (SCRIPTS / "lib" / "strih-program-vban.sh").read_text()
    body = lib[lib.index("strih_program_vban_grade() {"):]
    body = body[:body.index("\n}\n")]
    assert 'strih_program_vban_channels_row "$ws" "$name"' in body
