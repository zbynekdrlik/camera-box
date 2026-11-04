"""Issue 1487: the strih seed against the PINNED service, `bkshading --check-config` from the release.

camera-box generates the seed (`strih_bkshading_config_text`), shading owns the parser. An unknown table
is ignored, so a misspelled `[[cameras]]` still exits 0 and prints `ok: 0 cameras`: the count is compared,
never only the exit code. This fetches the real release (the pin's bytes). No network = a FAILED test.
"""
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
BOXES = REPO / "scripts" / "strih-boxes"
EXAMPLE = REPO / "scripts" / "bkshading.example.toml"


@pytest.fixture(scope="module")
def service(tmp_path_factory):
    env = dict(os.environ, SHADING_RELEASE_CACHE=str(tmp_path_factory.mktemp("shading-release")))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. scripts/lib/shading-release.sh\n'
                        'shading_release_service "$PWD"'], capture_output=True, text=True, env=env,
                       cwd=str(REPO), timeout=240)
    assert r.returncode == 0, "the pinned shading release could not be fetched: " + r.stderr
    return Path(r.stdout.strip()) / "bkshading"


def _seed(box, tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_") and k != "OBS_FLEET"}
    env["STRIH_BOXES_DIR"] = str(d)
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. scripts/lib/strih-box-facts.sh\n'
                        '. scripts/lib/strih-provision.sh\nstrih_box_load "$1"\nstrih_bkshading_config_text',
                        "harness", box], capture_output=True, text=True, env=env, cwd=str(REPO), timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _check(service, path):
    return subprocess.run([str(service), "--check-config", str(path)], capture_output=True, text=True, timeout=30)


@pytest.mark.parametrize("box", ["strih-snv", "strih-pp"])
def test_the_pinned_service_reads_every_camera_of_the_seed(box, service, tmp_path):
    text = _seed(box, tmp_path)
    cams = tomllib.loads(text)["camera"]
    assert len(cams) == 7, "the seed declares all seven cameras (issue 1431)"
    f = tmp_path / "bkshading.toml"
    f.write_text(text)
    r = _check(service, f)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "ok: %d cameras" % len(cams), r.stdout


def test_a_misspelled_table_passes_the_exit_code_and_fails_the_count(service, tmp_path):
    f = tmp_path / "bkshading.toml"
    f.write_text(_seed("strih-snv", tmp_path).replace("[[camera]]", "[[cameras]]"))
    r = _check(service, f)
    assert r.returncode == 0 and r.stdout.strip() == "ok: 0 cameras", r.stdout + r.stderr


def test_the_pinned_service_refuses_a_broken_seed(service, tmp_path):
    f = tmp_path / "bkshading.toml"
    f.write_text('bind = 7\n')
    r = _check(service, f)
    assert r.returncode != 0 and str(f) in r.stderr, r.stdout + r.stderr


def test_the_windows_config_seed_loads_in_the_pinned_service(service):
    cams = tomllib.loads(EXAMPLE.read_text())["camera"]
    r = _check(service, EXAMPLE)
    assert r.returncode == 0 and r.stdout.strip() == "ok: %d cameras" % len(cams), r.stdout + r.stderr
