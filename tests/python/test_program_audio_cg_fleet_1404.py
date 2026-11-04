"""issue 1404 -- the CG arm's home check: scripts/obs_fleet_table.py's python twin of
scripts/lib/obs-fleet.sh `obs_fleet_canonical_name` / `obs_fleet_home_check` /
`obs_fleet_expected_identity` / `obs_fleet_is_home`, run side by side with the bash functions
through their offline seams (OBS_FLEET_HOME, OBS_FLEET_RESOLVE_CMD, OBS_FLEET_STATUS_PROBE_CMD).
No test touches the network: every resolve and probe is a stub.
"""
from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import obs_fleet_table as oft  # noqa: E402

LIB = _SCRIPTS / "lib" / "obs-fleet.sh"
NAMES = ["strih-snv", "stream-snv", "imag", "resolume-snv", "resolume", "RESOLUME-SNV", "strih-pp",
         "resolume-pp", "nope"]
_CLEAN = ("OBS_FLEET", "OBS_FLEET_HOME", "OBS_FLEET_IDENTITY", "OBS_FLEET_RESOLVE_CMD",
          "OBS_FLEET_STATUS_PROBE_CMD", "OBS_FLEET_HOME_PORT")


def _env(**extra):
    env = {k: v for k, v in os.environ.items() if k not in _CLEAN}
    env.update(extra)
    return env


def _bash(func, name, env):
    script = 'set -euo pipefail\n. "$1"\nset +e\n"$2" "$3"\necho "rc=$?"\n'
    r = subprocess.run(["bash", "-c", script, "harness", str(LIB), func, name], env=env,
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    out, _, rc = r.stdout.rpartition("rc=")
    return out, int(rc)


def _stub(tmp_path, name, body):
    p = tmp_path / name
    p.write_text("#!/bin/bash\n" + body + "\n", encoding="utf-8")
    p.chmod(0o755)
    return str(p)


@pytest.fixture
def clean_env(monkeypatch):
    for k in _CLEAN:
        monkeypatch.delenv(k, raising=False)
    return monkeypatch


@pytest.mark.parametrize("name", NAMES)
def test_canonical_name_home_check_and_identity_match_the_bash_lib(clean_env, name):
    env = _env()
    assert oft.fleet_canonical_name(name) == _bash("obs_fleet_canonical_name", name, env)[0]
    out, rc = _bash("obs_fleet_home_check", name, env)
    assert oft.fleet_home_check(name) == (out if rc == 0 else None)
    out, rc = _bash("obs_fleet_expected_identity", name, env)
    assert oft.fleet_expected_identity(name) == (out if rc == 0 else None)


def test_the_resolume_identity_is_the_ndi_machine_name(clean_env):
    assert oft.fleet_expected_identity("resolume-snv") == "RESOLUME-SNV"


def test_an_identity_override_set_but_empty_wins_like_bash(clean_env):
    clean_env.setenv("OBS_FLEET_IDENTITY", "")
    assert oft.fleet_expected_identity("resolume-snv") is None
    assert _bash("obs_fleet_expected_identity", "resolume-snv", _env(OBS_FLEET_IDENTITY=""))[1] == 1


@pytest.mark.parametrize("forced", ["resolume-snv", "resolume", "strih-snv stream-snv", "nope"])
@pytest.mark.parametrize("name", NAMES)
def test_the_forced_home_list_matches_the_bash_lib(clean_env, forced, name):
    clean_env.setenv("OBS_FLEET_HOME", forced)
    assert oft.fleet_is_home(name) == (_bash("obs_fleet_is_home", name, _env(OBS_FLEET_HOME=forced))[1] == 0)


@pytest.mark.parametrize("resolves, answers", [(True, True), (True, False), (False, True)])
@pytest.mark.parametrize("name", NAMES)
def test_the_live_home_check_matches_the_bash_lib(clean_env, tmp_path, name, resolves, answers):
    seen = []
    resolve_cmd = _stub(tmp_path, "resolve", 'echo 10.0.0.9' if resolves else 'exit 0')
    probe_cmd = _stub(tmp_path, "probe", f'echo "$1 $2" >> "{tmp_path}/probed"; exit {0 if answers else 1}')
    env = _env(OBS_FLEET_RESOLVE_CMD=resolve_cmd, OBS_FLEET_STATUS_PROBE_CMD=probe_cmd)
    bash_home = _bash("obs_fleet_is_home", name, env)[1] == 0

    def probe(host, port):
        seen.append((host, port))
        return answers

    py_home = oft.fleet_is_home(name, resolve=lambda h: "10.0.0.9" if resolves else "", probe=probe)
    assert py_home == bash_home
    probed = (tmp_path / "probed").read_text().split() if (tmp_path / "probed").exists() else []
    if seen:
        assert probed == [seen[0][0], str(seen[0][1])]  # the same host and port as bash dials
    else:
        assert probed == []


@pytest.mark.parametrize("resolves, answers", [(True, True), (True, False), (False, True)])
@pytest.mark.parametrize("name", NAMES)
def test_the_python_twin_honours_the_bash_seams_too(clean_env, tmp_path, name, resolves, answers):
    """OBS_FLEET_RESOLVE_CMD / OBS_FLEET_STATUS_PROBE_CMD steer BOTH sides, so an ops or test override
    reads the same in python and bash (review)."""
    resolve_cmd = _stub(tmp_path, "resolve", 'echo 10.0.0.9' if resolves else 'exit 0')
    probe_cmd = _stub(tmp_path, "probe", f'echo "$1 $2" >> "{tmp_path}/probed"; exit {0 if answers else 1}')
    clean_env.setenv("OBS_FLEET_RESOLVE_CMD", resolve_cmd)
    clean_env.setenv("OBS_FLEET_STATUS_PROBE_CMD", probe_cmd)
    env = _env(OBS_FLEET_RESOLVE_CMD=resolve_cmd, OBS_FLEET_STATUS_PROBE_CMD=probe_cmd)
    bash_home = _bash("obs_fleet_is_home", name, env)[1] == 0
    bash_probed = (tmp_path / "probed").read_text() if (tmp_path / "probed").exists() else ""
    (tmp_path / "probed").unlink(missing_ok=True)
    assert oft.fleet_is_home(name) == bash_home
    py_probed = (tmp_path / "probed").read_text() if (tmp_path / "probed").exists() else ""
    assert py_probed == bash_probed


def test_the_traveling_cg_box_is_probed_on_its_obs_ws_port(clean_env):
    seen = []
    assert oft.fleet_is_home("resolume-snv", resolve=lambda h: "10.77.9.201",
                             probe=lambda h, p: seen.append((h, p)) or True)
    assert seen == [("resolume.lan", 4455)]
    clean_env.setenv("OBS_FLEET_HOME_PORT", "4460")
    seen.clear()
    oft.fleet_is_home("resolume-snv", resolve=lambda h: "x", probe=lambda h, p: seen.append(p) or False)
    assert seen == [4460]


def test_the_status_probe_is_a_bounded_tcp_connect(monkeypatch):
    """No live socket (the suite runs in a network-less namespace, where loopback is down too): the
    connect is stubbed, the probe's host / port / timeout and its two answers are what is pinned."""
    import socket

    calls = []

    class _Conn:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

    def ok(addr, timeout):
        calls.append((addr, timeout))
        return _Conn()

    def refused(addr, timeout):
        raise ConnectionRefusedError(111, "refused")

    monkeypatch.setattr(socket, "create_connection", ok)
    assert oft._status_probe("resolume.lan", 4455, 4.0) is True
    assert calls == [(("resolume.lan", 4455), 4.0)]
    monkeypatch.setattr(socket, "create_connection", refused)
    assert oft._status_probe("resolume.lan", 4455, 4.0) is False
