"""issue 1387 slice 1b (design 6558387590): the full-path E2E, rig-mode TEST and the A/V soak measure the
JPEG XS path when the box facts declare it.

Covers scripts/lib/program-inputs.sh (the resolver, the [4i/8jxs] step, the rendered-input check, the
#856 recommendation, the burn unit's JPEG XS target), camera-set.sh's camera_strih_input and the four
CSV helpers that follow it, the one naming rule moved into strih-box-facts.sh, frozen-camera-gate.py's
scene map, the mv-reverify cure gates, and the wiring in recording-e2e.sh / rig-mode.sh / av-soak.sh.
Every lib call runs under the caller's `set -euo pipefail` with fakes on PATH or in a fake HERE dir;
no rig, no network (the hosts that do not name a fact file are TEST-NET addresses).
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "program-inputs.sh"
CAMSET = SCRIPTS / "camera-set.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-jxs-provision.sh"
MVLIB = SCRIPTS / "lib" / "mv-reverify-escalate.sh"
CPULIB = SCRIPTS / "lib" / "cpu-affinity-burn.sh"
E2E = SCRIPTS / "recording-e2e.sh"
RIG = SCRIPTS / "rig-mode.sh"
SOAK = SCRIPTS / "av-soak.sh"
BOXES = SCRIPTS / "strih-boxes"

SNV_STRIH, SNV_STREAM = "10.77.9.202", "10.77.9.204"
NOWHERE_A, NOWHERE_B = "192.0.2.10", "192.0.2.11"

# the harness never inherits a resolved state or an override from the runner's environment
_SCRUB = ("E2E_PROGRAM_PATH", "CAMERA_STRIH_JXS_SET", "STRIH_PROG_SOURCE", "STREAM_PROG_SOURCE",
          "STRIH_PROG_SCENE", "STREAM_PROG_SCENE", "CAMERA_ACTIVE_SET", "CAMERA_SOURCE_BOX",
          "CAMERA_ALIGN_SET", "STRIH_BOXES_DIR", "OBS_FLEET", "STRIH_LX_IP", "MEASUREMENT_EQ",
          "COLD_CUT_BYPASS_CAM", "PRERECORD_PHASE_CALIBRATE", "ALL_CAMBOX", "PREFLIGHT_EXCLUDED_CAMS",
          "PROGRAM_INPUTS_STREAM_PIN_MS", "PROGRAM_INPUTS_STRIH_BOX", "PROGRAM_INPUTS_STREAM_BOX",
          "AV_SYNC_APPLY_OFFSET_MS", "AV_SYNC_HELD_REASON", "OUTDIR", "RUN_ID", "STRIH_IP", "STREAM_IP",
          "OBS_PASSWORD", "AV_SOAK_STRIH_INPUT_FMT")


def _env(extra=None):
    e = {k: v for k, v in os.environ.items() if k not in _SCRUB}
    e.update(extra or {})
    return e


def _bash(body, *args, env=None, libs=(CAMSET, LIB), strict="set -euo pipefail"):
    """Run BODY after sourcing LIBS under the caller's strict mode; ARGS are $1.. (never spliced)."""
    src = "".join(f'. "{p}"\n' for p in libs)
    return subprocess.run(["bash", "-c", f"{strict}\n{src}{body}", "harness", *args],
                          capture_output=True, text=True, env=_env(env), timeout=180)


def _fact(box, key, boxes_dir=None):
    env = {"STRIH_BOXES_DIR": str(boxes_dir)} if boxes_dir else None
    r = _bash('strih_box_load "$1" >/dev/null; strih_box_fact "$2"', box, key, env=env, libs=(FACTS,))
    assert r.returncode == 0, r.stderr
    return r.stdout


RESOLVED = ('printf "path=%s\\nset=%s\\nstrih=%s\\nstream=%s\\npin=%s\\nboxes=%s/%s\\n" "$E2E_PROGRAM_PATH" '
            '"$CAMERA_STRIH_JXS_SET" "${STRIH_PROG_SOURCE:-<unset>}" "${STREAM_PROG_SOURCE:-<unset>}" '
            '"$PROGRAM_INPUTS_STREAM_PIN_MS" "$PROGRAM_INPUTS_STRIH_BOX" "$PROGRAM_INPUTS_STREAM_BOX"')


def _resolved(stdout):
    return dict(line.split("=", 1) for line in stdout.splitlines() if "=" in line)


def _snv_jxs_cams():
    return " ".join(f"cam{e.split(':')[0]}" for e in _fact("strih-snv", "STRIH_JXS_CAMERAS").split())


# ---------------------------------------------------------------------------------------------------
# the resolver


def test_the_snv_venue_resolves_to_the_jpeg_xs_path_from_the_facts():
    r = _bash(f'program_inputs_resolve "$1" "$2" cam5\n{RESOLVED}', SNV_STRIH, SNV_STREAM)
    assert r.returncode == 0, r.stderr
    got = _resolved(r.stdout)
    assert got["path"] == "jxs"
    assert got["set"] == _snv_jxs_cams() == "cam2 cam3 cam5"
    assert got["strih"] == "JXS cam5 src"
    assert got["stream"] == "JXS PGM src"
    assert got["pin"] == _fact("stream-snv", "STRIH_JXS_PROGRAM_PIN_MS")
    assert got["boxes"] == "strih-snv/stream-snv"
    assert "[program-inputs] issue 1387: the jxs path" in r.stderr


def test_the_pp_venue_stays_on_the_ndi_path_and_presets_nothing():
    r = _bash(f'program_inputs_resolve "$1" "$2" cam1\n{RESOLVED}', "strih-pp.lan", "stream-pp.lan")
    assert r.returncode == 0, r.stderr
    got = _resolved(r.stdout)
    assert got["path"] == "ndi" and got["set"] == ""
    assert got["strih"] == "<unset>" and got["stream"] == "<unset>", "the NDI defaults stay the caller's own"
    assert got["boxes"] == "strih-pp/stream-pp"


def test_hosts_no_fact_file_names_read_ndi():
    r = _bash(f'program_inputs_resolve "$1" "$2" cam5\n{RESOLVED}', NOWHERE_A, NOWHERE_B)
    assert r.returncode == 0, r.stderr
    got = _resolved(r.stdout)
    assert got["path"] == "ndi" and got["set"] == "" and got["boxes"] == "/"


def test_program_path_ndi_is_the_default_when_nothing_resolved():
    r = _bash('program_path_ndi && echo NDI')
    assert r.returncode == 0 and r.stdout.strip() == "NDI"


def _boxes_copy(tmp_path, edits):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        shutil.copy(f, d / f.name)
    for name, key, value in edits:
        p = d / name
        lines = [f"{key}={value}" if ln.startswith(key + "=") else ln for ln in p.read_text().splitlines()]
        p.write_text("\n".join(lines) + "\n")
    return d


def test_a_venue_with_ndi_cameras_beside_jpeg_xs_ones_refuses_naming_them(tmp_path):
    d = _boxes_copy(tmp_path, [("strih-snv.env", "STRIH_JXS_CAMERAS", "2:7002 3:7003")])
    r = _bash('program_inputs_resolve "$1" "$2" cam5 || echo "RC=$?"\n echo "PATH=${E2E_PROGRAM_PATH:-unset}"',
              SNV_STRIH, SNV_STREAM, env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert "RC=1" in r.stdout and "PATH=unset" in r.stdout, "a refusal sets nothing"
    assert "MIXED venue" in r.stderr and "cam2 cam3 over JPEG XS but cam5 over NDI" in r.stderr


def test_a_jpeg_xs_strih_with_a_stream_that_receives_no_jpeg_xs_program_refuses(tmp_path):
    d = _boxes_copy(tmp_path, [("stream-snv.env", "STRIH_JXS_PROGRAM_PORT", "none"),
                               ("stream-snv.env", "STRIH_JXS_PROGRAM_PIN_MS", "none")])
    r = _bash('program_inputs_resolve "$1" "$2" cam5 || echo "RC=$?"', SNV_STRIH, SNV_STREAM,
              env={"STRIH_BOXES_DIR": str(d)})
    assert "RC=1" in r.stdout, r.stderr
    assert "declares no JPEG XS program" in r.stderr and "MIXED venue" in r.stderr


def test_a_jpeg_xs_stream_with_an_ndi_strih_refuses(tmp_path):
    d = _boxes_copy(tmp_path, [("strih-snv.env", "STRIH_JXS_CAMERAS", "none"),
                               ("strih-snv.env", "STRIH_JXS_CAMERA_PIN_MS", "none")])
    r = _bash('program_inputs_resolve "$1" "$2" cam5 || echo "RC=$?"', SNV_STRIH, SNV_STREAM,
              env={"STRIH_BOXES_DIR": str(d)})
    assert "RC=1" in r.stdout, r.stderr
    assert "declares no JPEG XS camera" in r.stderr


def test_an_e2e_program_path_the_facts_contradict_refuses_and_a_matching_one_passes():
    r = _bash('program_inputs_resolve "$1" "$2" cam5 || echo "RC=$?"', SNV_STRIH, SNV_STREAM,
              env={"E2E_PROGRAM_PATH": "ndi"})
    assert "RC=1" in r.stdout and "E2E_PROGRAM_PATH=ndi but the box facts say jxs" in r.stderr
    r = _bash(f'program_inputs_resolve "$1" "$2" cam5\n{RESOLVED}', SNV_STRIH, SNV_STREAM,
              env={"E2E_PROGRAM_PATH": "jxs"})
    assert r.returncode == 0 and _resolved(r.stdout)["path"] == "jxs", r.stderr


def test_an_env_override_of_the_measured_inputs_wins():
    r = _bash(f'program_inputs_resolve "$1" "$2" cam5\n{RESOLVED}', SNV_STRIH, SNV_STREAM,
              env={"STRIH_PROG_SOURCE": "my strih", "STREAM_PROG_SOURCE": "my stream",
                   "CAMERA_STRIH_JXS_SET": "cam3"})
    got = _resolved(r.stdout)
    assert (got["strih"], got["stream"], got["set"]) == ("my strih", "my stream", "cam3")


def test_an_empty_source_camera_leaves_the_strih_input_to_the_caller():
    r = _bash(f'program_inputs_resolve "$1" "$2" ""\n{RESOLVED}', SNV_STRIH, SNV_STREAM)
    got = _resolved(r.stdout)
    assert got["strih"] == "<unset>" and got["stream"] == "JXS PGM src"


def test_the_ndi_only_opt_ins_refuse_on_jpeg_xs_and_pass_on_ndi():
    for var, val in (("MEASUREMENT_EQ", "1"), ("PRERECORD_PHASE_CALIBRATE", "1"), ("COLD_CUT_BYPASS_CAM", "CAM5")):
        r = _bash('program_inputs_resolve "$1" "$2" cam5 && program_inputs_refuse_ndi_only_optins || echo "RC=$?"',
                  SNV_STRIH, SNV_STREAM, env={var: val})
        assert "RC=1" in r.stdout, (var, r.stderr)
        assert f"{var}=" in r.stderr and "NDI-only measurement opt-in on the JPEG XS path" in r.stderr
        r = _bash('program_inputs_resolve "$1" "$2" cam1 && program_inputs_refuse_ndi_only_optins && echo OK',
                  "strih-pp.lan", "stream-pp.lan", env={var: val})
        assert r.stdout.strip() == "OK", (var, r.stderr)


def test_a_fact_box_is_never_lost_to_sigpipe_under_pipefail(tmp_path):
    """The fleet-name -> fact-file lookup runs under the caller's pipefail. A `lister | grep -q` there
    read a matched box as missing whenever grep's early exit SIGPIPEd the lister (a flake seen in a
    parallel run). More fact-file names than a pipe holds make that SIGPIPE certain."""
    d = _boxes_copy(tmp_path, [])
    for i in range(8000):
        (d / f"zz-pad-{i:05d}.env").write_text("")
    r = _bash(f'program_inputs_resolve "$1" "$2" cam5\n{RESOLVED}', SNV_STRIH, SNV_STREAM,
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert _resolved(r.stdout)["path"] == "jxs" and _resolved(r.stdout)["boxes"] == "strih-snv/stream-snv"


# ---------------------------------------------------------------------------------------------------
# camera-set.sh: camera_strih_input + the four CSV helpers; the one naming rule


def test_camera_strih_input_is_ndi_unless_the_camera_is_a_jpeg_xs_camera():
    r = _bash('for c in cam5 cam1; do printf "%s|" "$(camera_strih_input "$c")"; done; '
              'CAMERA_STRIH_JXS_SET="cam2 cam3 cam5"; '
              'for c in cam5 cam1 cam2; do printf "%s|" "$(camera_strih_input "$c")"; done', libs=(CAMSET,))
    assert r.stdout == "NDI cam5|NDI cam1|JXS cam5 src|NDI cam1|JXS cam2 src|", r.stderr


def test_the_four_csv_helpers_follow_the_measured_input_and_stay_ndi_without_a_set():
    body = ('printf "%s\\n" "$(camera_active_ndi_sources_csv)" "$(camera_align_ndi_sources_csv)" '
            '"$(camera_align_ndi_sources_excluding_csv cam3)" "$(camera_active_ndi_sources_excluding_csv cam2)"')
    ndi = _bash(body, libs=(CAMSET,), env={"CAMERA_ACTIVE_SET": "cam2 cam3 cam5"})
    assert ndi.stdout.splitlines() == ["NDI cam2,NDI cam3,NDI cam5", "NDI cam3,NDI cam5", "NDI cam5",
                                       "NDI cam3,NDI cam5"], ndi.stderr
    jxs = _bash(body, libs=(CAMSET,), env={"CAMERA_ACTIVE_SET": "cam2 cam3 cam5",
                                           "CAMERA_STRIH_JXS_SET": "cam2 cam3 cam5"})
    assert jxs.stdout.splitlines() == ["JXS cam2 src,JXS cam3 src,JXS cam5 src", "JXS cam3 src,JXS cam5 src",
                                       "JXS cam5 src", "JXS cam3 src,JXS cam5 src"], jxs.stderr


def test_the_naming_rule_lives_in_the_facts_loader_and_the_manifest_still_uses_it():
    assert "strih_jxs_input_name() {" in FACTS.read_text()
    assert "strih_jxs_input_name() {" not in PROVISION.read_text()
    r = _bash('strih_jxs_input_name 5; strih_jxs_input_name PGM', libs=(FACTS,))
    assert r.stdout.splitlines() == ["JXS cam5 src", "JXS PGM src"]
    r = _bash('strih_box_load strih-snv >/dev/null; strih_jxs_seed_manifest_json', libs=(FACTS, PROVISION))
    names = [i["name"] for i in json.loads(r.stdout)["inputs"]]
    assert names == [f"JXS {c} src" for c in _snv_jxs_cams().split()], r.stderr


# ---------------------------------------------------------------------------------------------------
# the burn unit's JPEG XS target (blocker 2)


def _fake_systemctl(tmp_path, env_line):
    b = tmp_path / "bin"
    b.mkdir(exist_ok=True)
    (b / "systemctl").write_text(f"#!/bin/bash\nprintf '%s\\n' {json.dumps(env_line)}\n")
    (b / "systemctl").chmod(0o755)
    (b / "hostname").write_text("#!/bin/bash\necho cam5\n")
    (b / "hostname").chmod(0o755)
    return b


def _burn_setenv(tmp_path, env_line):
    b = _fake_systemctl(tmp_path, env_line)
    return _bash('eval "$(cambox_jxs_burn_setenv_resolve_cmd)"; printf "[%s]" "$CAMBOX_JXS_BURN_SETENV"',
                 env={"PATH": f"{b}:{os.environ['PATH']}"})


def test_the_burn_unit_takes_the_cameras_loaded_jpeg_xs_target(tmp_path):
    r = _burn_setenv(tmp_path, "NDI_CONFIG_DIR=/etc/ndi CAMERA_BOX_JXS_TARGET=10.77.9.202:7000")
    assert r.stdout.endswith("[--setenv=CAMERA_BOX_JXS_TARGET=10.77.9.202:7000]"), r.stderr
    assert "sends JPEG XS to 10.77.9.202:7000" in r.stdout


def test_a_camera_with_no_target_sends_ndi_only(tmp_path):
    r = _burn_setenv(tmp_path, "NDI_CONFIG_DIR=/etc/ndi")
    assert r.stdout.endswith("[]") and "sends NDI only" in r.stdout, r.stderr


def test_a_target_that_is_not_ipv4_port_is_refused(tmp_path):
    for bad in ("10.77.9.202", "10.0.0.1:7000;touch /tmp/x", "host.lan:7000", ":7000", "1.2.3.4:"):
        r = _burn_setenv(tmp_path, f"CAMERA_BOX_JXS_TARGET={bad}")
        assert r.stdout.endswith("[]"), (bad, r.stdout)
        assert "WARNING: [jxs-burn]" in r.stderr, bad


def test_the_emitted_text_never_glues_the_next_command_onto_the_unit_line(tmp_path):
    """The e2e embeds the emitter mid-string through $(...) (which strips the trailing newline): the
    command that follows must still run as its own statement and the unit gets the token as ONE arg."""
    b = _fake_systemctl(tmp_path, "CAMERA_BOX_JXS_TARGET=10.77.9.202:7003")
    marker = tmp_path / "burn.log"
    marker.write_text("old")
    (b / "systemd-run").write_text("#!/bin/bash\nfor a in \"$@\"; do printf '<%s>\\n' \"$a\"; done\n")
    (b / "systemd-run").chmod(0o755)
    body = ('remote="$(cpu_affinity_burn_resolve_cmd) \\\n   $(cambox_jxs_burn_setenv_resolve_cmd) \\\n'
            '   rm -f $1; \\\n   systemd-run --unit=x --collect \\\n     \\$CPU_AFFINITY_BURN_PROPERTY \\\n'
            '     \\$CAMBOX_JXS_BURN_SETENV \\\n     --setenv=CAMERA_BOX_BURN_RUN_ID=911006 /tmp/bin"\n'
            'bash -c "$remote"')
    r = _bash(body, str(marker), libs=(CAMSET, LIB, CPULIB), env={"PATH": f"{b}:{os.environ['PATH']}"})
    assert r.returncode == 0, r.stderr
    assert not marker.exists(), "the rm after the emitter ran as its own command"
    assert "<--setenv=CAMERA_BOX_JXS_TARGET=10.77.9.202:7003>" in r.stdout
    assert "<--setenv=CAMERA_BOX_BURN_RUN_ID=911006>" in r.stdout


# ---------------------------------------------------------------------------------------------------
# [4i/8jxs] (blocking) + the report-only one-frame measure


def _fake_here(tmp_path, rows_by_host, measure=None):
    here = tmp_path / "here"
    here.mkdir()
    log = tmp_path / "calls.log"
    (here / "obs_jxs_inputs.py").write_text(f"""import json, sys
open({str(log)!r}, "a").write("obs_jxs_inputs " + json.dumps(sys.argv[1:]) + "\\n")
host = sys.argv[sys.argv.index("--host") + 1]
rows = json.loads({json.dumps(json.dumps(rows_by_host))})
print(rows.get(host, "FAIL (jxs-inputs) unknown host"))
""")
    (here / "qr_align_pins.py").write_text(f"""import json, sys
open({str(log)!r}, "a").write("qr_align_pins " + json.dumps(sys.argv[1:]) + "\\n")
print({json.dumps(json.dumps(measure if measure is not None else {}))})
""")
    return here, log


GOOD_STRIH = "PASS (jxs-inputs) 'JXS cam5 src': ok\nNOTE (jxs-place) 'JXS cam5 src' is shown in: Cam 5\nPASS (jxs-output-live) running"
GOOD_STREAM = "PASS (jxs-inputs) 'JXS PGM src': ok"


def _preflight(tmp_path, rows, measure=None, env=None):
    here, log = _fake_here(tmp_path, rows, measure)
    out = tmp_path / "out"
    out.mkdir()
    e = {"ALL_CAMBOX": "1", "CAMERA_ACTIVE_SET": "cam2 cam3 cam5"}
    e.update(env or {})
    r = _bash('program_inputs_resolve "$1" "$2" cam5\nprogram_inputs_jxs_preflight "$3" "$1" "$2" "$4" 77 || echo "RC=$?"',
              SNV_STRIH, SNV_STREAM, str(here), str(out), env=e)
    calls = log.read_text().splitlines() if log.exists() else []
    return r, calls, out


def test_the_jpeg_xs_step_grades_both_boxes_against_the_facts_and_measures_report_only(tmp_path):
    measure = {"JXS cam2 src": 0, "JXS cam3 src": 0, "JXS cam5 src": 0, "spread_frames": 0,
               "rounds_used": 5, "converged": True}
    r, calls, out = _preflight(tmp_path, {SNV_STRIH: GOOD_STRIH, SNV_STREAM: GOOD_STREAM}, measure)
    assert "RC=" not in r.stdout, r.stdout + r.stderr
    strih_call = json.loads(calls[0].split(" ", 1)[1])
    stream_call = json.loads(calls[1].split(" ", 1)[1])
    target = f"{_fact('stream-snv', 'STRIH_IP')}:{_fact('stream-snv', 'STRIH_JXS_PROGRAM_PORT')}"
    assert strih_call[strih_call.index("--output-target") + 1] == target
    assert strih_call[strih_call.index("--output-stream-id") + 1] == "100"
    assert "--output-target" not in stream_call
    manifest = json.loads(Path(strih_call[strih_call.index("--manifest") + 1]).read_text())
    assert [i["name"] for i in manifest["inputs"]] == ["JXS cam2 src", "JXS cam3 src", "JXS cam5 src"]
    m = json.loads(calls[2].split(" ", 1)[1])
    assert m[m.index("--sources") + 1] == "JXS cam2 src,JXS cam3 src,JXS cam5 src" and "--measure-only" in m
    assert json.loads((out / "jxs-measure-77.json").read_text())["spread_frames"] == 0
    assert "(b) REPORT-ONLY one painter frame on every camera" in r.stdout and ": COMMON " in r.stdout


def test_a_spread_measure_is_reported_never_an_abort(tmp_path):
    measure = {"JXS cam2 src": 0, "JXS cam3 src": -2, "spread_frames": 2, "rounds_used": 6, "converged": True}
    r, _calls, _out = _preflight(tmp_path, {SNV_STRIH: GOOD_STRIH, SNV_STREAM: GOOD_STREAM}, measure)
    assert "RC=" not in r.stdout and ": SPREAD spread_frames=2" in r.stdout, r.stderr


def test_a_fail_row_aborts_before_the_measure(tmp_path):
    rows = {SNV_STRIH: GOOD_STRIH, SNV_STREAM: "FAIL (jxs-inputs) 'JXS PGM src': genlock_latency_ms_src 987 != 1054"}
    r, calls, _out = _preflight(tmp_path, rows)
    assert "RC=1" in r.stdout and "[4i/8jxs] FAIL" in r.stderr and "987 != 1054" in r.stderr
    assert not any(c.startswith("qr_align_pins") for c in calls)


def test_a_camera_moved_off_its_declared_input_aborts(tmp_path):
    rows = {SNV_STRIH: GOOD_STRIH + "\nNOTE (jxs-pick) 'JXS cam5 src' shows CAM3 (UDP 7003)", SNV_STREAM: GOOD_STREAM}
    r, _calls, _out = _preflight(tmp_path, rows)
    assert "RC=1" in r.stdout and "(jxs-pick)" in r.stderr and "rig-mode.sh test puts the mapping back" in r.stderr


def test_an_unreachable_obs_aborts(tmp_path):
    rows = {SNV_STRIH: "NOTE (jxs-inputs) OBS WebSocket 10.77.9.202:4455 not reachable (refused)", SNV_STREAM: GOOD_STREAM}
    r, _calls, _out = _preflight(tmp_path, rows)
    assert "RC=1" in r.stdout and "needs the box's OBS answering" in r.stderr


def test_a_single_camera_run_skips_the_measure(tmp_path):
    r, calls, _out = _preflight(tmp_path, {SNV_STRIH: GOOD_STRIH, SNV_STREAM: GOOD_STREAM}, env={"ALL_CAMBOX": "0"})
    assert "RC=" not in r.stdout and "SKIPPED (single-camera run)" in r.stdout
    assert [c.split(" ")[0] for c in calls] == ["obs_jxs_inputs", "obs_jxs_inputs"]


def test_an_acked_offline_camera_is_left_out_of_the_measure(tmp_path):
    _r, calls, _out = _preflight(tmp_path, {SNV_STRIH: GOOD_STRIH, SNV_STREAM: GOOD_STREAM},
                                 env={"PREFLIGHT_EXCLUDED_CAMS": "cam3"})
    m = json.loads(calls[2].split(" ", 1)[1])
    assert m[m.index("--sources") + 1] == "JXS cam2 src,JXS cam5 src"


def test_the_ndi_path_runs_none_of_the_jpeg_xs_step(tmp_path):
    here, log = _fake_here(tmp_path, {})
    r = _bash('program_inputs_jxs_preflight "$1" a b "$2" 1; program_inputs_rendered_verify "$1" a b "Cam 5:CAM5"',
              str(here), str(tmp_path), env={"ALL_CAMBOX": "1"})
    assert r.returncode == 0 and "SKIPPED (the NDI path" in r.stdout and not log.exists(), r.stderr


def test_the_measure_verdict_vectors():
    cases = [
        ('{"a": 0, "b": 0, "spread_frames": 0, "rounds_used": 5, "converged": true}', "COMMON"),
        ('{"a": 0, "b": -1, "spread_frames": 1, "rounds_used": 5, "converged": true}', "SPREAD"),
        ('{"a": 0, "b": 0, "spread_frames": 0, "rounds_used": 2, "converged": false}', "UNKNOWN"),
        ('{"error": "no round", "spread_frames": null, "converged": false}', "UNKNOWN"),
        ('{"spread_frames": true, "converged": true}', "UNKNOWN"),
        ("", "UNKNOWN"), ("not json", "UNKNOWN"), ("[1, 2]", "UNKNOWN"),
    ]
    for text, want in cases:
        r = _bash('program_inputs_measure_verdict "$1"', text)
        assert r.returncode == 0 and r.stdout.split(" ", 1)[0] == want, (text, r.stdout, r.stderr)


# ---------------------------------------------------------------------------------------------------
# the rendered-input check before StartRecord


def _fake_render(tmp_path, table):
    here = tmp_path / "rhere"
    here.mkdir()
    log = tmp_path / "render.log"
    (here / "obs_phase2.py").write_text(f"""import json, sys
a = sys.argv[1:]
host, scene = a[a.index("--host") + 1], a[a.index("--scene") + 1]
open({str(log)!r}, "a").write(host + "|" + scene + "\\n")
t = json.loads({json.dumps(json.dumps(table))})
if host + "|" + scene not in t:
    sys.exit("no scene")
print(t[host + "|" + scene])
""")
    return here, log


RENDER_ENV = {"E2E_PROGRAM_PATH": "jxs", "CAMERA_STRIH_JXS_SET": "cam2 cam3 cam5", "STRIH_PROG_SCENE": "Cam 5",
              "STRIH_PROG_SOURCE": "JXS cam5 src", "STREAM_PROG_SCENE": "Development",
              "STREAM_PROG_SOURCE": "JXS PGM src", "ALL_CAMBOX": "1"}
GOOD_RENDER = {"s|Cam 5": "JXS cam5 src", "s|Cam 2": "JXS cam2 src", "s|Cam 3": "JXS cam3 src",
               "p|Development": "JXS PGM src"}


def test_every_measured_scene_renders_its_jpeg_xs_input(tmp_path):
    here, log = _fake_render(tmp_path, GOOD_RENDER)
    r = _bash('program_inputs_rendered_verify "$1" s p "Cam 5:CAM5 Cam 2:CAM2 Cam 3:CAM3" && echo OK', str(here),
              env=RENDER_ENV)
    assert r.stdout.rstrip().endswith("OK"), r.stderr
    assert log.read_text().splitlines() == ["s|Cam 5", "s|Cam 2", "s|Cam 3", "p|Development"]


def test_a_scene_rendering_another_input_aborts_and_every_scene_is_still_read(tmp_path):
    bad = dict(GOOD_RENDER, **{"s|Cam 3": "NDI cam3"})
    here, log = _fake_render(tmp_path, bad)
    r = _bash('program_inputs_rendered_verify "$1" s p "Cam 5:CAM5 Cam 2:CAM2 Cam 3:CAM3" || echo "RC=$?"', str(here),
              env=RENDER_ENV)
    assert "RC=1" in r.stdout
    assert "strih scene 'Cam 3' renders 'NDI cam3', not the measured input 'JXS cam3 src'" in r.stderr
    assert "p|Development" in log.read_text()


def test_an_unreadable_scene_aborts(tmp_path):
    here, _log = _fake_render(tmp_path, {k: v for k, v in GOOD_RENDER.items() if k != "p|Development"})
    r = _bash('program_inputs_rendered_verify "$1" s p "Cam 5:CAM5" || echo "RC=$?"', str(here), env=RENDER_ENV)
    assert "RC=1" in r.stdout and "'Development' renders '<unreadable>'" in r.stderr


def test_a_single_camera_run_checks_only_the_program_scenes(tmp_path):
    here, log = _fake_render(tmp_path, GOOD_RENDER)
    r = _bash('program_inputs_rendered_verify "$1" s p "Cam 5:CAM5 Cam 2:CAM2"', str(here),
              env=dict(RENDER_ENV, ALL_CAMBOX="0"))
    assert r.returncode == 0 and log.read_text().splitlines() == ["s|Cam 5", "p|Development"], r.stderr


# ---------------------------------------------------------------------------------------------------
# #856 on JPEG XS = a recommendation (the HOLD shape)


def _required_delay(pin, off):
    sys.path.insert(0, str(SCRIPTS))
    import av_sync_calibrate
    return av_sync_calibrate.required_delay_ms(pin, off)


def test_the_av_apply_on_jpeg_xs_prints_the_recommended_fact_and_holds(tmp_path):
    for off in ("20", "-35.5", "200"):
        out = tmp_path / off
        out.mkdir()
        r = _bash('program_inputs_av_pin_hold; printf "OFF=[%s]\\nWHY=%s\\n" "$AV_SYNC_APPLY_OFFSET_MS" "$AV_SYNC_HELD_REASON"',
                  env={"E2E_PROGRAM_PATH": "jxs", "PROGRAM_INPUTS_STREAM_PIN_MS": "1054",
                       "AV_SYNC_APPLY_OFFSET_MS": off, "OUTDIR": str(out), "RUN_ID": "9",
                       "PROGRAM_INPUTS_STREAM_BOX": "stream-snv"})
        want = _required_delay(1054, float(off))
        assert r.returncode == 0 and "OFF=[]" in r.stdout, r.stderr
        assert f"recommended STRIH_JXS_PROGRAM_PIN_MS={want}" in r.stdout
        assert f"RECOMMENDATION (a fact change" in r.stderr and f"STRIH_JXS_PROGRAM_PIN_MS={want}" in r.stderr
        assert (out / "av-sync-jxs-pin-recommendation-9.txt").read_text().splitlines()[0] == \
            f"STRIH_JXS_PROGRAM_PIN_MS={want}"


def _split(raw, pin, gain):
    sys.path.insert(0, str(SCRIPTS))
    import av_sync_calibrate
    return av_sync_calibrate.split_av_correction(raw, pin, 0.0, gain,
                                                 audio_gain=av_sync_calibrate.AUDIO_TRIM_LOOP_GAIN)


def test_a_sub_frame_residual_on_jpeg_xs_goes_to_the_audio_trim_and_keeps_the_pin_fact_1482(tmp_path):
    # issue 1482: the JPEG XS pin is frame-quantized (1054, 1060 and 1064 ms all hold 32 frames), so
    # a residual under one frame can only be corrected on the mbc audio offset: the apply runs, with
    # the fact pin exported so av_sync_calibrate.py never writes the pin.
    r = _bash('program_inputs_av_pin_hold; printf "OFF=[%s]\\nFACT=[%s]\\nWHY=[%s]\\n" '
              '"$AV_SYNC_APPLY_OFFSET_MS" "${AV_SYNC_PIN_FACT_MS:-}" "${AV_SYNC_HELD_REASON:-}"; '
              'env | grep "^AV_SYNC_PIN_FACT_MS=" || true',
              env={"E2E_PROGRAM_PATH": "jxs", "PROGRAM_INPUTS_STREAM_PIN_MS": "1064",
                   "AV_SYNC_APPLY_OFFSET_MS": "-5.664", "AV_SYNC_COMBINED_OFFSET_MS_RAW": "-14.16",
                   "AV_SYNC_LOOP_GAIN_VALUE": "0.4", "OUTDIR": str(tmp_path), "RUN_ID": "9",
                   "PROGRAM_INPUTS_STREAM_BOX": "stream-snv"})
    assert _split(-14.16, 1064, 0.4)[0] == 1064
    assert r.returncode == 0, r.stderr
    assert "OFF=[-5.664]" in r.stdout and "FACT=[1064]" in r.stdout and "WHY=[]" in r.stdout
    assert "AV_SYNC_PIN_FACT_MS=1064" in r.stdout  # exported to the apply's child process
    assert "mbc audio offset" in r.stderr
    assert not list(tmp_path.iterdir())


def test_a_whole_frame_residual_on_jpeg_xs_recommends_the_split_pin_fact_and_holds_1482(tmp_path):
    r = _bash('program_inputs_av_pin_hold; printf "OFF=[%s]\\nFACT=[%s]\\n" '
              '"$AV_SYNC_APPLY_OFFSET_MS" "${AV_SYNC_PIN_FACT_MS:-}"',
              env={"E2E_PROGRAM_PATH": "jxs", "PROGRAM_INPUTS_STREAM_PIN_MS": "1064",
                   "AV_SYNC_APPLY_OFFSET_MS": "-40", "AV_SYNC_COMBINED_OFFSET_MS_RAW": "-100",
                   "AV_SYNC_LOOP_GAIN_VALUE": "0.4", "OUTDIR": str(tmp_path), "RUN_ID": "9",
                   "PROGRAM_INPUTS_STREAM_BOX": "stream-snv"})
    want = _split(-100.0, 1064, 0.4)[0]
    assert want != 1064
    assert r.returncode == 0 and "OFF=[]" in r.stdout and "FACT=[]" in r.stdout, r.stderr
    assert f"STRIH_JXS_PROGRAM_PIN_MS={want}" in r.stderr
    assert (tmp_path / "av-sync-jxs-pin-recommendation-9.txt").read_text().splitlines()[0] == \
        f"STRIH_JXS_PROGRAM_PIN_MS={want}"


def test_the_av_apply_on_ndi_or_with_no_correction_is_untouched(tmp_path):
    r = _bash('program_inputs_av_pin_hold; printf "OFF=[%s]" "$AV_SYNC_APPLY_OFFSET_MS"',
              env={"AV_SYNC_APPLY_OFFSET_MS": "20", "OUTDIR": str(tmp_path), "RUN_ID": "9"})
    assert r.stdout == "OFF=[20]" and not list(tmp_path.iterdir()), r.stderr
    r = _bash('program_inputs_av_pin_hold; printf "OFF=[%s] WHY=[%s]" "$AV_SYNC_APPLY_OFFSET_MS" "${AV_SYNC_HELD_REASON:-}"',
              env={"E2E_PROGRAM_PATH": "jxs", "PROGRAM_INPUTS_STREAM_PIN_MS": "1054", "AV_SYNC_APPLY_OFFSET_MS": ""})
    assert r.stdout == "OFF=[] WHY=[]", r.stderr


# ---------------------------------------------------------------------------------------------------
# frozen-camera-gate.py + the e2e frozen-gate source list


def _gate_module():
    spec = importlib.util.spec_from_file_location("frozen_camera_gate_1387", SCRIPTS / "frozen-camera-gate.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def test_the_frozen_gate_warms_a_jpeg_xs_input_through_its_camera_scene():
    m = _gate_module()
    assert m._scene_for_input("JXS cam5 src") == "Cam 5"
    assert m._scene_for_input("NDI cam5") == "Cam 5"
    assert m._scene_for_input("JXS PGM src") is None
    assert m._scene_for_input("JXS cam5") is None


def _frozen_heredoc():
    s = E2E.read_text()
    start = s.index("FROZEN_CAM_SOURCES=\"$(python3 - ")
    body = s[s.index("<<'PYEOF'\n", start) + len("<<'PYEOF'\n"):]
    return body[:body.index("\nPYEOF\n")]


def _run_frozen(tmp_path, args, bindings):
    fake = tmp_path / "fake_obs_phase2.py"
    fake.write_text(f"""import json
B = json.loads({json.dumps(json.dumps(bindings))})
class _W:
    def close(self):
        pass
def _conn(host, pw):
    return _W()
def _rpc(ws, req, data):
    if data["inputName"] not in B:
        raise RuntimeError("no input")
    return {{"inputSettings": {{"ndi_source_name": B[data["inputName"]]}}}}
""")
    return subprocess.run([sys.executable, "-", "10.77.9.202", "CAM2 (usb)", str(fake), *args],
                          input=_frozen_heredoc(), capture_output=True, text=True, env=_env(), timeout=60)


def test_the_frozen_gate_list_takes_the_jpeg_xs_inputs_excluding_the_painter_by_camera_number(tmp_path):
    r = _run_frozen(tmp_path, ["cam2 cam3 cam5", "JXS cam2 src,JXS cam3 src,JXS cam5 src"], {})
    assert r.returncode == 0 and r.stdout.strip() == "JXS cam3 src,JXS cam5 src", r.stderr
    assert "excluding 'JXS cam2 src' (camera cam2 = 'CAM2 (usb)'" in r.stderr


def test_the_frozen_gate_list_on_ndi_is_unchanged(tmp_path):
    b = {"NDI cam2": "CAM2 (usb)", "NDI cam3": "CAM3 (usb)", "NDI cam5": "CAM5 (usb)"}
    for args in (["cam2 cam3 cam5", "NDI cam2,NDI cam3,NDI cam5"], ["cam2 cam3 cam5"]):
        r = _run_frozen(tmp_path, args, b)
        assert r.returncode == 0 and r.stdout.strip() == "NDI cam3,NDI cam5", (args, r.stderr)


# ---------------------------------------------------------------------------------------------------
# mv-reverify: detection on the measured input, the NDI cures skipped on a JPEG XS input


def _fake_py(tmp_path):
    b = tmp_path / "pybin"
    b.mkdir()
    log = tmp_path / "py.log"
    (b / "python3").write_text(f"#!/bin/bash\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n")
    (b / "python3").chmod(0o755)
    (b / "timeout").write_text("#!/bin/bash\nshift\nexec \"$@\"\n")
    (b / "timeout").chmod(0o755)
    return b, log


def test_mv_reverify_detects_on_the_measured_input_and_gates_the_ndi_cures():
    body = ('printf "%s|" "$(mv_reverify_input 5)"; mv_reverify_ndi_cures 5 && echo -n "cure|" || echo -n "nocure|"; '
            'CAMERA_STRIH_JXS_SET="cam2 cam3 cam5"; printf "%s|" "$(mv_reverify_input 5)"; '
            'mv_reverify_ndi_cures 5 && echo -n "cure" || echo -n "nocure"')
    r = _bash(body, libs=(CAMSET, LIB, MVLIB))
    assert r.stdout == "NDI cam5|cure|JXS cam5 src|nocure", r.stderr
    r = _bash('printf "%s" "$(mv_reverify_input 5)"', libs=(MVLIB,))
    assert r.stdout == "NDI cam5", "without camera-set.sh the old NDI name stays"


def test_the_proactive_reset_never_touches_a_jpeg_xs_input(tmp_path):
    b, log = _fake_py(tmp_path)
    env = {"ALL_CAMBOX": "1", "PATH": f"{b}:{os.environ['PATH']}", "HERE": str(SCRIPTS), "STRIH": "s",
           "PROBE_BIN_DIR": "/nonexistent"}
    r = _bash('mv_reverify_proactive_reset cam5 5 && echo DONE', libs=(CAMSET, LIB, MVLIB),
              env=dict(env, CAMERA_STRIH_JXS_SET="cam2 cam3 cam5"))
    assert r.stdout.strip() == "DONE" and not log.exists(), r.stderr
    r = _bash('mv_reverify_proactive_reset cam5 5 && echo DONE', libs=(CAMSET, LIB, MVLIB), env=env)
    assert r.stdout.strip() == "DONE" and "--sources NDI cam5" in log.read_text(), r.stderr


def test_the_finder_heal_wait_is_skipped_on_the_jpeg_xs_path(tmp_path):
    seen = tmp_path / "heal.log"
    cmd = tmp_path / "heal.sh"
    cmd.write_text(f"#!/bin/bash\necho called >> {seen}\n")
    cmd.chmod(0o755)
    r = _bash('mv_reverify_finder_heal_wait s "cam5" 5', libs=(CAMSET, LIB, MVLIB),
              env={"E2E_PROGRAM_PATH": "jxs", "MV_REVERIFY_HEAL_WAIT_CMD": str(cmd)})
    assert r.returncode == 0 and not seen.exists() and "skipped: the JPEG XS path" in r.stderr
    r = _bash('mv_reverify_finder_heal_wait s "cam5" 5', libs=(CAMSET, LIB, MVLIB),
              env={"MV_REVERIFY_HEAL_WAIT_CMD": str(cmd)})
    assert r.returncode == 0 and seen.read_text().strip() == "called", r.stderr


# ---------------------------------------------------------------------------------------------------
# the wiring in recording-e2e.sh (static: the text is the contract) + one executed slice


def _e2e():
    return E2E.read_text()


def test_the_e2e_sources_the_lib_once_and_resolves_before_any_rig_mutation():
    s = _e2e()
    assert s.count('. "$HERE/lib/program-inputs.sh"') == 1
    call = 'program_inputs_resolve "$STRIH" "$STREAM" "$CAMERA_NAME" && program_inputs_refuse_ndi_only_optins || exit 2'
    assert s.count(call) == 1
    pos = s.index(call)
    assert s.index('STRIH_LX_GATE_PREFIX="$(strih_lx_gate_prefix') < pos < s.index('RUN_ID="${RUN_ID:-')
    assert pos < s.index("bkshading_e2e_pause_stop ") and pos < s.index("trap cleanup EXIT HUP INT TERM")
    assert pos < s.index('STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-$CAMERA_STRIH_SOURCE}"')


def test_both_burn_units_carry_the_cameras_jpeg_xs_target():
    s = _e2e()
    assert s.count("$(cambox_jxs_burn_setenv_resolve_cmd) \\\n") == 2
    assert s.count("\\$CAMBOX_JXS_BURN_SETENV \\\n") == 2
    assert "   $(cpu_affinity_burn_resolve_cmd) \\\n   $(cambox_jxs_burn_setenv_resolve_cmd) \\\n" in s
    assert "       $(cpu_affinity_burn_resolve_cmd) \\\n       $(cambox_jxs_burn_setenv_resolve_cmd) \\\n" in s
    assert "     \\$CPU_AFFINITY_BURN_PROPERTY \\\n     \\$CAMBOX_JXS_BURN_SETENV \\\n" in s
    assert "         \\$CPU_AFFINITY_BURN_PROPERTY \\\n         \\$CAMBOX_JXS_BURN_SETENV \\\n" in s


def test_the_ndi_pin_steps_are_ndi_only():
    s = _e2e()
    for cond in ('if [ "$PHASE_REANCHOR" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ] && program_path_ndi; then',
                 'if [ "${ALL_CAMBOX:-0}" = "1" ] && [ "${QR_ALIGN:-1}" != "1" ] && ! measurement_eq_enabled && program_path_ndi; then',
                 'if [ "$QR_ALIGN" = "1" ] && [ "${ALL_CAMBOX:-0}" = "1" ] && ! measurement_eq_enabled && program_path_ndi; then'):
        assert s.count(cond) == 1, cond


def test_the_jpeg_xs_step_and_the_rendered_check_run_before_the_recording():
    s = _e2e()
    jxs = 'program_inputs_jxs_preflight "$HERE" "$STRIH" "$STREAM" "$OUTDIR" "$RUN_ID" || exit 1'
    rendered = 'program_inputs_rendered_verify "$HERE" "$STRIH" "$STREAM" "${CAMBOX_SWEEP:-$(camera_active_sweep_pairs)}" || exit 1'
    assert s.count(jxs) == 1 and s.count(rendered) == 1
    floor_gate = s.index("phase_sync_active_floor_check.py")
    align = s.index("# [4i/8align] #1003 floor-3 per-run camera alignment")
    assert floor_gate < s.index(jxs) < s.index(rendered) < align < s.index("[5/8] StartRecord")


def test_the_all_camera_burn_targets_follow_the_measured_inputs():
    s = _e2e()
    assert '_acs="$(camera_strih_input "$_acn")"' in s and '_acs="NDI $_acn"' not in s
    start = s.index('if [ "${ALL_CAMBOX:-0}" = "1" ]; then\n  for _acn in')
    block = s[start:s.index("\ntrap cleanup EXIT HUP INT TERM", start)]
    body = f'BURN_TARGETS=()\nSTRIH=1.2.3.4\n{block}\nprintf "%s\\n" "${{BURN_TARGETS[@]}}"'
    env = {"ALL_CAMBOX": "1", "CAMERA_ACTIVE_SET": "cam2 cam3 cam5", "CAMERA_STRIH_JXS_SET": "cam2 cam3 cam5",
           "STRIH_PROG_SOURCE": "JXS cam5 src"}
    r = _bash(body, env=env)
    assert r.stdout.splitlines() == ["strih-JXS_cam2_src=1.2.3.4=JXS cam2 src", "strih-JXS_cam3_src=1.2.3.4=JXS cam3 src"], r.stderr
    r = _bash(body, env=dict(env, CAMERA_STRIH_JXS_SET="", STRIH_PROG_SOURCE="NDI cam5"))
    assert r.stdout.splitlines() == ["strih-NDI_cam2=1.2.3.4=NDI cam2", "strih-NDI_cam3=1.2.3.4=NDI cam3"], r.stderr


def test_the_av_apply_hold_sits_before_the_guard_and_the_apply():
    s = _e2e()
    assert s.count("  program_inputs_av_pin_hold\n") == 1
    pos = s.index("  program_inputs_av_pin_hold\n")
    assert s.index('AV_SYNC_PROPOSED_OFFSET_MS="$AV_SYNC_APPLY_OFFSET_MS"') < pos
    assert pos < s.index('_avs_hold="$(av_sync_apply_guard_decide') < s.index("#856: applying this run's own")


def test_the_frozen_gate_list_and_the_sender_bounce_check_read_the_measured_inputs():
    s = _e2e()
    assert '"$HERE/obs_phase2.py" "$CAMERA_ACTIVE_SET" "$(camera_active_ndi_sources_csv)" <<\'PYEOF\'' in s
    f = s[s.index("preflight_mv_reverify() {"):]
    f = f[:f.index("\n}\n")]
    assert '--sources "$(mv_reverify_input "$cam_n")"' in f and '--sources "NDI cam${cam_n}"' not in f
    assert f.index('! mv_reverify_ndi_cures "$cam_n"') < f.index("strih_mv_scenes.py")
    assert f.count("strih_mv_scenes.py") == 1


# ---------------------------------------------------------------------------------------------------
# rig-mode.sh + av-soak.sh


RIG_PRINT = ('printf "path=%s\\nstrih=%s\\nstream=%s\\n" "${E2E_PROGRAM_PATH:-}" "$STRIH_PROG_SOURCE" "$STREAM_PROG_SOURCE"; '
             'obs_burn_targets')


def _rig(env=None):
    return subprocess.run(["bash", "-c", f'set -uo pipefail\n. "{RIG}"\n{RIG_PRINT}'], capture_output=True,
                          text=True, env=_env(env), timeout=180)


def test_rig_mode_test_burns_follow_the_jpeg_xs_inputs_on_the_snv_venue():
    r = _rig()
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines[:3] == ["path=jxs", "strih=JXS cam5 src", "stream=JXS PGM src"]
    assert f"{SNV_STRIH}|JXS cam5 src|strih" in lines and f"{SNV_STREAM}|JXS PGM src|stream" in lines


def test_rig_mode_on_a_venue_without_facts_keeps_the_ndi_defaults():
    r = _rig({"STRIH_IP": NOWHERE_A, "STREAM_IP": NOWHERE_B})
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines()[:3] == ["path=ndi", "strih=NDI cam5", "stream=NDI 2ME PGM"]


def test_rig_mode_resolves_between_the_hosts_and_the_burn_targets_above_the_source_guard():
    s = RIG.read_text()
    call = s.index('program_inputs_resolve "$STRIH_IP" "$STREAM_IP" "$RIG_SOURCE_BOX"')
    assert s.index('STREAM_IP="${STREAM_IP:-10.77.9.204}"') < call
    assert call < s.index('STRIH_PROG_SOURCE="${STRIH_PROG_SOURCE:-$RIG_SOURCE_STRIH_SOURCE}"')
    assert call < s.index('if [ "${BASH_SOURCE[0]}" != "${0}" ]; then')
    assert s.count('. "$RIG_MODE_DIR/lib/program-inputs.sh"') == 1


def test_rig_mode_event_latency_item_checks_the_measured_stream_input_against_the_fact():
    s = RIG.read_text()
    item = s[s.index("# --- item 6: stream PGM latency"):s.index("# --- item 7: NDI mapping")]
    assert '--source "$STREAM_PROG_SOURCE" --calibrated-ms "$calibrated_ms"' in item
    assert '--source "NDI 2ME PGM"' not in item
    assert 'program_path_ndi || calibrated_ms="${PROGRAM_INPUTS_STREAM_PIN_MS:-}"' in item
    assert item.index("av-sync-last.json") < item.index("program_path_ndi ||") < item.index("latency-check")


def test_the_soak_defaults_from_the_resolver_and_an_env_override_still_wins():
    s = SOAK.read_text()
    assert s.count('. "$HERE/lib/program-inputs.sh"') == 1
    call = s.index('program_inputs_resolve "$STRIH_HOST" "$STREAM_HOST" ""')
    assert s.index('STREAM_HOST="${STREAM_HOST:-') < call < s.index('STREAM_PROG_SOURCE="${STREAM_PROG_SOURCE:-NDI 2ME PGM}"')
    assert 'STRIH_INPUT_FMT="${AV_SOAK_STRIH_INPUT_FMT:-}"' in s
    loop = s[s.index("for _c in $SOAK_CAMS; do\n  _si="):]
    loop = loop[:loop.index("\ndone\n")]
    assert '_si="$(camera_strih_input "$_c")"' in loop
    assert loop.index("camera_strih_input") < loop.index('[ -z "$STRIH_INPUT_FMT" ] ||') < loop.index('BURN_TARGETS+=')


def test_the_e2e_builds_its_burn_binary_with_the_jpeg_xs_link():
    """The live run 41335442079: the burn unit got its CAMERA_BOX_JXS_TARGET, but the binary the E2E
    built itself had no `jxs` feature, logged `the JPEG XS link stays off`, and the measured input
    never moved. ci.yml's probe artifact already carried the feature; every other build must too."""
    text = (Path(__file__).resolve().parents[2] / "scripts" / "recording-e2e.sh").read_text()
    builds = [ln for ln in text.splitlines()
              if ln.lstrip().startswith("cargo build") and "--bin camera-box" in ln]
    assert builds, "the E2E build line"
    for ln in builds:
        assert "--features probe,jxs" in ln, ln
