"""The development handover check must run each dev1 alert watchdog with the SAME environment its
systemd --user unit gives the production timer.

Found 15.10.2026 after the evening event: obs-liveness reads its gate binary from
OBS_WATCHDOG_GATE_BIN, which only the unit's dev1 drop-in sets. The handover check ran the dry-run
from a shell without it, the probe printed "obs-watchdog-gate binary not found", and the `obs` item
read UNKNOWN on a rig where the timer read HEALTHY on every box.

Tier-0: pytest + bash, a fake `systemctl` on PATH, no rig.
"""
import os
import pathlib
import stat
import subprocess

_REPO = pathlib.Path(__file__).resolve().parents[2]
_SCRIPT = _REPO / "scripts" / "rig-dev-handover-check.sh"
_LIB = _REPO / "scripts" / "lib" / "unit-env.sh"


def _exe(path: pathlib.Path, text: str) -> pathlib.Path:
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _fake_systemctl(bindir: pathlib.Path, env_value: str) -> None:
    # Answers ONLY `systemctl --user show <unit> -p Environment --value` for the obs-liveness unit.
    _exe(bindir / "systemctl", f"""#!/usr/bin/env bash
if [ "$1" = --user ] && [ "$2" = show ] && [ "$3" = obs-liveness-watchdog.service ]; then
  printf '%s\\n' '{env_value}'
fi
exit 0
""")


def _lib(bindir, unit, extra_env=None):
    env = {"PATH": f"{bindir}:{os.environ['PATH']}", "HOME": os.environ.get("HOME", "/tmp")}
    env.update(extra_env or {})
    return subprocess.run(
        ["bash", "-c", f'set -euo pipefail; . "{_LIB}"; unit_env_assignments "$1"', "_", unit],
        capture_output=True, text=True, env=env, timeout=30)


def test_lib_prints_the_units_variables_and_undoes_systemd_quoting(tmp_path):
    _fake_systemctl(tmp_path, 'OBS_WATCHDOG_GATE_BIN=/opt/gate "SPACED=two words" X=1')
    r = _lib(tmp_path, "obs-liveness-watchdog.service")
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == [
        "OBS_WATCHDOG_GATE_BIN=/opt/gate", "SPACED=two words", "X=1"]


def test_lib_never_overrides_a_variable_the_caller_set(tmp_path):
    _fake_systemctl(tmp_path, "OBS_WATCHDOG_GATE_BIN=/opt/gate X=1")
    r = _lib(tmp_path, "obs-liveness-watchdog.service", {"OBS_WATCHDOG_GATE_BIN": "/mine"})
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["X=1"]


def test_lib_prints_nothing_for_an_unknown_unit_or_a_missing_systemctl(tmp_path):
    _fake_systemctl(tmp_path, "OBS_WATCHDOG_GATE_BIN=/opt/gate")
    assert _lib(tmp_path, "nope.service").stdout == ""
    empty = tmp_path / "empty"
    empty.mkdir()
    r = subprocess.run(
        ["bash", "-c", f'set -euo pipefail; . "{_LIB}"; unit_env_assignments x.service /no/such'],
        capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout == ""


def test_handover_runs_the_obs_watchdog_with_its_units_environment(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _fake_systemctl(bindir, "OBS_WATCHDOG_GATE_BIN=/opt/from-unit/obs-watchdog-gate")
    # A stub watchdog with the real unit's script name: it reports what it was given.
    stub = _exe(tmp_path / "obs-liveness-watchdog.sh",
                '#!/usr/bin/env bash\necho "gate_bin=${OBS_WATCHDOG_GATE_BIN:-UNSET} args=$*"\n')
    missing = str(tmp_path / "missing")
    env = {
        "PATH": f"{bindir}:{os.environ['PATH']}",
        "HOME": str(tmp_path),
        "TMPDIR": str(tmp_path),
        "RDH_OBS_PROBE": str(stub),
        "RDH_TIMEOUT": "10",
    }
    for item in ("MIC", "MIC_GUARD", "PAINTER", "CLOCK", "NET", "AUDIOLAG", "GENLOCK", "BURN", "MAPPING",
                 "PINS", "DANTESYNC", "CAMBOX", "AVLATENCY", "EXPOSURE", "RIGMODE_LIB",
                 "WATCHDOG_ROSTER", "CAMSET_LIB"):
        key = f"RDH_{item}" if item.endswith("_LIB") or item == "WATCHDOG_ROSTER" else f"RDH_{item}_PROBE"
        env[key] = missing
    env["RDH_IMAG_RETIRED"] = "1"
    subprocess.run(["bash", str(_SCRIPT), "--keep"], capture_output=True, text=True, env=env,
                   timeout=120)
    outs = list(tmp_path.glob("rig-dev-handover.*/obs.out"))
    assert len(outs) == 1, outs
    assert outs[0].read_text().strip() == \
        "gate_bin=/opt/from-unit/obs-watchdog-gate args=--dry-run"
