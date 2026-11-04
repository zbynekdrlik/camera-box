"""issue 1387 -- the JPEG XS path on the Linux OBS workstation boxes, provisioned from the box FACTS
(scripts/lib/strih-jxs-provision.sh + the four STRIH_JXS_* facts of scripts/lib/strih-box-facts.sh).

setup-strih step 6 renders the declared inputs into /opt/camera-box/jxs-seed.json (the launch seeder
obs_jxs_inputs.py reads it) and, on the strih role, upserts the program output's jxs-output.json towards
the VENUE's stream box; verify-strih item 49 grades the manifest, the inputs over obs-websocket and the
output file. Every bash call runs under the callers' `set -euo pipefail`, values passed as ARGUMENTS.
"""
import importlib.util
import json
import os
import re
import shutil
import socket
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-jxs-provision.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
FACTS_DIR = SCRIPTS / "strih-boxes"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
START = SCRIPTS / "strih-obs-start.sh"
OUT_DECIDE = REPO / "vendor" / "distroav" / "src" / "jxs-output-decide.h"
SRC_DECIDE = REPO / "vendor" / "distroav" / "src" / "jxs-source-decide.h"
LIVE_OUTPUT = ('{"enabled": true, "jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100, "jxs_bpp_min": 0, '
               '"jxs_bpp_max": 0, "jxs_cores": "", "jxs_pace_mbps": 0}\n')  # strih-snv, 31.10.2026, 147 bytes


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


oji = _load("obs_jxs_inputs", SCRIPTS / "obs_jxs_inputs.py")


def _bash(body, *args, env=None, baseline=False):
    pre = "set -euo pipefail\n" + f'. "{PROVISION}"\n'
    if baseline:
        pre += f'fail() {{ echo "FAIL: $*" >&2; exit 1; }}\n. "{BASELINE}"\n'
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", pre + body, "harness", *map(str, args)], capture_output=True,
                          text=True, cwd=str(REPO), env=e, timeout=120)


def _fact(box, key):
    for line in (FACTS_DIR / f"{box}.env").read_text().splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1]
    raise KeyError(key)


# --- the facts ----------------------------------------------------------------------------------------

def test_the_four_fact_files_declare_the_live_path_and_pp_is_deferred():
    assert _fact("strih-snv", "STRIH_JXS_CAMERAS") == "2:7002 3:7003 5:7000"
    assert _fact("strih-snv", "STRIH_JXS_CAMERA_PIN_MS") == "36"
    assert (_fact("stream-snv", "STRIH_JXS_PROGRAM_PORT"), _fact("stream-snv", "STRIH_JXS_PROGRAM_PIN_MS")) == ("7100", "1054")
    for box in ("strih-pp", "stream-pp"):
        for key in ("STRIH_JXS_CAMERAS", "STRIH_JXS_CAMERA_PIN_MS", "STRIH_JXS_PROGRAM_PORT", "STRIH_JXS_PROGRAM_PIN_MS"):
            assert _fact(box, key) == "none", (box, key)
    for n in [e.split(":")[0] for e in _fact("strih-snv", "STRIH_JXS_CAMERAS").split()]:
        assert n in _fact("strih-snv", "STRIH_CAMERAS").split()


def test_every_fact_file_loads():
    for box in ("strih-snv", "stream-snv", "strih-pp", "stream-pp"):
        r = _bash('strih_box_load "$1"', box)
        assert r.returncode == 0, (box, r.stderr)


def test_the_camera_pin_is_the_owners_100_ms_grid_age():
    # ROZHODNUTE 6546700019: pin_for_frames(3, 1, 33.3 ms) = 36, present_age_ms(36) = 100.0
    g = _load("genlock_n2_grid", SCRIPTS / "genlock_n2_grid.py")
    si = g.source_interval_ns(g.canvas_interval_from_fps(30), 1)
    pin = int(_fact("strih-snv", "STRIH_JXS_CAMERA_PIN_MS"))
    assert g.pin_for_frames(3, 1, si) == pin and g.present_age_ms(pin, si) == 100.0


def test_the_pin_range_is_the_jxs_source_clamp():
    src = SRC_DECIDE.read_text()
    lo = re.search(r"#define CB_JXS_LATENCY_MS_MIN (\d+)", src).group(1)
    hi = re.search(r"#define CB_JXS_LATENCY_MS_MAX (\d+)", src).group(1)
    r = _bash('printf "%s %s\\n" "$STRIH_BOX_JXS_PIN_MS_MIN" "$STRIH_BOX_JXS_PIN_MS_MAX"')
    assert r.stdout.split() == [lo, hi]


@pytest.mark.parametrize("args, ok, why", [
    (("strih", "2 3 5", "2:7002 3:7003 5:7000", "36", "none", "none"), True, ""),
    (("strih", "2 3 5", "none", "none", "none", "none"), True, ""),
    (("stream", "none", "none", "none", "7100", "1087"), True, ""),
    (("stream", "none", "none", "none", "none", "none"), True, ""),
    (("strih", "2 3 5", "6:7006", "36", "none", "none"), False, "not one of this box's STRIH_CAMERAS"),
    (("strih", "2 3 5", "2:7002 2:7003", "36", "none", "none"), False, "camera 2 twice"),
    (("strih", "2 3 5", "2:7002 3:7002", "36", "none", "none"), False, "port 7002 to two cameras"),
    (("strih", "2 3 5", "2:80", "36", "none", "none"), False, "not N:PORT"),
    (("strih", "2 3 5", "2-7002", "36", "none", "none"), False, "not N:PORT"),
    (("strih", "2 3 5", "02:7002", "36", "none", "none"), False, "not N:PORT"),
    (("strih", "2 3 5", "2:7002", "none", "none", "none"), False, "STRIH_JXS_CAMERA_PIN_MS"),
    (("strih", "2 3 5", "2:7002", "2", "none", "none"), False, "STRIH_JXS_CAMERA_PIN_MS"),
    (("strih", "2 3 5", "2:7002", "2001", "none", "none"), False, "STRIH_JXS_CAMERA_PIN_MS"),
    (("strih", "2 3 5", "none", "36", "none", "none"), False, "must be none while STRIH_JXS_CAMERAS is none"),
    (("stream", "none", "2:7002", "36", "none", "none"), False, "none on the stream role"),
    (("strih", "2 3 5", "none", "none", "7100", "1087"), False, "none on the strih role"),
    (("stream", "none", "none", "none", "7100", "none"), False, "STRIH_JXS_PROGRAM_PIN_MS"),
    (("stream", "none", "none", "none", "none", "1087"), False, "must be none while STRIH_JXS_PROGRAM_PORT"),
    (("stream", "none", "none", "none", "70000", "1087"), False, "UDP port 1024..65535"),
    (("stream", "none", "none", "none", "1000", "1087"), False, "UDP port 1024..65535"),
    (("strih", "2 3 5", "2:1023", "36", "none", "none"), False, "not N:PORT"),
    (("strih", "2 3 5", "2:1024", "36", "none", "none"), True, ""),
])
def test_the_jxs_facts_validator(args, ok, why):
    r = _bash('strih_box_jxs_facts_ok "$1" "$2" "$3" "$4" "$5" "$6" "T: "', *args)
    assert (r.returncode == 0) == ok, (args, r.stderr)
    if why:
        assert why in r.stderr, r.stderr


def test_a_fact_file_with_a_bad_jxs_fact_refuses_to_load(tmp_path):
    for f in FACTS_DIR.glob("*.env"):
        shutil.copy(f, tmp_path)
    p = tmp_path / "strih-snv.env"
    p.write_text(p.read_text().replace("STRIH_JXS_CAMERAS=2:7002 3:7003 5:7000", "STRIH_JXS_CAMERAS=2:7002 7:7007"))
    r = _bash('strih_box_load strih-snv', env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode != 0 and "camera 7 is not one of this box's STRIH_CAMERAS" in r.stderr


def test_port_of_reads_one_camera():
    r = _bash('strih_box_jxs_port_of "2:7002 3:7003 5:7000" 5; strih_box_jxs_port_of "2:7002 12:7012" 1; '
              'strih_box_jxs_port_of none 2')
    assert r.stdout.splitlines() == ["7000", "none", "none"]


# --- the manifest ---------------------------------------------------------------------------------------

def _manifest(box):
    r = _bash('strih_box_load "$1"\nstrih_jxs_seed_manifest_json', box)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_strih_snv_declares_its_three_camera_inputs():
    specs = oji.parse_manifest(_manifest("strih-snv"))
    assert [(s.name, s.scene) for s in specs] == [("JXS cam2 src", "JXS cam2"), ("JXS cam3 src", "JXS cam3"),
                                                  ("JXS cam5 src", "JXS cam5")]
    assert [(s.settings["jxs_port"], s.settings["jxs_camera"]) for s in specs] == [(7002, 2), (7003, 3), (7000, 5)]
    for s in specs:
        assert s.settings["genlock_latency_ms_src"] == 36 and s.settings["genlock_grid"] is True
        assert s.settings["genlock_fifo"] is True and s.settings["jxs_max_bpp"] == 0


def test_stream_snv_declares_its_program_input_on_the_programs_stream_id():
    sid = int(re.search(r"#define CB_JXS_OUT_STREAM_ID_DEFAULT (\d+)u", OUT_DECIDE.read_text()).group(1))
    assert _bash('printf "%s\\n" "$STRIH_JXS_PROGRAM_STREAM_ID"').stdout.strip() == str(sid)
    (s,) = oji.parse_manifest(_manifest("stream-snv"))
    assert s.name == "JXS PGM src" and s.scene == "JXS PGM"
    assert s.settings == {"jxs_port": 7100, "jxs_camera": sid, "jxs_max_bpp": 0, "genlock_latency_ms_src": 1054,
                          "genlock_fifo": True, "genlock_grid": False}


def test_a_pp_box_declares_nothing():
    assert _manifest("strih-pp") == "" and _manifest("stream-pp") == ""


# --- the program target ----------------------------------------------------------------------------------

def test_the_program_target_is_the_venues_stream_box():
    r = _bash('for b in strih-snv stream-snv strih-pp stream-pp; do strih_box_load "$b"; strih_jxs_program_target; done')
    assert r.stdout.splitlines() == ["10.77.9.204:7100", "none", "none", "none"], r.stderr


def test_two_stream_boxes_in_one_venue_refuse(tmp_path):
    for f in FACTS_DIR.glob("*.env"):
        shutil.copy(f, tmp_path)
    (tmp_path / "stream-xx.env").write_text((FACTS_DIR / "stream-snv.env").read_text())
    r = _bash('strih_box_load strih-snv; strih_jxs_program_target', env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode != 0 and "two stream boxes" in r.stderr


# --- the program output file -----------------------------------------------------------------------------

def test_the_render_of_the_live_file_is_the_live_file(tmp_path):
    f = tmp_path / "jxs-output.json"
    f.write_text(LIVE_OUTPUT)
    r = _bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100', f)
    assert r.stdout == LIVE_OUTPUT


def test_the_render_keeps_the_operators_keys_and_refuses_garbage(tmp_path):
    f = tmp_path / "jxs-output.json"
    f.write_text('{"jxs_pace_mbps": 900, "jxs_bpp_max": 6, "enabled": false, "x": 1}')
    out = json.loads(_bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100', f).stdout)
    assert out == {"enabled": True, "jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100, "jxs_bpp_min": 0,
                   "jxs_bpp_max": 6, "jxs_cores": "", "jxs_pace_mbps": 900, "x": 1}
    f.write_text("[]")
    assert _bash('strih_jxs_output_render "$1" t 100', f).returncode == 3


# --- setup-strih step 6, end to end on temp paths -------------------------------------------------------

def _seams(tmp_path):
    return {"STRIH_JXS_SEED_MANIFEST": str(tmp_path / "opt" / "jxs-seed.json"),
            "STRIH_JXS_SEEDER_BIN": str(tmp_path / "bin" / "obs_jxs_inputs.py")}


def _provision(tmp_path, box):
    home = tmp_path / "home"
    user = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    return _bash('strih_box_load "$1"\nstrih_jxs_provision "$2/.config/obs-studio" "$3" "$2" "$4"',
                 box, home, user, REPO, env=_seams(tmp_path), baseline=True), home


def test_step6_on_strih_snv_writes_the_manifest_the_seeder_and_the_live_output_file(tmp_path):
    r, home = _provision(tmp_path, "strih-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    out = home / ".config/obs-studio/plugin_config/distroav/jxs-output.json"
    assert out.read_text() == LIVE_OUTPUT and oct(out.stat().st_mode & 0o777) == "0o644"
    assert (tmp_path / "opt" / "jxs-seed.json").read_text() == _manifest("strih-snv")
    seeder = tmp_path / "bin" / "obs_jxs_inputs.py"
    assert seeder.read_bytes() == (SCRIPTS / "obs_jxs_inputs.py").read_bytes() and os.access(seeder, os.X_OK)
    # a re-run touches nothing
    r, _ = _provision(tmp_path, "strih-snv")
    assert r.returncode == 0 and "unchanged" in r.stdout and "written" not in r.stdout, r.stdout


def test_step6_on_the_stream_box_writes_its_manifest_and_no_output_file(tmp_path):
    r, home = _provision(tmp_path, "stream-snv")
    assert r.returncode == 0, r.stderr
    assert oji.parse_manifest((tmp_path / "opt" / "jxs-seed.json").read_text())[0].name == "JXS PGM src"
    assert not (home / ".config/obs-studio/plugin_config").exists()


def test_step6_on_a_pp_box_writes_nothing_jxs(tmp_path):
    r, home = _provision(tmp_path, "strih-pp")
    assert r.returncode == 0, r.stderr
    assert not (tmp_path / "opt" / "jxs-seed.json").exists() and not (home / ".config").exists()
    assert "left as it is" in r.stdout


def test_step6_moves_an_unparseable_output_file_aside(tmp_path):
    out = tmp_path / "home/.config/obs-studio/plugin_config/distroav/jxs-output.json"
    out.parent.mkdir(parents=True)
    out.write_text("not json")
    r, _ = _provision(tmp_path, "strih-snv")
    assert r.returncode == 0, r.stderr
    assert out.read_text() == LIVE_OUTPUT
    assert [p.read_text() for p in out.parent.glob("jxs-output.json.bak-*")] == ["not json"]


def test_step6_never_reads_or_writes_through_a_planted_symlink(tmp_path):
    # setup runs as root in the desktop user's tree: a link to a root-only JSON file must leak nothing
    secret = tmp_path / "root-only.json"
    secret.write_text('{"api_key": "SECRET-ROOT-ONLY-VALUE"}')
    out = tmp_path / "home/.config/obs-studio/plugin_config/distroav/jxs-output.json"
    out.parent.mkdir(parents=True)
    out.symlink_to(secret)
    r, _ = _provision(tmp_path, "strih-snv")
    assert r.returncode != 0 and "is a symlink" in r.stderr
    assert out.is_symlink() and secret.read_text() == '{"api_key": "SECRET-ROOT-ONLY-VALUE"}'
    # the render alone refuses the link too (O_NOFOLLOW), and a non-regular file
    assert _bash('strih_jxs_output_render "$1" t 100', out).returncode == 4
    fifo = tmp_path / "fifo.json"
    os.mkfifo(fifo)
    assert _bash('strih_jxs_output_render "$1" t 100', fifo).returncode == 4


def test_step6_disables_a_hand_enabled_output_where_the_venue_declares_none(tmp_path):
    out = tmp_path / "home/.config/obs-studio/plugin_config/distroav/jxs-output.json"
    out.parent.mkdir(parents=True)
    out.write_text(LIVE_OUTPUT)
    r, home = _provision(tmp_path, "strih-pp")
    assert r.returncode == 0, r.stderr
    d = json.loads(out.read_text())
    assert d["enabled"] is False and d["jxs_target"] == "10.77.9.204:7100" and d["jxs_pace_mbps"] == 0
    r = _verify(tmp_path, "strih-pp", home)
    assert "FAIL" not in r.stdout, r.stdout
    # a disabled file is left alone
    r, _ = _provision(tmp_path, "strih-pp")
    assert "nothing enabled" in r.stdout


# --- verify-strih item 49's config rows ------------------------------------------------------------------

def _closed_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _verify(tmp_path, box, home, obs_runs=False):
    env = dict(_seams(tmp_path), STRIH_JXS_OBS_PORT=str(_closed_port()),
               STRIH_OPERATOR_OBS_RUNNING_CMD="true" if obs_runs else "false")
    return _bash('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
                 'strih_box_load "$1"\nstrih_jxs_config_verify_row "$2"', box, home, env=env)


def test_verify_fails_an_unreachable_websocket_while_obs_runs(tmp_path):
    _, home = _provision(tmp_path, "strih-snv")
    lines = _verify(tmp_path, "strih-snv", home, obs_runs=True).stdout.splitlines()
    assert lines[1].startswith("FAIL (jxs-inputs) OBS WebSocket 127.0.0.1:") and "OBS runs" in lines[1]


def test_verify_passes_a_provisioned_strih_with_obs_down(tmp_path):
    _, home = _provision(tmp_path, "strih-snv")
    r = _verify(tmp_path, "strih-snv", home)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert lines[0].startswith("PASS (jxs-seed)")
    assert lines[1].startswith("NOTE (jxs-inputs) OBS WebSocket 127.0.0.1:")
    assert lines[2].startswith("PASS (jxs-output)") and "10.77.9.204:7100" in lines[2]


def test_verify_fails_a_hand_state_strih(tmp_path):
    home = tmp_path / "home"
    out = home / ".config/obs-studio/plugin_config/distroav/jxs-output.json"
    out.parent.mkdir(parents=True)
    out.write_text(LIVE_OUTPUT.replace("10.77.9.204:7100", "10.77.9.204:7101"))
    r = _verify(tmp_path, "strih-snv", home)
    fails = [ln for ln in r.stdout.splitlines() if ln.startswith("FAIL")]
    assert any("(jxs-seed)" in ln and "missing or differs" in ln for ln in fails), r.stdout
    assert any("(jxs-output)" in ln and "10.77.9.204:7101" in ln for ln in fails), r.stdout


def test_verify_fails_a_stale_installed_manifest(tmp_path):
    _, home = _provision(tmp_path, "strih-snv")
    m = tmp_path / "opt" / "jxs-seed.json"
    m.write_text(m.read_text().replace('"genlock_latency_ms_src": 36', '"genlock_latency_ms_src": 3'))
    assert "FAIL (jxs-seed)" in _verify(tmp_path, "strih-snv", home).stdout


def test_verify_on_a_pp_box_notes_and_fails_a_hand_enabled_output(tmp_path):
    home = tmp_path / "home"
    r = _verify(tmp_path, "strih-pp", home)
    assert [ln.split(" ", 2)[:2] for ln in r.stdout.splitlines()] == [["NOTE", "(jxs-seed)"], ["NOTE", "(jxs-output)"]]
    out = home / ".config/obs-studio/plugin_config/distroav/jxs-output.json"
    out.parent.mkdir(parents=True)
    out.write_text(LIVE_OUTPUT)
    assert "FAIL (jxs-output)" in _verify(tmp_path, "strih-pp", home).stdout


# --- the wiring ------------------------------------------------------------------------------------------

def test_setup_strih_step6_calls_it_once_after_the_seed_manifest_within_budget():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    assert s.count("strih_jxs_provision ") == 1
    step6 = s[s.index('step 6 "'):s.index('step 7 "')]
    assert step6.index("obs_box_seed_manifest_json > /opt/camera-box/strih-seed.json") \
        < step6.index('strih_jxs_provision "$OBS_CFG" "$DESKTOP_USER" "$USER_HOME" "${HERE}/.." || fail')


def test_verify_strih_runs_the_config_rows_next_to_item_49_within_budget():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    i = v.index('strih_jxs_verify_row "$LOG"')
    j = v.index('strih_jxs_config_verify_row "$USER_HOME"')
    assert i < j and "\n" not in v[v.index("\n", i) + 1:j]


def test_strih_provision_sources_the_lib():
    p = PROVISION.read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-jxs-provision.sh"' in p


def test_the_launcher_runs_the_jxs_seed_after_the_scene_seed_only_with_a_manifest():
    w = START.read_text()
    scn = w.index('python3 "$SCN" --bootstrap')
    guard = w.index('if [ -f "$JXS_MANIFEST" ]; then')
    jxs = w.index('if python3 "$JXS_SEED" --bootstrap --manifest "$JXS_MANIFEST"; then')
    assert scn < guard < jxs < w.index('wait "$OBS_PID"')
    # the launcher reads the same overrides as the lib
    assert 'JXS_SEED="${STRIH_JXS_SEEDER_BIN:-/usr/local/bin/obs_jxs_inputs.py}"' in w
    assert 'JXS_MANIFEST="${STRIH_JXS_SEED_MANIFEST:-/opt/camera-box/jxs-seed.json}"' in w
    # the launcher's default paths are the lib's
    lib = LIB.read_text()
    assert 'STRIH_JXS_SEED_MANIFEST="${STRIH_JXS_SEED_MANIFEST:-/opt/camera-box/jxs-seed.json}"' in lib
    assert 'STRIH_JXS_SEEDER_BIN="${STRIH_JXS_SEEDER_BIN:-/usr/local/bin/obs_jxs_inputs.py}"' in lib
    assert oji.MANIFEST_PATH == "/opt/camera-box/jxs-seed.json"
