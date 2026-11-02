"""issue 1467 + 1487: verify names a hub that is not the pinned intercom release.

23.10.2026: the FOH console's 24-bit VBAN stream was dropped silently by a hub binary built before
the 24-bit decode. The fetch itself is tests/python/test_intercom_release_1487.py. Pinned here:

  * verify item 16's version row: a hub other than the pin is a NOTE with the reinstall line, the pin
    passes, nothing readable is a NOTE; a known stream with decode rejects is a NOTE naming the leg;
  * verify item 16 calls the grade.
"""
import json
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
RELEASE = SCRIPTS / "lib" / "strih-intercom-release.sh"
HUB_UNIT = SCRIPTS / "lib" / "strih-intercom-hub-unit.sh"
VERIFY = SCRIPTS / "verify-strih.sh"

LEAKY = ("STRIH_HUB_VERSION_JSON_FILE", "STRIH_HUB_DECODE_STATE_FILE")
PIN = "9.9.3"


def _run(script, *args, env=None):
    """Run SCRIPT under the callers' `set -euo pipefail` with ARGS as $1.. (never in the text)."""
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    full["STRIH_HUB_AMBIENT_CAPS"] = ""
    if env:
        full.update(env)
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *map(str, args)],
                          capture_output=True, text=True, env=full, cwd=str(REPO), timeout=60)


def _tree(tmp_path, pin=PIN):
    root = tmp_path / "stage" / "repo"
    (root / "scripts").mkdir(parents=True)
    (root / "scripts" / "intercom-release.pin").write_text(
        "INTERCOM_RELEASE_TAG=v%s\nINTERCOM_RELEASE_SHA256=%s\n" % (pin, "a" * 64))
    return root


# --- verify item 16: the version row ---------------------------------------------------------------

def _grade(tmp_path, running=None, recorded=None, state=None):
    root = _tree(tmp_path)
    binp = tmp_path / "bin" / "intercom-hub"
    binp.parent.mkdir(exist_ok=True)
    if recorded:
        (tmp_path / "bin" / "intercom-hub.version").write_text("version=%s\nrelease_tag=x\ntarball_sha256=x\n" % recorded)
    env = {}
    vj = tmp_path / "version.json"
    vj.write_text(json.dumps({"version": running}) if running else "")
    env["STRIH_HUB_VERSION_JSON_FILE"] = str(vj)
    sj = tmp_path / "state.json"
    sj.write_text(json.dumps(state) if state is not None else "")
    env["STRIH_HUB_DECODE_STATE_FILE"] = str(sj)
    script = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              '. "$1"\n. "$2"\nstrih_intercom_hub_version_grade http://h:8790 strih-snv "$3" "$4"\necho END')
    return _run(script, RELEASE, HUB_UNIT, root, binp, env=env)


def test_a_hub_other_than_the_pin_is_a_note_with_the_reinstall_line(tmp_path):
    for i, other in enumerate(("9.9.2", "9.9.4", "1.8.3")):
        r = _grade(tmp_path / str(i), running=other)
        assert r.returncode == 0, r.stderr
        lines = [ln for ln in r.stdout.splitlines() if "runs %s" % other in ln]
        assert lines and lines[0].startswith("NOTE "), r.stdout
        assert "not the pinned release %s" % PIN in lines[0]
        assert "setup-strih.sh --box strih-snv" in lines[0] and "systemctl restart intercom-hub" in lines[0]
        assert r.stdout.rstrip().endswith("END")


def test_the_pinned_hub_passes(tmp_path):
    r = _grade(tmp_path, running=PIN)
    assert r.returncode == 0, r.stderr
    assert any(ln.startswith("PASS ") and PIN in ln for ln in r.stdout.splitlines()), r.stdout


def test_a_hub_that_does_not_answer_is_graded_from_its_recorded_version(tmp_path):
    r = _grade(tmp_path, running=None, recorded="9.8.0")
    line = [ln for ln in r.stdout.splitlines() if "9.8.0" in ln]
    assert line and line[0].startswith("NOTE ") and "not the pinned" in line[0] and "recorded" in line[0], r.stdout


def test_nothing_readable_is_a_note_never_a_pass(tmp_path):
    r = _grade(tmp_path, running=None, recorded=None)
    assert r.returncode == 0, r.stderr
    rows = [ln for ln in r.stdout.splitlines() if "(intercom-hub)" in ln and "version" in ln]
    assert rows and all(ln.startswith("NOTE ") for ln in rows), r.stdout


def test_decode_rejects_on_a_known_stream_is_a_note_naming_the_leg(tmp_path):
    state = {"participants": [{"name": "fohabl", "adapter": "vban", "decode_rejects": 1234},
                              {"name": "cam1", "adapter": "vban", "decode_rejects": 0},
                              {"name": "cutters", "adapter": "pipewire"}]}
    r = _grade(tmp_path, running=PIN, state=state)
    rows = [ln for ln in r.stdout.splitlines() if "decode_rejects" in ln]
    assert len(rows) == 1 and rows[0].startswith("NOTE "), r.stdout
    assert "fohabl=1234" in rows[0] and "cam1" not in rows[0]
    quiet = _grade(tmp_path / "q", running=PIN, state={"participants": [{"name": "fohabl", "decode_rejects": 0}]})
    assert "decode_rejects" not in quiet.stdout, quiet.stdout


def test_a_state_larger_than_one_environment_string_is_still_graded(tmp_path):
    # review: one environment string is capped at 128 KB; the state reaches python as a file.
    big = [{"name": "leg%d" % i, "adapter": "vban", "decode_rejects": 0, "pad": "x" * 200} for i in range(1000)]
    big.append({"name": "fohabl", "adapter": "vban", "decode_rejects": 5})
    r = _grade(tmp_path, running=PIN, state={"participants": big})
    assert r.returncode == 0, r.stderr
    assert any(ln.startswith("NOTE ") and "fohabl=5" in ln for ln in r.stdout.splitlines()), r.stdout[-500:]
    assert "failed" not in r.stdout


# --- issue 1485: dantesync stays the only PTP owner ----------------------------------------------------

def _cap(tmp_path, ambient):
    script = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\n. "$1"\n. "$2"\n'
              'strih_intercom_hub_cap_grade "$3"\necho END')
    return _run(script, RELEASE, HUB_UNIT, tmp_path / "no-such-hub", env={"STRIH_HUB_AMBIENT_CAPS": ambient})


def test_the_hub_without_the_capability_passes(tmp_path):
    r = _cap(tmp_path, "")
    assert "PASS (intercom-hub) no CAP_NET_BIND_SERVICE" in r.stdout, r.stdout + r.stderr


def test_the_capability_on_the_unit_fails_naming_dantesync(tmp_path):
    r = _cap(tmp_path, "cap_net_bind_service")
    assert r.returncode == 0 and "FAIL (intercom-hub) CAP_NET_BIND_SERVICE" in r.stdout, r.stdout + r.stderr
    assert "dantesync" in r.stdout and r.stdout.rstrip().endswith("END")


def test_verify_item_16_grades_the_hub_version():
    v = VERIFY.read_text()
    i16 = v.index("# 16) intercom hub unit")
    block = v[i16:v.index("\n# ", i16 + 10)]
    assert 'strih_intercom_hub_version_grade "http://${WS_HOST}:8790" "$(strih_box_loaded_name)"' in block


def test_the_grade_defaults_to_the_tree_it_lives_in(tmp_path):
    """verify item 16 passes no root (its text needs no HERE): the pin next to the lib is the one read."""
    vj = tmp_path / "v.json"
    vj.write_text(json.dumps({"version": "0.0.1"}))
    script = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              '. "$1"\n. "$2"\nstrih_intercom_hub_version_grade http://h:8790 strih-snv\necho END')
    r = _run(script, RELEASE, HUB_UNIT, env={"STRIH_HUB_VERSION_JSON_FILE": str(vj),
                                             "STRIH_HUB_DECODE_STATE_FILE": str(tmp_path / "none")})
    assert r.returncode == 0, r.stderr
    tag = _run('. "$1"\nstrih_intercom_pin_tag "$2"', RELEASE, REPO).stdout.strip()
    assert tag and any(ln.startswith("NOTE ") and "not the pinned release %s" % tag[1:] in ln
                       for ln in r.stdout.splitlines()), r.stdout
