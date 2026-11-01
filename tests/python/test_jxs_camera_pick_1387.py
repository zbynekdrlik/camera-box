"""issue 1387 (owner 1.11.2026, design comment 6558038959 Approach 1): the operator picks which camera a
JPEG XS input shows ("Kamera" list in jxs-source.cpp), like the NDI source's list.

  * scripts/obs_jxs_inputs.py --bootstrap keeps an input's jxs_port + jxs_camera PAIR when it names a camera
    the manifest declares (the operator's pick); an undeclared pair and every other key are still healed;
  * --verify reads such a pick as a NOTE (verify-strih item 49), never a FAIL;
  * --enforce-mapping puts the declared 1:1 pair back on every declared input and reads it back, never on
    air; rig-mode TEST and EVENT run it next to the strih NDI mapping (the issue-399 parity) through
    scripts/lib/jxs-mapping-enforce.sh, recorded and folded into the exit like the neighbouring steps.

Tier-0: a fake obs-websocket `rpc`, a fake seeder for the lib, the issue-1407 harness for the mode bodies.
"""
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_obs_jxs_inputs_1387 import CAM2, DEFAULTS, FakeObs, _live_cam2, _manifest, oji  # noqa: E402
from test_rig_mode_relay_rc_1407 import _at, _run  # noqa: E402

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "jxs-mapping-enforce.sh"
RIG = REPO / "scripts" / "rig-mode.sh"

CAM3 = dict(CAM2, name="JXS cam3 src", scene="JXS cam3",
            settings=dict(CAM2["settings"], jxs_port=7003, jxs_camera=3))
CAM5 = dict(CAM2, name="JXS cam5 src", scene="JXS cam5",
            settings=dict(CAM2["settings"], jxs_port=7000, jxs_camera=5))
SPECS = oji.parse_manifest(_manifest(CAM2, CAM3, CAM5))


def _obs(**cams):
    """A strih with the three declared inputs; cams overrides one input's explicit settings."""
    base = {"JXS cam2 src": _live_cam2(), "JXS cam3 src": _live_cam2(jxs_port=7003, jxs_camera=3),
            "JXS cam5 src": _live_cam2(jxs_port=7000, jxs_camera=5)}
    for name, over in cams.items():
        base[name.replace("_", " ")] = _live_cam2(**over)
    return FakeObs({n: (oji.INPUT_KIND, st) for n, st in base.items()},
                   {"Cam 2": [("JXS cam2 src", True)], "Cam 3": [("JXS cam3 src", True)]})


def _pair(obs, name):
    st = {**DEFAULTS, **obs.inputs[name][1]}
    return st["jxs_port"], st["jxs_camera"]


# --- --bootstrap: the operator's pick is kept ------------------------------------------------------------

def test_bootstrap_keeps_a_pick_of_another_declared_camera_and_still_heals_the_other_keys():
    # JXS cam2 src shows cam3 (picked in its Kamera list) and its pin drifted to 3
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3, "genlock_latency_ms_src": 3})
    lines, ok = oji.bootstrap(obs.rpc, SPECS)
    assert ok
    assert obs.writes() == [("SetInputSettings", {"inputName": "JXS cam2 src", "overlay": True,
                                                  "inputSettings": {"genlock_latency_ms_src": 36}})]
    assert _pair(obs, "JXS cam2 src") == (7003, 3)
    assert "healed 'JXS cam2 src': genlock_latency_ms_src 3 -> 36" in lines[0]
    assert "operator's pick" in lines[0] and "CAM3 (UDP 7003)" in lines[0], lines[0]


def test_bootstrap_keeps_a_pick_that_needs_no_other_write_as_a_pure_read_even_on_air():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3})
    lines, ok = oji.bootstrap(obs.rpc, SPECS, live=True)
    assert ok and obs.writes() == []
    assert "matches its declared settings" in lines[0] and "CAM3 (UDP 7003)" in lines[0], lines[0]


def test_bootstrap_keeps_a_swap_of_two_cameras():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3}, JXS_cam3_src={"jxs_port": 7002, "jxs_camera": 2})
    _lines, ok = oji.bootstrap(obs.rpc, SPECS)
    assert ok and obs.writes() == []


def test_bootstrap_heals_an_undeclared_pair_back_to_the_declared_one():
    # 7099 is no declared port: both numbers go back
    obs = _obs(JXS_cam2_src={"jxs_port": 7099, "jxs_camera": 3})
    lines, ok = oji.bootstrap(obs.rpc, SPECS)
    assert ok
    assert obs.writes() == [("SetInputSettings", {"inputName": "JXS cam2 src", "overlay": True,
                                                  "inputSettings": {"jxs_port": 7002, "jxs_camera": 2}})]
    assert "operator's pick" not in lines[0]
    # a MIXED pair (cam3's port, cam2's id) is no declared camera: the differing number is healed
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 2})
    oji.bootstrap(obs.rpc, SPECS)
    assert obs.writes() == [("SetInputSettings", {"inputName": "JXS cam2 src", "overlay": True,
                                                  "inputSettings": {"jxs_port": 7002}})]


def test_a_pair_read_as_a_json_float_is_still_the_pick():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003.0, "jxs_camera": 3.0})
    _lines, ok = oji.bootstrap(obs.rpc, SPECS)
    assert ok and obs.writes() == []


def test_plan_input_without_the_manifest_keeps_its_old_contract():
    spec = SPECS[0]
    eff = {**DEFAULTS, **_live_cam2(jxs_port=7003, jxs_camera=3)}
    assert oji.plan_input(spec, {spec.name: oji.INPUT_KIND}, eff)[0] == "update"
    assert oji.plan_input(spec, {spec.name: oji.INPUT_KIND}, eff, SPECS) == ("ok", None)
    assert oji.picked_camera(spec, SPECS, eff).name == "JXS cam3 src"
    assert oji.picked_camera(spec, SPECS, {**DEFAULTS, **_live_cam2()}) is None


# --- --verify: a pick is a NOTE ---------------------------------------------------------------------------

def test_verify_reads_a_pick_as_a_note_never_a_fail():
    obs = _obs(JXS_cam2_src={"jxs_port": 7000, "jxs_camera": 5})
    rows = oji.verify_rows(obs.rpc, SPECS)
    assert [lvl for lvl, _ in rows if lvl == "FAIL"] == []
    picks = [t for lvl, t in rows if lvl == "NOTE" and t.startswith("(jxs-pick)")]
    assert len(picks) == 1, rows
    assert "'JXS cam2 src' shows CAM5 (UDP 7000)" in picks[0] and "CAM2 (UDP 7002)" in picks[0], picks[0]
    # a second input on the same camera stays black: the note names it
    assert "'JXS cam5 src' listens on UDP 7000 too" in picks[0], picks[0]
    passes = [t for lvl, t in rows if lvl == "PASS" and t.startswith("(jxs-inputs)")]
    assert len(passes) == 3 and "port 7000 camera 5" in passes[0], passes
    assert obs.writes() == []


def test_verify_a_pick_with_a_drifted_pin_fails_only_the_pin():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3, "genlock_latency_ms_src": 3})
    rows = oji.verify_rows(obs.rpc, SPECS)
    fails = [t for lvl, t in rows if lvl == "FAIL"]
    assert len(fails) == 1 and "genlock_latency_ms_src 3 != 36" in fails[0], rows
    assert "jxs_port" not in fails[0] and "jxs_camera" not in fails[0]
    assert any(lvl == "NOTE" and t.startswith("(jxs-pick) 'JXS cam2 src' shows CAM3") for lvl, t in rows), rows


def test_verify_an_undeclared_pair_still_fails():
    obs = _obs(JXS_cam2_src={"jxs_port": 7099, "jxs_camera": 3})
    fails = [t for lvl, t in oji.verify_rows(obs.rpc, SPECS) if lvl == "FAIL"]
    assert len(fails) == 1 and "jxs_port 7099 != 7002" in fails[0] and "jxs_camera 3 != 2" in fails[0], fails


# --- --enforce-mapping -------------------------------------------------------------------------------------

def test_enforce_mapping_puts_the_declared_pair_back_and_reads_it_back():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3, "genlock_latency_ms_src": 3},
               JXS_cam3_src={"jxs_port": 7002, "jxs_camera": 2})
    lines, ok = oji.enforce_mapping(obs.rpc, SPECS)
    assert ok, lines
    assert obs.writes() == [
        ("SetInputSettings", {"inputName": "JXS cam2 src", "overlay": True,
                              "inputSettings": {"jxs_port": 7002, "jxs_camera": 2}}),
        ("SetInputSettings", {"inputName": "JXS cam3 src", "overlay": True,
                              "inputSettings": {"jxs_port": 7003, "jxs_camera": 3}}),
    ]
    assert [_pair(obs, n) for n in ("JXS cam2 src", "JXS cam3 src", "JXS cam5 src")] == [(7002, 2), (7003, 3), (7000, 5)]
    # only the mapping: the drifted pin is the launch seed's business, never written here
    assert obs.inputs["JXS cam2 src"][1]["genlock_latency_ms_src"] == 3
    assert "put 'JXS cam2 src' back on CAM2 (UDP 7002) (it showed CAM3 (UDP 7003))" in lines[0], lines
    assert "read back" in lines[0], lines[0]
    assert "'JXS cam5 src' already on CAM5 (UDP 7000)" in lines[2], lines
    # the read-back is a GET after the write
    sets = [i for i, r in enumerate(obs.log) if r[0] == "SetInputSettings"]
    assert any(r[0] == "GetInputSettings" and r[1]["inputName"] == "JXS cam2 src" for r in obs.log[sets[0] + 1:])


def test_enforce_mapping_on_a_clean_strih_is_a_pure_read():
    obs = _obs()
    lines, ok = oji.enforce_mapping(obs.rpc, SPECS)
    assert ok and obs.writes() == [] and len(lines) == 3


def test_enforce_mapping_never_writes_on_air():
    for kw in ({"streaming": True}, {"recording": True}, {"streaming": None}):
        obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3})
        obs.streaming = kw.get("streaming", False)
        obs.recording = kw.get("recording", False)
        lines, ok = oji.enforce_mapping(obs.rpc, SPECS, live=oji.obs_live(obs.rpc))
        assert not ok and obs.writes() == [], kw
        assert lines[0].startswith("FAIL jxs mapping: REFUSED to put 'JXS cam2 src' back"), lines
    # nothing to write: no refusal, even on air
    obs = _obs()
    _lines, ok = oji.enforce_mapping(obs.rpc, SPECS, live=True)
    assert ok and obs.writes() == []


def test_enforce_mapping_fails_a_missing_input_or_another_kind_and_creates_nothing():
    obs = _obs()
    del obs.inputs["JXS cam5 src"]
    obs.inputs["JXS cam3 src"] = ("ndi_source", {"ndi_source_name": "CAM3 (usb)"})
    lines, ok = oji.enforce_mapping(obs.rpc, SPECS)
    assert not ok and obs.writes() == []
    assert any(ln.startswith("FAIL jxs mapping: 'JXS cam3 src' is a ndi_source") for ln in lines), lines
    assert any(ln.startswith("FAIL jxs mapping: 'JXS cam5 src' is missing") for ln in lines), lines


def test_enforce_mapping_fails_when_the_read_back_disagrees():
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3})
    real = obs.rpc

    def deaf(req, data=None, ignore_err=False):
        if req == "SetInputSettings":
            obs.log.append((req, data))
            return {}
        return real(req, data, ignore_err)

    lines, ok = oji.enforce_mapping(deaf, SPECS)
    assert not ok
    assert lines[0].startswith("FAIL jxs mapping: 'JXS cam2 src' reads CAM3 (UDP 7003) after the write"), lines


def test_cli_enforce_mapping_exits_3_when_the_mapping_is_not_proven(tmp_path, monkeypatch, capsys):
    # rig-mode folds the seeder's rc into the mode's exit: a refused or failed mapping must not read 0
    m = tmp_path / "jxs-seed.json"
    m.write_text(_manifest(CAM2, CAM3, CAM5))
    obs = _obs(JXS_cam2_src={"jxs_port": 7003, "jxs_camera": 3})
    obs.recording = True
    client = type("Client", (), {"req": staticmethod(obs.rpc)})
    monkeypatch.setattr(oji, "_client", lambda args: client())
    assert oji.main(["--enforce-mapping", "--manifest", str(m)]) == 3
    assert "REFUSED" in capsys.readouterr().out and obs.writes() == []
    obs.recording = False
    assert oji.main(["--enforce-mapping", "--manifest", str(m)]) == 0
    assert _pair(obs, "JXS cam2 src") == (7002, 2)


def test_cli_enforce_mapping_exit_codes(tmp_path, capsys):
    m = tmp_path / "jxs-seed.json"
    assert oji.main(["--enforce-mapping", "--manifest", str(m)]) == 0
    assert "declares no JPEG XS input" in capsys.readouterr().out
    m.write_text("{}")
    assert oji.main(["--enforce-mapping", "--manifest", str(m)]) == 2
    m.write_text(_manifest(CAM2))
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = str(s.getsockname()[1])
    s.close()
    assert oji.main(["--enforce-mapping", "--manifest", str(m), "--port", port]) == 1


# --- the rig-mode lib --------------------------------------------------------------------------------------

def _fake_seeder(tmp_path, rc):
    log = tmp_path / "seeder.log"
    fake = tmp_path / "fake_seeder.py"
    fake.write_text(f"""import sys
args = sys.argv[1:]
with open({str(log)!r}, "a") as fh:
    fh.write(" ".join(args) + "\\n")
    fh.write(open(args[args.index("--manifest") + 1]).read())
print("FAIL jxs mapping: a line of the seeder" if {rc} else "jxs mapping: 'JXS cam2 src' already on CAM2 (UDP 7002)")
sys.exit({rc})
""")
    return fake, log


def _lib(tmp_path, ip, rc=0, mode="event"):
    """Run the lib function under the caller's set -euo pipefail with the record-and-fold call shape."""
    fake, log = _fake_seeder(tmp_path, rc)
    script = tmp_path / "drive.sh"
    script.write_text(f"""set -euo pipefail
. "{LIB}"
_r=0
jxs_mapping_enforce {mode} "$1" "" || _r=$?
echo "AFTER rc=$_r"
""")
    env = dict(os.environ, JXS_MAPPING_ENFORCE_SEEDER=str(fake), HOME=str(tmp_path))
    proc = subprocess.run(["bash", str(script), ip], capture_output=True, text=True, env=env, timeout=60)
    return proc, (log.read_text() if log.exists() else "")


def _snv_manifest():
    out = subprocess.run(
        ["bash", "-c", f'set -euo pipefail; . "{REPO}/scripts/lib/strih-box-facts.sh"; '
                       f'. "{REPO}/scripts/lib/strih-jxs-provision.sh"; strih_box_load strih-snv; '
                       f'strih_jxs_seed_manifest_json'], capture_output=True, text=True, check=True)
    return out.stdout


def test_the_lib_runs_the_seeder_against_the_strih_with_the_facts_render(tmp_path):
    proc, log = _lib(tmp_path, "10.77.9.202")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "AFTER rc=0" in proc.stdout
    args = log.splitlines()[0].split()
    assert args[0] == "--enforce-mapping"
    assert args[args.index("--host") + 1] == "10.77.9.202"
    assert "--password" in args
    assert log.split("\n", 1)[1] == _snv_manifest()
    assert "    [strih jxs-map] jxs mapping: 'JXS cam2 src' already on CAM2" in proc.stdout, proc.stdout
    assert "WARNING" not in proc.stderr


def test_a_failed_enforce_returns_its_rc_and_warns_by_name_and_the_caller_continues(tmp_path):
    proc, _log = _lib(tmp_path, "10.77.9.202", rc=3, mode="test")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "AFTER rc=3" in proc.stdout
    warn = [ln for ln in proc.stderr.splitlines() if ln.startswith("WARNING issue 1387")]
    assert len(warn) == 1 and "rc=3" in warn[0] and "TEST mode will still exit non-zero" in warn[0], proc.stderr


def test_a_strih_that_declares_no_jpeg_xs_camera_or_no_fact_file_is_a_note(tmp_path):
    # strih-pp declares none; stream-snv is no strih (its JXS PGM src is never enforced from here); no box at
    # the TEST-NET address
    for ip in ("10.77.8.202", "10.77.9.204", "192.0.2.9"):
        proc, log = _lib(tmp_path, ip)
        assert proc.returncode == 0 and "AFTER rc=0" in proc.stdout, proc.stdout + proc.stderr
        assert log == ""
        assert "NOTE" in proc.stdout and "issue 1387" in proc.stdout, proc.stdout


def test_the_lib_has_no_side_effect_when_sourced():
    proc = subprocess.run(["bash", "-c", f'set -euo pipefail; . "{LIB}"; declare -F | wc -l'],
                          capture_output=True, text=True, check=True)
    assert proc.stdout.strip() != "0" and proc.stderr == ""


# --- rig-mode.sh: the call sites ---------------------------------------------------------------------------

def _body(s, name):
    start = s.index(f"\n{name}() {{\n")
    return s[start:s.index("\n}\n", start)]


def test_rig_mode_sources_the_lib_once_and_calls_it_next_to_the_ndi_mapping_in_both_modes():
    s = RIG.read_text()
    assert s.count('. "$RIG_MODE_DIR/lib/jxs-mapping-enforce.sh"') == 1
    assert s.index('. "$RIG_MODE_DIR/lib/jxs-mapping-enforce.sh"') < s.index('\nif [ "${BASH_SOURCE[0]}" = "${0}" ]; then')
    for mode in ("test", "event"):
        body = _body(s, f"do_{mode}")
        call = f'jxs_mapping_enforce {mode} "$STRIH_IP" "$OBS_WS_PASSWORD" || _jxsmap_rc=$?'
        assert body.count(call) == 1, mode
        ndi = body.index("enforce_strih_ndi_mapping")
        assert ndi < body.index(call), mode
        assert "local _jxsmap_rc=0" in body
    # the call LINES (a comment near the top of do_test names the imag step too)
    test_body = _body(s, "do_test")
    assert test_body.index("jxs_mapping_enforce test") < test_body.index("\n  set_imag_test_program\n")
    event_body = _body(s, "do_event")
    assert event_body.index("jxs_mapping_enforce event") < event_body.index("\n  print_genlock_relaunch_note event\n")


def test_event_with_a_failed_jxs_mapping_runs_every_step_then_exits_nonzero_naming_it(tmp_path):
    fail = 'jxs_mapping_enforce() { _stub_step jxs_mapping_enforce "$@"; return 3; }'
    proc, ran, _boxes, sent = _run(tmp_path, "event", extra=fail)
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    order = [_at(ran, s) for s in ("toggle_burn event", "enforce_strih_ndi_mapping", "jxs_mapping_enforce event",
                                   "print_genlock_relaunch_note event", "event_mode_assert",
                                   "event_mode_discord_confirm_send")]
    assert order == sorted(order), "\n".join(ran)
    results = [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:")]
    assert any("strih JPEG XS camera mapping enforce FAILED (issue 1387, rc=3)" in ln for ln in results), proc.stderr
    assert not any("CONTRACT CONFIRMED" in ln for ln in proc.stdout.splitlines()), out
    top = sent.splitlines()[0]
    assert top.startswith("⚠️") and "JPEG XS" in top, sent


def test_event_and_test_pass_the_strih_address_to_the_step(tmp_path):
    proc, ran, _boxes, _sent = _run(tmp_path, "event")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    step = ran[_at(ran, "jxs_mapping_enforce")]
    assert step.split()[1:3] == ["event", "10.77.9.202"], step


def test_test_with_a_failed_jxs_mapping_runs_every_step_then_exits_nonzero(tmp_path):
    fail = 'jxs_mapping_enforce() { _stub_step jxs_mapping_enforce "$@"; return 3; }'
    proc, ran, _boxes, _sent = _run(tmp_path, "test", enabled=True, extra=fail)
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    order = [_at(ran, s) for s in ("enforce_strih_ndi_mapping", "jxs_mapping_enforce test", "set_imag_test_program",
                                   "verify_measurement_audio_arrives", "park_stream_program_dev",
                                   "print_genlock_relaunch_note test")]
    assert order == sorted(order), "\n".join(ran)
    assert "WHOLE CHAIN verified" not in out, out
    assert any(ln.startswith("RESULT:") and "JPEG XS camera mapping enforce FAILED (issue 1387, rc=3)" in ln
               for ln in proc.stderr.splitlines()), proc.stderr
