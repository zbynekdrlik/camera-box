"""issue 1459 item (2): the SyncNet advice names the venue's PROGRAM AUDIO sync offset, never the
`2ME PGM` latency, at every venue (one PP/SNV design).

The knob's input is the venue stream box's fact STRIH_PROGRAM_AUDIO_INPUT (stream-pp `Zvuk z mixu`,
stream-snv `mbc`), read through the venue table (scripts/lib/avsync-venues.sh
avsync_venue_audio_input). The dev2 measurer hands it to av_sync_measure.py as `--knob-input`, the
dev1 report pass hands it to avsync_report.py the same way. Absent = the old `2ME PGM` text (a
measurer or report that is not upgraded yet keeps working).

Sign: SyncNet +N = audio LEADS video -> more audio delay -> ZVYS '<input>' sync offset o N;
SyncNet -N = video leads -> ZNIZ '<input>' sync offset o N.
"""

import inspect
import os
import pathlib
import subprocess
import sys
import types

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_measure  # noqa: E402
import avsync_report as ar  # noqa: E402

_VENUES = _SCRIPTS / "lib" / "avsync-venues.sh"
_MEASURER = _SCRIPTS / "avsync-measure-dev2.sh"
_WATCHDOG = _SCRIPTS / "avsync-heartbeat-alert-watchdog.sh"

# (offset_ms, knob_input) -> the expected advice, the ONE table both python files must follow.
VECTORS = [
    (80, "mbc", "ZVYS 'mbc' sync offset o 80"),
    (-120, "mbc", "ZNIZ 'mbc' sync offset o 120"),
    (40, "Zvuk z mixu", "ZVYS 'Zvuk z mixu' sync offset o 40"),
    (-40, "Zvuk z mixu", "ZNIZ 'Zvuk z mixu' sync offset o 40"),
    (120, None, "ZNIZ '2ME PGM' latency o 120"),
    (-40, None, "ZVYS '2ME PGM' latency o 40"),
    (120, "", "ZNIZ '2ME PGM' latency o 120"),
    (0, "mbc", "A/V sync OK"),
    (0, None, "A/V sync OK"),
]


def _bash(script, args=(), env_extra=None):
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent-home-1459"}
    if env_extra:
        env.update(env_extra)
    proc = subprocess.run(
        ["bash", "-c", script, "harness", *args], env=env, capture_output=True, text=True
    )
    return proc


# ── the pure advice text: one shape, byte-identical in both python files ─────────────────────────


def test_measure_knob_advice_follows_the_table():
    for offset, knob, want in VECTORS:
        assert av_sync_measure.knob_advice(offset, knob) == want, (offset, knob)


def test_report_knob_advice_follows_the_table():
    for offset, knob, want in VECTORS:
        assert ar.knob_advice(offset, knob) == want, (offset, knob)


def test_the_two_knob_advice_functions_are_byte_identical():
    a = inspect.getsource(av_sync_measure.knob_advice)
    b = inspect.getsource(ar.knob_advice)
    assert a == b, "av_sync_measure.knob_advice and avsync_report.knob_advice must stay one text"


def test_report_rounds_a_median_to_whole_ms():
    assert ar.knob_advice(80.4, "mbc") == "ZVYS 'mbc' sync offset o 80"
    assert ar.knob_advice(-79.6, "mbc") == "ZNIZ 'mbc' sync offset o 80"


def test_the_measure_sign_doc_names_the_audio_sync_offset():
    doc = av_sync_measure.__doc__
    assert "sync offset" in doc
    assert "RAISE" in doc and "LOWER" in doc


# ── av_sync_measure: the verdict line carries the knob input ─────────────────────────────────────


def _one(monkeypatch, tmp_path, capsys, offset_frames, knob):
    media = tmp_path / "clip.mp4"
    media.write_bytes(b"x")
    monkeypatch.setattr(av_sync_measure, "measure", lambda *a, **k: [(offset_frames, 8.0, None)])
    monkeypatch.setattr(av_sync_measure, "deliver_alert", lambda *a, **k: None)
    args = types.SimpleNamespace(
        grab=None, media=str(media), calibration_log=None, threshold_ms=30,
        outer_loop=False, webhook=None,
    )
    if knob is not None:
        args.knob_input = knob
    rc = av_sync_measure.one_measurement(args, tmp_path)
    assert rc == 0
    return capsys.readouterr().out.strip().splitlines()[-1]


def test_verdict_audio_leads_raises_the_audio_sync_offset(monkeypatch, tmp_path, capsys):
    line = _one(monkeypatch, tmp_path, capsys, 2, "mbc")
    assert line.endswith(":: audio predbieha video o ~80 ms -> ZVYS 'mbc' sync offset o 80"), line


def test_verdict_video_leads_lowers_the_audio_sync_offset(monkeypatch, tmp_path, capsys):
    line = _one(monkeypatch, tmp_path, capsys, -3, "Zvuk z mixu")
    assert line.endswith(
        ":: video predbieha audio o ~120 ms -> ZNIZ 'Zvuk z mixu' sync offset o 120"
    ), line


def test_verdict_without_knob_input_keeps_the_old_text(monkeypatch, tmp_path, capsys):
    line = _one(monkeypatch, tmp_path, capsys, 2, None)
    assert line.endswith(":: audio predbieha video o ~80 ms -> ZNIZ '2ME PGM' latency o 80"), line


def test_measure_cli_accepts_knob_input():
    # The flag parses (the run then stops at the missing syncnet repo, before any measurement).
    proc = subprocess.run(
        [sys.executable, str(_SCRIPTS / "av_sync_measure.py"), "--media", "/nonexistent-1459",
         "--repo", "/nonexistent-repo-1459", "--knob-input", "mbc"],
        capture_output=True, text=True,
    )
    assert "unrecognized arguments" not in proc.stderr, proc.stderr
    assert "syncnet_python repo not found" in (proc.stderr + proc.stdout)


# ── avsync_report: the NESEDÍ advice carries the knob input ──────────────────────────────────────


def _measured(offset_ms, conf=6.0):
    fr = offset_ms // 40
    return (
        "measured: db=-5.0 [2026-10-26 19:00:59] AV offset {:+d} fr ({:+d} ms) conf {:.1f}"
        " :: x".format(fr, offset_ms, conf)
    )


def _periodic(knob):
    start = 1_800_000_000
    rows = [(start + i * 90, _measured(120)) for i in range(15)]
    now = start + 1201
    msgs, _ = ar.decide(rows, {}, now, knob_input=knob)
    return [m for m in msgs if "za 20 min" in m]


def test_report_periodic_advice_names_the_venue_audio_input():
    periodic = _periodic("mbc")
    assert periodic and "NESEDÍ o 120 ms → ZVYS 'mbc' sync offset o 120" in periodic[0], periodic


def test_report_without_knob_input_keeps_the_old_text():
    periodic = _periodic(None)
    assert periodic and "ZNIZ '2ME PGM' latency o 120" in periodic[0], periodic


def test_report_cli_passes_knob_input(tmp_path):
    start = 1_800_000_000
    rows = tmp_path / "rows.tsv"
    rows.write_text("".join(f"{start + i * 90}\t{_measured(-80)}\n" for i in range(15)))
    state = tmp_path / "state.json"
    proc = subprocess.run(
        [sys.executable, str(_SCRIPTS / "avsync_report.py"), "--state", str(state),
         "--rows", str(rows), "--now", str(start + 1201), "--knob-input", "Zvuk z mixu"],
        capture_output=True, text=True,
    )
    assert proc.returncode == 0, proc.stderr
    assert "ZNIZ 'Zvuk z mixu' sync offset o 80" in proc.stdout, proc.stdout


# ── the venue table: the knob input comes from the stream box's fact file ────────────────────────


def _venue_input(venue, env_extra=None):
    return _bash(
        'set -euo pipefail\n. "$1"\navsync_venue_audio_input "$2"', (str(_VENUES), venue),
        env_extra,
    )


def test_venue_audio_input_reads_the_stream_box_facts():
    assert _venue_input("snv").stdout == "mbc"
    assert _venue_input("pp").stdout == "Zvuk z mixu"


def test_venue_audio_input_unknown_venue_fails_closed():
    p = _venue_input("zz")
    assert p.returncode != 0 and p.stdout == ""
    assert "unknown venue" in p.stderr


def test_venue_audio_input_of_a_declared_windows_row_is_empty(tmp_path):
    # A declared row whose box has no fact file: no knob input -> the old `2ME PGM` wording.
    p = _venue_input(
        "snv",
        {"STRIH_BOXES_DIR": str(tmp_path),
         "AVSYNC_DECLARED_STREAM_VENUES": "snv|stream-snv|obs-e2e-test"},
    )
    assert p.returncode == 0, p.stderr
    assert p.stdout == ""


def test_venue_audio_input_none_fact_is_empty(tmp_path):
    (tmp_path / "stream-snv.env").write_text(
        "STRIH_HOSTNAME=stream-snv\nSTRIH_IP=10.99.0.4\nSTRIH_ROLE=stream\nSTRIH_VENUE=snv\n"
        "STRIH_PROGRAM_AUDIO_INPUT=none\n"
    )
    p = _venue_input("snv", {"STRIH_BOXES_DIR": str(tmp_path)})
    assert p.returncode == 0, p.stderr
    assert p.stdout == ""


# ── the dev2 measurer passes the venue's knob input to av_sync_measure.py ────────────────────────


def _run_measure(tmp_path, knob):
    log = tmp_path / "py-argv.log"
    fake = tmp_path / "fake-python"
    fake.write_text(f'#!/bin/bash\nprintf "%s\\n" "$@" > "{log}"\necho ok\n')
    fake.chmod(0o755)
    body = (
        # the measurer parses its own argv when sourced: take ours first, then clear it.
        'set -euo pipefail\nscript="$1"; knob="$2"; set --\n. "$script"\n'
        f'PYTHON_BIN="{fake}"\nMEASURE_SCRIPT=/dev/null\n'
        f'LOCK_FILE="{tmp_path}/lock"\nLOCK_WAIT_S=2\nKNOB_INPUT="$knob"\n'
        "run_measure /dev/null >/dev/null\n"
    )
    p = _bash(body, (str(_MEASURER), knob), {"HOME": str(tmp_path)})
    assert p.returncode == 0, p.stderr
    return log.read_text().splitlines()


def test_measurer_passes_the_knob_input(tmp_path):
    argv = _run_measure(tmp_path, "Zvuk z mixu")
    assert "--knob-input" in argv
    assert argv[argv.index("--knob-input") + 1] == "Zvuk z mixu"


def test_measurer_without_a_knob_input_passes_no_flag(tmp_path):
    assert "--knob-input" not in _run_measure(tmp_path, "")


def test_measurer_resolves_the_knob_input_from_the_venue(tmp_path):
    body = (
        'set -euo pipefail\nscript="$1"; venue="$2"; set --\n. "$script"\nVENUE="$venue"\n'
        'avsync_venue_table_load\nresolve_venue_config\n'
        'printf "%s" "$KNOB_INPUT"\n'
    )
    p = _bash(body, (str(_MEASURER), "pp"), {"HOME": str(tmp_path)})
    assert p.returncode == 0, p.stderr
    assert p.stdout == "Zvuk z mixu"


# ── the dev1 report pass hands the venue's knob input to the decider ─────────────────────────────


def _watchdog_report_argv(tmp_path, extra=None):
    fake = tmp_path / "fake_report.py"
    fake.write_text("import sys\nprint('ARGV ' + ' | '.join(sys.argv[1:]))\n")
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": os.environ.get("HOME", str(tmp_path)),
        "AVSYNC_HEARTBEAT_HOST": "none",
        "AVSYNC_VENUES": "pp",
        "AVSYNC_HEARTBEAT_STATE_FILE": str(tmp_path / "hb.state"),
        "AVSYNC_REPORT_STATE_FILE": str(tmp_path / "report-state.json"),
        "AVSYNC_REPORT_SCRIPT": str(fake),
        "AVSYNC_REPORT_FETCH_CMD": "printf '1\\tmeasured: x\\n'",
        # pp forced AWAY (no live probe): the report pass still runs for an away venue.
        "OBS_FLEET_HOME": "stream-snv",
    }
    env.update(extra or {})
    proc = subprocess.run(
        ["bash", str(_WATCHDOG), "--dry-run"], env=env, capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    argv = [ln for ln in proc.stderr.splitlines() if "WOULD post report message" in ln]
    assert len(argv) == 1, proc.stderr
    return argv[0]


def test_watchdog_report_pass_passes_the_venue_knob_input(tmp_path):
    assert "--knob-input | Zvuk z mixu" in _watchdog_report_argv(tmp_path)


def test_watchdog_report_pass_without_a_knob_input_passes_no_flag(tmp_path):
    # A venue whose stream box has no fact file (a declared Windows row): no --knob-input, the report
    # still runs under the watchdog's `set -u` with an empty argument array.
    facts = tmp_path / "facts"
    facts.mkdir()
    line = _watchdog_report_argv(tmp_path, {
        "STRIH_BOXES_DIR": str(facts),
        "AVSYNC_DECLARED_STREAM_VENUES": "pp|stream-pp|stream-pp",
    })
    assert "--knob-input" not in line, line
    assert "--state" in line and "--rows" in line, line
