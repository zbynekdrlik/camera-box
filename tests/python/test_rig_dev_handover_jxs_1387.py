"""issue 1387 slice 3 (design 6558387590): the development handover check grades the JPEG XS path.

On the SNV venue the on-air camera inputs are `JXS camN src` on strih and the program arrives as
`JXS PGM src` on stream. The handover check had no JPEG XS item, so an input the operator re-picked to
another camera, a pin written over WS, a stopped program output or a dead camera link (the 1.11.2026
event morning: the moved cameras came back without JPEG XS) all read "nič si nezabudol".

The item `jxs` reuses two EXISTING read-only probes, never a new one:
  * per broadcast box, `obs_jxs_inputs.py --verify` against the box FACTS' render (verify-strih item 49
    and the E2E [4i/8jxs] run the same call; scripts/lib/program-inputs.sh renders the manifest on dev1,
    which has no /opt/camera-box): every declared input present, of the kind, with the declared
    settings; on the strih also the running program output. A FAIL row or a camera moved off its
    declared input (the (jxs-pick) NOTE) = FORGOT-BY-OWNER naming the input; an unreachable websocket
    = UNKNOWN; a box that declares no JPEG XS = OK (N/A).
  * the links' liveness through the EVENT contract's bounded facet (scripts/lib/event-jxs-facet.sh: the
    last 1 MiB of the newest OBS log at nice 19) graded by event_assert.py's jxs_live_ok /
    jxs_live_detail: a link with no frames or a stale window = FORGOT naming the link; an unread box =
    UNKNOWN. Only the boxes that declare JPEG XS are read.
Report-only: nothing is ever written.

Tier-0: pytest + the bash lib / orchestrator with fakes, no rig (the SNV hosts resolve the committed
fact files; the verifier and the facet reader are fakes).
"""
import json
import os
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_dev_handover_decision as d  # noqa: E402

LIB = _SCRIPTS / "lib" / "program-inputs.sh"
ORCH = _SCRIPTS / "rig-dev-handover-check.sh"
SNV_STRIH, SNV_STREAM = "10.77.9.202", "10.77.9.204"
NOWHERE = "192.0.2.10"

_SCRUB = ("E2E_PROGRAM_PATH", "CAMERA_STRIH_JXS_SET", "STRIH_PROG_SOURCE", "STREAM_PROG_SOURCE",
          "STRIH_BOXES_DIR", "OBS_FLEET", "PROGRAM_INPUTS_VERIFY_TIMEOUT_S", "PROGRAM_INPUTS_OBS_PORT",
          "OBS_PASSWORD", "STRIH_HOST", "STREAM_HOST", "STRIH_LX_USER", "STRIH_LX_PW", "STREAM_USER",
          "STREAM_PW", "EVENT_JXS_SSH_TIMEOUT")


def _env(extra=None):
    e = {k: v for k, v in os.environ.items() if k not in _SCRUB}
    e.update(extra or {})
    return e


def _item():
    return next(i for i in d.ITEMS if i.key == "jxs")


def _decide(strih, stream, live):
    return _item().decide({"jxs_strih": (strih, 0), "jxs_stream": (stream, 0), "jxs_live": (live, 0)})


def _src(t, name, frames):
    return (f"{t}: [distroav] jxs-source '{name}': fps=30.00 frames={frames} lost_slices=0 concealed=0 "
            f"undecodable=0 reopens=0 decoder_reopens=0 bpp=8.00 (issue 1387)")


def _out(t, frames):
    return (f"{t}: [distroav] jxs-output 'camera-box JPEG XS program': fps=30.02 frames={frames} dropped_busy=0 "
            f"offered={frames} (issue 1387)")


STRIH_ROWS = ("PASS (jxs-inputs) 'JXS cam2 src': UDP 7002, camera 2, pin 36 ms\n"
              "NOTE (jxs-place) 'JXS cam2 src' is shown in: Cam 2 (report-only, the scenes are the operator's)\n"
              "PASS (jxs-inputs) 'JXS cam5 src': UDP 7000, camera 5, pin 36 ms\n"
              "PASS (jxs-output-live) 'camera-box JPEG XS program' running: the program to 10.77.9.204:7100 "
              "(stream id 100)\n")
STREAM_ROWS = "PASS (jxs-inputs) 'JXS PGM src': UDP 7100, camera 100, pin 1054 ms\n"
LIVE_OK = json.dumps({
    "strih": {"head": "13:30:10.559", "lines": [_src("13:30:05.000", "JXS cam2 src", 150),
                                                _src("13:30:05.100", "JXS cam5 src", 150),
                                                _out("13:30:06.000", 150)]},
    "stream": {"head": "13:30:09.000", "lines": [_src("13:30:05.000", "JXS PGM src", 150)]},
})
NONE_ROW = "NONE (jxs) strih 192.0.2.10: no fact file names this box -- it declares no JPEG XS input\n"


# --- the decision ----------------------------------------------------------------------------------


def test_the_jxs_item_is_in_the_checklist_and_reads_three_captures():
    assert _item().captures == ["jxs_strih", "jxs_stream", "jxs_live"]


def test_every_declared_input_on_its_settings_and_every_link_delivering_is_ok():
    e = _decide(STRIH_ROWS, STREAM_ROWS, LIVE_OK)
    assert e["status"] == d.OK, e


def test_a_venue_that_declares_no_jpeg_xs_is_ok_not_applicable():
    e = _decide(NONE_ROW, NONE_ROW.replace("strih", "stream"),
                "NONE (jxs-live) no broadcast box declares a JPEG XS link\n")
    assert e["status"] == d.OK and "NDI" in e["message"], e


def test_a_fail_row_is_forgot_by_owner_naming_the_input():
    stream = ("FAIL (jxs-inputs) 'JXS PGM src': genlock_latency_ms_src 987 != 1054 -- the launch seed heals it "
              "at the next OBS start, or obs_jxs_inputs.py --bootstrap now (issue 1387)\n")
    e = _decide(STRIH_ROWS, stream, LIVE_OK)
    assert e["status"] == d.FORGOT, e
    assert "stream 'JXS PGM src': genlock_latency_ms_src 987 != 1054" in e["message"]
    assert "launch seed" not in e["message"], "the English remedy tail stays in the capture"


def test_a_stopped_program_output_is_forgot_by_owner():
    strih = STRIH_ROWS.replace(
        "PASS (jxs-output-live) 'camera-box JPEG XS program' running: the program to 10.77.9.204:7100 (stream id 100)",
        "FAIL (jxs-output-live) 'camera-box JPEG XS program' is not running -- the program is not on the JPEG XS "
        "link; restart OBS (it starts from jxs-output.json) or StartOutput (issue 1387)")
    e = _decide(strih, STREAM_ROWS, LIVE_OK)
    assert e["status"] == d.FORGOT and "'camera-box JPEG XS program' is not running" in e["message"], e


def test_a_camera_moved_off_its_declared_input_is_forgot_with_the_rig_mode_fix():
    strih = STRIH_ROWS + ("NOTE (jxs-pick) 'JXS cam5 src' shows CAM3 (UDP 7003), the operator's pick in its "
                          "Kamera list (its declared camera is CAM5 (UDP 7000)) -- rig-mode TEST / EVENT puts the "
                          "declared camera back (report-only, issue 1387)\n")
    e = _decide(strih, STREAM_ROWS, LIVE_OK)
    assert e["status"] == d.FORGOT, e
    assert "strih 'JXS cam5 src' shows CAM3 (UDP 7003)" in e["message"]
    assert "rig-mode.sh test" in e["message"]


def test_the_place_note_is_report_only():
    e = _decide(STRIH_ROWS, STREAM_ROWS + "NOTE (jxs-place) 'JXS PGM src' is shown in: no scene\n", LIVE_OK)
    assert e["status"] == d.OK, e


def test_an_unreachable_websocket_is_unknown_never_forgot():
    stream = "NOTE (jxs-inputs) OBS WebSocket 10.77.9.204:4455 not reachable (refused) -- OBS not running?\n"
    e = _decide(STRIH_ROWS, stream, LIVE_OK)
    assert e["status"] == d.UNKNOWN and "stream" in e["message"], e


def test_an_error_row_an_empty_capture_or_a_missing_probe_is_unknown():
    for bad in ("ERROR (jxs) stream stream-snv: the box facts do not render a JPEG XS manifest\n", "",
                "MISSING: /x/obs_jxs_inputs.py\n", "NOTE (jxs-place) 'JXS PGM src' is shown in: PRO\n"):
        assert _decide(STRIH_ROWS, bad, LIVE_OK)["status"] == d.UNKNOWN, bad


def test_a_forgot_input_wins_over_an_unknown_box():
    stream = "NOTE (jxs-inputs) OBS WebSocket 10.77.9.204:4455 not reachable (refused)\n"
    strih = STRIH_ROWS + "FAIL (jxs-inputs) 'JXS cam2 src' is missing -- strih-obs-start.sh creates it\n"
    assert _decide(strih, stream, LIVE_OK)["status"] == d.FORGOT


def test_a_dead_link_is_forgot_by_owner_naming_the_link():
    live = json.loads(LIVE_OK)
    live["strih"]["lines"][1] = _src("13:30:05.100", "JXS cam5 src", 0)
    e = _decide(STRIH_ROWS, STREAM_ROWS, json.dumps(live))
    assert e["status"] == d.FORGOT, e
    assert "strih: 'JXS cam5 src' 0 snimok" in e["message"]


def test_a_stale_window_is_forgot_by_owner():
    live = json.loads(LIVE_OK)
    live["stream"]["head"] = "13:31:09.000"  # the program link's newest window is 64 s old
    e = _decide(STRIH_ROWS, STREAM_ROWS, json.dumps(live))
    assert e["status"] == d.FORGOT and "'JXS PGM src' bez okna" in e["message"], e


def test_an_unread_box_or_a_malformed_facet_is_unknown():
    live = json.loads(LIVE_OK)
    live["stream"] = None
    assert _decide(STRIH_ROWS, STREAM_ROWS, json.dumps(live))["status"] == d.UNKNOWN
    for bad in ("", "MISSING: lib\n", "not json", "null", "{}", "[]"):
        assert _decide(STRIH_ROWS, STREAM_ROWS, bad)["status"] == d.UNKNOWN, bad


def test_the_owner_summary_lists_the_item():
    stream = "FAIL (jxs-inputs) 'JXS PGM src' is missing -- strih-obs-start.sh creates it\n"
    entries = [_decide(STRIH_ROWS, stream, LIVE_OK)]
    _lines, summary, code = d.build_checklist(entries)
    assert code == 1 and summary.startswith("zabudol si: ") and _item().label in summary


# --- the lib: one box's raw rows against its facts ---------------------------------------------------


def _fake_seeder(tmp_path, rows_by_host, rc=0):
    here = tmp_path / "here"
    here.mkdir(exist_ok=True)
    log = tmp_path / "seeder.log"
    (here / "obs_jxs_inputs.py").write_text(f"""import json, sys
open({str(log)!r}, "a").write(json.dumps(sys.argv[1:]) + "\\n")
host = sys.argv[sys.argv.index("--host") + 1]
print(json.loads({json.dumps(json.dumps(rows_by_host))}).get(host, ""), end="")
sys.exit({rc})
""")
    return here, log


def _rows(tmp_path, here, host, role, env=None):
    out = tmp_path / "out"
    out.mkdir(exist_ok=True)
    r = subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{LIB}"\nprogram_inputs_jxs_box_rows "$@"\necho RC=$?',
                        "harness", str(here), host, role, str(out)],
                       capture_output=True, text=True, env=_env(env), timeout=120)
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    return r.stdout.replace("RC=0\n", "")


def test_the_lib_runs_the_verifier_against_the_facts_render_with_the_program_output_on_the_strih(tmp_path):
    here, log = _fake_seeder(tmp_path, {SNV_STRIH: STRIH_ROWS, SNV_STREAM: STREAM_ROWS})
    assert _rows(tmp_path, here, SNV_STRIH, "strih") == STRIH_ROWS
    assert _rows(tmp_path, here, SNV_STREAM, "stream") == STREAM_ROWS
    strih_call, stream_call = [json.loads(line) for line in log.read_text().splitlines()]
    assert "--verify" in strih_call and strih_call[strih_call.index("--host") + 1] == SNV_STRIH
    assert strih_call[strih_call.index("--output-target") + 1] == "10.77.9.204:7100"
    manifest = json.loads(pathlib.Path(strih_call[strih_call.index("--manifest") + 1]).read_text())
    assert [i["name"] for i in manifest["inputs"]] == ["JXS cam2 src", "JXS cam3 src", "JXS cam5 src"]
    assert "--output-target" not in stream_call and "--bootstrap" not in stream_call


def test_a_box_no_fact_file_names_is_one_none_row_and_never_dialled(tmp_path):
    here, log = _fake_seeder(tmp_path, {})
    out = _rows(tmp_path, here, NOWHERE, "strih")
    assert out.startswith("NONE (jxs) strih 192.0.2.10:") and len(out.splitlines()) == 1, out
    assert not log.exists()


def test_a_box_whose_facts_declare_no_jpeg_xs_is_one_none_row(tmp_path):
    # the fact loader checks the fleet row against the box's own STRIH_IP: dial it by that address
    facts = (_SCRIPTS / "strih-boxes" / "strih-pp.env").read_text(encoding="utf-8").splitlines()
    pp_ip = next(ln.split("=", 1)[1].strip() for ln in facts if ln.startswith("STRIH_IP="))
    here, log = _fake_seeder(tmp_path, {})
    out = _rows(tmp_path, here, pp_ip, "strih", env={"OBS_FLEET": f"strih-pp|{pp_ip}|linux-genlock|always"})
    assert out.startswith(f"NONE (jxs) strih {pp_ip}:") and "strih-pp" in out, out
    assert not log.exists()


def test_a_verifier_that_fails_or_prints_nothing_is_one_error_row(tmp_path):
    here, _log = _fake_seeder(tmp_path, {SNV_STREAM: STREAM_ROWS}, rc=1)
    out = _rows(tmp_path, here, SNV_STREAM, "stream")
    assert out.startswith("ERROR (jxs) ") and "obs_jxs_inputs.py --verify failed" in out, out
    second = tmp_path / "second"
    second.mkdir()
    here2, _ = _fake_seeder(second, {})
    out = _rows(second, here2, SNV_STREAM, "stream")
    assert out.startswith("ERROR (jxs) ") and "printed nothing" in out, out


# --- the orchestrator ---------------------------------------------------------------------------------


def _fake_live_lib(tmp_path, facet):
    log = tmp_path / "live.log"
    lib = tmp_path / "event-jxs-facet.sh"
    lib.write_text("event_jxs_windows_json() {\n"
                   f'  printf \'%s\\n\' "$*" >> "{log}"\n'
                   f"  printf '%s\\n' {json.dumps(json.dumps(facet))}\n"
                   "}\n")
    return lib, log


def _orchestrate(tmp_path, seeder, live_lib, extra=None):
    missing = str(tmp_path / "missing")
    env = {"PATH": os.environ["PATH"], "HOME": str(tmp_path), "TMPDIR": str(tmp_path), "RDH_TIMEOUT": "20",
           "RDH_IMAG_RETIRED": "1", "STRIH_HOST": SNV_STRIH, "STREAM_HOST": SNV_STREAM,
           "RDH_JXS_PROBE": str(seeder), "RDH_JXS_LIVE_LIB": str(live_lib)}
    for item in ("MIC", "MIC_GUARD", "PAINTER", "CLOCK", "OBS", "NET", "AUDIOLAG", "GENLOCK", "BURN", "MAPPING",
                 "PINS", "DANTESYNC", "CAMBOX", "AVLATENCY", "EXPOSURE", "RIGMODE_LIB", "WATCHDOG_ROSTER",
                 "CAMSET_LIB"):
        key = f"RDH_{item}" if item.endswith("_LIB") or item == "WATCHDOG_ROSTER" else f"RDH_{item}_PROBE"
        env[key] = missing
    env.update(extra or {})
    r = subprocess.run(["bash", str(ORCH), "--json"], capture_output=True, text=True, env=_env(env), timeout=180)
    out = json.loads(r.stdout)
    return next(e for e in out["items"] if e["key"] == "jxs"), r


def test_the_orchestrator_grades_both_boxes_and_reads_the_links_of_the_declaring_boxes(tmp_path):
    stream = "FAIL (jxs-inputs) 'JXS PGM src' is missing -- strih-obs-start.sh creates it\n"
    here, log = _fake_seeder(tmp_path, {SNV_STRIH: STRIH_ROWS, SNV_STREAM: stream})
    live = json.loads(LIVE_OK)
    live["strih"]["lines"][0] = _src("13:30:05.000", "JXS cam2 src", 0)
    lib, live_log = _fake_live_lib(tmp_path, live)
    entry, r = _orchestrate(tmp_path, here / "obs_jxs_inputs.py", lib)
    assert entry["status"] == d.FORGOT, (entry, r.stderr)
    assert "stream 'JXS PGM src' is missing" in entry["message"]
    assert "strih: 'JXS cam2 src' 0 snimok" in entry["message"]
    assert len(log.read_text().splitlines()) == 2
    # one facet read for both declaring boxes, read-only (user, password, name, host per box)
    (call,) = live_log.read_text().splitlines()
    assert call.split()[2:4] == ["strih", SNV_STRIH] and call.split()[6:8] == ["stream", SNV_STREAM]
    assert r.returncode == 1


def test_the_orchestrator_never_dials_a_box_that_declares_no_jpeg_xs(tmp_path):
    here, log = _fake_seeder(tmp_path, {})
    lib, live_log = _fake_live_lib(tmp_path, {})
    entry, r = _orchestrate(tmp_path, here / "obs_jxs_inputs.py", lib,
                            {"STRIH_HOST": NOWHERE, "STREAM_HOST": "192.0.2.11"})
    assert entry["status"] == d.OK, (entry, r.stderr)
    assert not log.exists() and not live_log.exists()


def test_a_missing_probe_is_unknown_and_reads_no_link(tmp_path):
    lib, live_log = _fake_live_lib(tmp_path, json.loads(LIVE_OK))
    entry, r = _orchestrate(tmp_path, tmp_path / "missing", lib)
    assert entry["status"] == d.UNKNOWN, (entry, r.stderr)
    assert not live_log.exists()


def test_the_orchestrator_bounds_the_verifier_and_the_facet_read():
    s = ORCH.read_text(encoding="utf-8")
    body = s[s.index("probe_jxs_box() {"):s.index('python3 "$DECIDE"')]
    assert 'PROGRAM_INPUTS_VERIFY_TIMEOUT_S="$(_probe_timeout jxs)"' in body
    assert 'EVENT_JXS_SSH_TIMEOUT="$(_probe_timeout jxs_live)"' in body
    assert "--bootstrap" not in body and "--enforce-mapping" not in body
