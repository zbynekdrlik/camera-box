"""issue 1387 slice 3 (design 6558387590): the full-path E2E report names the inputs the run MEASURED.

On a JPEG XS venue the strih cameras arrive as `JXS camN src` and the stream program as `JXS PGM src`;
the report used to print the hard-coded `'NDI 2ME PGM' hold` and "Video sync NDI kamier" whatever the
run measured. recording-e2e.sh hands what the ONE resolver (scripts/lib/program-inputs.sh) set through
the existing e2e_discord_report_send call; the composer prints it, and the pins snapshot reads the pin
of the stream input the run measured. An NDI run (or an old caller that passes nothing) renders as
before; the phone summary's PASS shape (3 lines, no input name) never changes.
"""
import json
import os
import pathlib
import subprocess
import sys

_REPO = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _REPO / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402
import latency_pins_snapshot as lps  # noqa: E402

_FIX = _REPO / "tests" / "python" / "fixtures" / "e2e_discord_report"
_SENDER = _SCRIPTS / "lib" / "e2e-discord-report.sh"
_E2E = _SCRIPTS / "recording-e2e.sh"

JXS_INPUTS = {"path": "jxs", "strih": "JXS cam5 src", "stream": "JXS PGM src", "stream_pin_fact_ms": 1054}
NDI_INPUTS = {"path": "ndi", "strih": "NDI cam5", "stream": "NDI 2ME PGM"}


def _clean():
    return json.loads((_FIX / "verdict_clean_pass.json").read_text(encoding="utf-8"))


def _delivery(cams):
    return {"overall_pass": True,
            "all_cambox_delivery_latency": {c: {"p50_ms": v} for c, v in cams.items()}}


# --- the full report ---------------------------------------------------------------------------


def test_the_header_names_the_measured_jpeg_xs_inputs_and_the_path():
    report = edr.compose_report(_clean(), {"run_id": "7", "inputs": JXS_INPUTS})
    assert "Merané vstupy (cesta JPEG XS): strih program 'JXS cam5 src', stream 'JXS PGM src'" in report


def test_the_header_names_the_ndi_inputs_on_the_ndi_path():
    report = edr.compose_report(_clean(), {"run_id": "7", "inputs": NDI_INPUTS})
    assert "Merané vstupy (cesta NDI): strih program 'NDI cam5', stream 'NDI 2ME PGM'" in report


def test_a_caller_that_passes_no_inputs_renders_exactly_as_before():
    plain = edr.compose_report(_clean(), {"run_id": "7"})
    assert "Merané vstupy" not in plain
    assert "Video sync NDI kamier v strih OBS" in plain
    # an empty inputs dict is the same as none
    assert edr.compose_report(_clean(), {"run_id": "7", "inputs": {}}) == plain


def test_the_video_sync_section_names_the_transport_the_cameras_arrived_on():
    jxs = edr.compose_report(_clean(), {"run_id": "7", "inputs": JXS_INPUTS})
    assert "Video sync JPEG XS kamier v strih OBS" in jxs and "Video sync NDI kamier" not in jxs
    ndi = edr.compose_report(_clean(), {"run_id": "7", "inputs": NDI_INPUTS})
    assert "Video sync NDI kamier v strih OBS" in ndi


def test_the_stream_hold_line_names_the_input_the_snapshot_read_and_grades_it_against_the_box_fact():
    pins = {"strih": {"cam5": {"main_ms": 3}}, "stream_hold_active_ms": 1054,
            "stream_hold_source": "JXS PGM src",
            "av_sync_last": {"applied_latency_ms": 987, "source": "NDI 2ME PGM"}}
    report = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": JXS_INPUTS})
    assert "Stream 'JXS PGM src' hold: živé z WS=1054ms, box fakt STRIH_JXS_PROGRAM_PIN_MS=1054ms" in report
    # the NDI source-of-truth file says nothing about the JPEG XS program pin: never compared to it
    assert "NDI 2ME PGM" not in report and "av-sync-last.json" not in report and "NEZHODA" not in report


def test_a_live_jpeg_xs_pin_off_its_fact_is_flagged():
    pins = {"strih": {"cam5": {"main_ms": 3}}, "stream_hold_active_ms": 987,
            "stream_hold_source": "JXS PGM src", "av_sync_last": {}}
    report = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": JXS_INPUTS})
    assert "živé z WS=987ms, box fakt STRIH_JXS_PROGRAM_PIN_MS=1054ms ⚠️ NEZHODA" in report


def test_an_old_snapshot_that_read_the_ndi_program_is_named_as_such_on_the_jpeg_xs_path():
    # a snapshot without stream_hold_source read 'NDI 2ME PGM' (the pre-1387 tool): say so, never
    # put the JPEG XS name on an NDI value
    pins = {"strih": {"cam5": {"main_ms": 3}}, "stream_hold_active_ms": 952,
            "av_sync_last": {"applied_latency_ms": 952, "source": "NDI 2ME PGM"}}
    report = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": JXS_INPUTS})
    assert "Stream 'NDI 2ME PGM' hold: živé z WS=952ms, posledný zdroj pravdy (av-sync-last.json)=952ms" in report
    assert "Stream 'JXS PGM src'" not in report


def test_the_ndi_path_keeps_the_av_sync_last_comparison():
    pins = {"strih": {"cam5": {"main_ms": 3}}, "stream_hold_active_ms": 925,
            "stream_hold_source": "NDI 2ME PGM",
            "av_sync_last": {"applied_latency_ms": 952, "source": "NDI 2ME PGM"}}
    report = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": NDI_INPUTS})
    assert "Stream 'NDI 2ME PGM' hold: živé z WS=925ms, posledný zdroj pravdy (av-sync-last.json)=952ms" in report
    assert "NEZHODA" in report and "STRIH_JXS_PROGRAM_PIN_MS" not in report


def test_the_per_camera_strih_pins_are_labelled_as_the_ndi_backup_on_the_jpeg_xs_path():
    # the snapshot reads the strih `NDI camN` inputs; on the JPEG XS path they are the backup, and the
    # JPEG XS camera pin is the one box fact -- the cell must not pass for the measured input's pin
    pins = {"strih": {"cam5": {"main_ms": 3}}}
    jxs = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": JXS_INPUTS})
    assert "cam5: strih NDI záloha=3ms" in jxs
    ndi = edr.compose_report(_delivery({"cam5": 40.0}), {"run_id": "7", "pins": pins, "inputs": NDI_INPUTS})
    assert "cam5: strih=3ms" in ndi


def test_the_phone_summary_pass_shape_never_changes():
    plain = edr.compose_summary(_clean(), {"run_id": "7"})
    assert edr.compose_summary(_clean(), {"run_id": "7", "inputs": JXS_INPUTS}) == plain
    assert len(plain.splitlines()) == 3


# --- the composer CLI ----------------------------------------------------------------------------


def _cli(tmp_path, *extra, pins=None):
    v = tmp_path / "verdict.json"
    v.write_text(json.dumps(_delivery({"cam5": 40.0})), encoding="utf-8")
    args = [sys.executable, str(_SCRIPTS / "e2e_discord_report.py"), "--json", str(v), "--run-id", "7"]
    if pins is not None:
        p = tmp_path / "pins.json"
        p.write_text(json.dumps(pins), encoding="utf-8")
        args += ["--pins-json", str(p)]
    return subprocess.run(args + list(extra), capture_output=True, text=True, timeout=60)


def test_the_cli_takes_the_resolved_inputs(tmp_path):
    pins = {"stream_hold_active_ms": 1054, "stream_hold_source": "JXS PGM src"}
    r = _cli(tmp_path, "--program-path", "jxs", "--strih-input", "JXS cam5 src",
             "--stream-input", "JXS PGM src", "--stream-pin-fact", "1054", pins=pins)
    assert r.returncode == 0, r.stderr
    assert "Merané vstupy (cesta JPEG XS): strih program 'JXS cam5 src', stream 'JXS PGM src'" in r.stdout
    assert "box fakt STRIH_JXS_PROGRAM_PIN_MS=1054ms" in r.stdout


def test_a_fact_that_is_not_a_number_is_left_out(tmp_path):
    # the PP stream box reads `none`; a box with no fact file reads empty
    for fact in ("none", "", "12a"):
        r = _cli(tmp_path, "--program-path", "jxs", "--stream-input", "JXS PGM src", "--stream-pin-fact", fact,
                 pins={"stream_hold_active_ms": 1054, "stream_hold_source": "JXS PGM src"})
        assert r.returncode == 0, r.stderr
        assert "STRIH_JXS_PROGRAM_PIN_MS" not in r.stdout


# --- the pins snapshot reads the measured stream input ---------------------------------------------


class _WS:
    def close(self):
        pass


def test_the_snapshot_reads_the_stream_input_it_is_given_and_records_it(monkeypatch, tmp_path):
    read = []
    monkeypatch.setattr(lps, "_conn", lambda host, password: _WS())
    monkeypatch.setattr(lps, "read_pin", lambda ws, name: read.append(name) or 1054)
    monkeypatch.setattr(lps, "av_sync_last_path", lambda: tmp_path / "absent.json")
    v = tmp_path / "v.json"
    v.write_text("{}", encoding="utf-8")
    out = tmp_path / "pins.json"
    lps.main(["--stream-host", "192.0.2.4", "--stream-input", "JXS PGM src", "--verdict-json", str(v),
              "--out", str(out)])
    data = json.loads(out.read_text(encoding="utf-8"))
    assert read == ["JXS PGM src"]
    assert data["stream_hold_active_ms"] == 1054 and data["stream_hold_source"] == "JXS PGM src"


def test_the_snapshot_defaults_to_the_ndi_program_and_says_so(monkeypatch, tmp_path):
    read = []
    monkeypatch.setattr(lps, "_conn", lambda host, password: _WS())
    monkeypatch.setattr(lps, "read_pin", lambda ws, name: read.append(name) or 952)
    monkeypatch.setattr(lps, "av_sync_last_path", lambda: tmp_path / "absent.json")
    v = tmp_path / "v.json"
    v.write_text("{}", encoding="utf-8")
    out = tmp_path / "pins.json"
    # recording-e2e passes "${STREAM_PROG_SOURCE:-}": an empty value is the NDI default
    for extra in ([], ["--stream-input", ""]):
        read.clear()
        lps.main(["--stream-host", "192.0.2.4", "--verdict-json", str(v), "--out", str(out)] + extra)
        assert read == ["NDI 2ME PGM"]
        assert json.loads(out.read_text(encoding="utf-8"))["stream_hold_source"] == "NDI 2ME PGM"


# --- the sender forwards what recording-e2e.sh resolved ------------------------------------------


def _send(tmp_path, *args):
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    log = tmp_path / "python.log"
    (bindir / "python3").write_text('#!/usr/bin/env bash\nfor a in "$@"; do printf \'%s\\n\' "$a"; done '
                                    f'>> "{log}"\nprintf \'["stub report"]\'\n')
    (bindir / "curl").write_text("#!/usr/bin/env bash\nprintf '{\"id\":\"1\"}\\n200'\n")
    for f in ("python3", "curl"):
        os.chmod(bindir / f, 0o755)
    verdict = tmp_path / "verdict.json"
    verdict.write_text("{}", encoding="utf-8")
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "HOME": str(tmp_path),
           "DISCORD_BOT_TOKEN": "fake-token-of-the-test", "DISCORD_NOTIFICATION_CHANNEL_ZBYNEK": "1",
           "DISCORD_MENTION_ZBYNEK": "2"}
    r = subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{_SENDER}"\ne2e_discord_report_send "$@"\necho RC=$?',
                        "harness", str(verdict), "77", "0", "300", "", "", "", *args],
                       capture_output=True, text=True, env=env, timeout=60)
    return r, (log.read_text().splitlines() if log.exists() else [])


def test_the_sender_forwards_the_resolved_inputs_to_the_composer(tmp_path):
    r, argv = _send(tmp_path, "jxs", "JXS cam5 src", "JXS PGM src", "1054")
    assert "RC=0" in r.stdout, r.stderr
    for flag, value in (("--program-path", "jxs"), ("--strih-input", "JXS cam5 src"),
                        ("--stream-input", "JXS PGM src"), ("--stream-pin-fact", "1054")):
        assert argv[argv.index(flag) + 1] == value, argv


def test_the_sender_forwards_nothing_it_was_not_given_and_never_a_bad_path(tmp_path):
    r, argv = _send(tmp_path)
    assert "RC=0" in r.stdout and "--program-path" not in argv and "--stream-input" not in argv, argv
    (tmp_path / "python.log").unlink()
    # a path the composer would refuse (argparse choices) must never cost the whole report
    r, argv = _send(tmp_path, "bogus", "", "JXS PGM src", "none")
    assert "RC=0" in r.stdout and argv, r.stderr
    assert "--program-path" not in argv and "--strih-input" not in argv
    assert argv[argv.index("--stream-input") + 1] == "JXS PGM src"


# --- the wiring in recording-e2e.sh ---------------------------------------------------------------


def test_recording_e2e_hands_the_resolved_inputs_to_the_report_and_the_pins_snapshot():
    s = _E2E.read_text(encoding="utf-8")
    assert s.count('e2e_discord_report_send "$REPORT_JSON"') == 1
    assert ('e2e_discord_report_send "$REPORT_JSON" "$RUN_ID" "$GATE" "$DURATION" "$PINS_JSON" "$MV_SKEW_JSON" '
            '"$GENLOCK_AUDIT_JSON" "${E2E_PROGRAM_PATH:-ndi}" "${STRIH_PROG_SOURCE:-}" "${STREAM_PROG_SOURCE:-}" '
            '"${PROGRAM_INPUTS_STREAM_PIN_MS:-}"') in s
    snap = s[s.index('python3 "$HERE/latency_pins_snapshot.py"'):]
    snap = snap[:snap.index("then")]
    assert '--stream-input "${STREAM_PROG_SOURCE:-}"' in snap
