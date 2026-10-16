"""issue 1447 -- the development handover `mic` item also reads the program-audio guard.

16.10.2026 08:14-08:31Z the mbc chain sent more than the measurement signal into the stream (music /
room sound on top of the QPSK marker). The strih-snv program-audio sampler read FOREIGN, yet the
handover `mic` item graded only the mbc PEAK through the measurement-audio watchdog's --dry-run,
read PRESENT and said OK. Owner ruling (16.10.2026): a foreign mbc is an operator who forgot to
switch it off -- report it at the handover and ask for the fix, nothing more.

So the item gets a second capture, `mic_guard`: `scripts/program_audio_guard.py` run as-is, graded
on its EXIT CODE (the guard's contract): 0 = OK, 1 = FOREIGN = FORGOT-BY-OWNER with the line
"mbc posiela do streamu aj iný zvuk ako merací signál — v Abletone na mbc nechaj do streamu len
merací mikrofón", anything else (2 = UNKNOWN/stale/unreachable, a timeout, a missing probe) =
UNKNOWN, never OK.

Fixtures are the REAL readings of 16.10.2026: FOREIGN rms -15.5 dBFS, 71.4 % outside the marker
band, markers decoded; MEASUREMENT (after the owner's fix) rms -35.7 dBFS, 13.4 %.

Tier-0: pytest + the bash orchestrator with stub probes, no rig.
"""
import json
import os
import pathlib
import stat
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio_guard as guard  # noqa: E402
import rig_dev_handover_decision as d  # noqa: E402

FOREIGN_LINE_TEXT = ("mbc posiela do streamu aj iný zvuk ako merací signál — "
                     "v Abletone na mbc nechaj do streamu len merací mikrofón")

# The measurement-audio watchdog's --dry-run: the mbc PEAK is present (the marker is loud) in BOTH
# readings -- the peak alone cannot tell the foreign flood from the healthy marker.
MIC_PRESENT = ("2026-10-16 08:20:02 [measurement-audio-alert-watchdog] stream "
               "(10.77.9.204:4455): reachable=1 verdict=PRESENT peak_db=-9.8 meter_present=1\n")
MIC_SILENT = ("2026-10-16 08:20:02 [measurement-audio-alert-watchdog] stream "
              "(10.77.9.204:4455): reachable=1 verdict=SILENT peak_db=-71.4 meter_present=1\n")

# program_audio_guard.py's one output line + exit code, the real 16.10.2026 readings
GUARD_FOREIGN = ("program-audio verdict=FOREIGN rms=-15.5 outside_band=71.4% age=1.0 "
                 "markers=8 chain=6\n", 1)
GUARD_MEASUREMENT = ("program-audio verdict=MEASUREMENT rms=-35.7 outside_band=13.4% age=0.9 "
                     "markers=8 chain=7\n", 0)
GUARD_UNREACHABLE = ("program-audio verdict=UNKNOWN rms=- outside_band=-% age=- markers=- chain=- "
                     "reason=unreachable: http://10.77.9.202:8891/program-audio.json: "
                     "[Errno 111] Connection refused\n", 2)


def _mic():
    return next(i for i in d.ITEMS if i.key == "mic")


def _decide(mic, guard_capture):
    caps = {"mic": mic}
    if guard_capture is not None:
        caps["mic_guard"] = guard_capture
    return _mic().decide(caps)


def test_the_guard_contract_gives_the_fixture_exit_codes():
    # the fixtures are what the real guard prints for the real readings, not hand-picked codes
    foreign = {"verdict": "FOREIGN", "rms_dbfs": -15.5, "outside_band_pct": 71.4, "age_s": 1.0,
               "markers_decoded": 8, "marker_chain": 6}
    measurement = {"verdict": "MEASUREMENT", "rms_dbfs": -35.7, "outside_band_pct": 13.4,
                   "age_s": 0.9, "markers_decoded": 8, "marker_chain": 7}
    assert guard.decide(foreign, 10.0) == (GUARD_FOREIGN[1], GUARD_FOREIGN[0].rstrip("\n"))
    assert guard.decide(measurement, 10.0) == (GUARD_MEASUREMENT[1],
                                               GUARD_MEASUREMENT[0].rstrip("\n"))


def test_mic_item_reads_the_guard_capture():
    assert "mic_guard" in _mic().captures


def test_foreign_mbc_is_forgot_by_owner_with_the_ableton_line():
    e = _decide((MIC_PRESENT, 0), GUARD_FOREIGN)
    assert e["status"] == d.FORGOT
    assert FOREIGN_LINE_TEXT in e["message"]


def test_measurement_mbc_is_ok():
    e = _decide((MIC_PRESENT, 0), GUARD_MEASUREMENT)
    assert e["status"] == d.OK


def test_an_unreadable_guard_is_unknown_never_ok():
    for cap in (GUARD_UNREACHABLE, ("", 124), ("MISSING: x\n", d.RC_MISSING)):
        e = _decide((MIC_PRESENT, 0), cap)
        assert e["status"] == d.UNKNOWN, cap
    # no guard capture at all (an older orchestrator) is UNKNOWN too, never a silent OK
    assert _decide((MIC_PRESENT, 0), None)["status"] == d.UNKNOWN


def test_a_silent_mic_keeps_its_own_line_and_never_names_the_foreign_fix():
    e = _decide((MIC_SILENT, 0), GUARD_MEASUREMENT)
    assert e["status"] == d.FORGOT
    assert "odmutuj" in e["message"]
    assert FOREIGN_LINE_TEXT not in e["message"]


def test_silent_mic_and_foreign_guard_name_both_fixes():
    e = _decide((MIC_SILENT, 0), GUARD_FOREIGN)
    assert e["status"] == d.FORGOT
    assert "odmutuj" in e["message"]
    assert FOREIGN_LINE_TEXT in e["message"]
    assert e["message"].index("odmutuj") < e["message"].index(FOREIGN_LINE_TEXT)


def test_the_checklist_summary_blames_the_owner_for_a_foreign_mbc():
    e = _decide((MIC_PRESENT, 0), GUARD_FOREIGN)
    lines, summary, code = d.build_checklist([e])
    assert code == 1
    assert summary.startswith("zabudol si: merací mikrofón (mbc)")
    assert FOREIGN_LINE_TEXT in lines[0]


# --- the orchestrator runs the guard, bounded, and hands its exit code to the decision ----------
def _exe(path: pathlib.Path, text: str) -> pathlib.Path:
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def test_the_orchestrator_names_the_guard_probe():
    s = (_SCRIPTS / "rig-dev-handover-check.sh").read_text(encoding="utf-8")
    assert 'MIC_GUARD_PROBE="${RDH_MIC_GUARD_PROBE:-$HERE/program_audio_guard.py}"' in s
    assert s.count('run_probe mic_guard python3 "$MIC_GUARD_PROBE"') == 1
    assert s.index("run_probe mic_guard") < s.index('python3 "$DECIDE"')


def test_the_orchestrator_reports_a_foreign_guard_as_forgot(tmp_path):
    stub = _exe(tmp_path / "program_audio_guard.py",
                "import sys\n"
                f"print({GUARD_FOREIGN[0].rstrip(chr(10))!r})\n"
                "sys.exit(1)\n")
    mic_text = tmp_path / "mic-dry-run.txt"
    mic_text.write_text(MIC_PRESENT, encoding="utf-8")
    mic = _exe(tmp_path / "measurement-audio-alert-watchdog.sh",
               f'#!/usr/bin/env bash\ncat "{mic_text}" >&2\n')
    missing = str(tmp_path / "missing")
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(tmp_path),
        "TMPDIR": str(tmp_path),
        "RDH_TIMEOUT": "10",
        "RDH_MIC_PROBE": str(mic),
        "RDH_MIC_GUARD_PROBE": str(stub),
        "RDH_IMAG_RETIRED": "1",
    }
    for item in ("PAINTER", "CLOCK", "OBS", "NET", "AUDIOLAG", "GENLOCK", "BURN", "MAPPING",
                 "PINS", "DANTESYNC", "CAMBOX", "AVLATENCY", "EXPOSURE", "RIGMODE_LIB",
                 "WATCHDOG_ROSTER", "CAMSET_LIB"):
        key = f"RDH_{item}" if item.endswith("_LIB") or item == "WATCHDOG_ROSTER" else f"RDH_{item}_PROBE"
        env[key] = missing
    r = subprocess.run(["bash", str(_SCRIPTS / "rig-dev-handover-check.sh"), "--json"],
                       capture_output=True, text=True, env=env, timeout=120)
    out = json.loads(r.stdout)
    mic_entry = next(e for e in out["items"] if e["key"] == "mic")
    assert mic_entry["status"] == d.FORGOT, (mic_entry, r.stderr)
    assert FOREIGN_LINE_TEXT in mic_entry["message"]
    assert r.returncode == 1
