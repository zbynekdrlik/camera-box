"""issue 1404 Task 2 -- the cam2 QPSK marker-log mirror on dev1.

`scripts/rig-marker-mirror.sh` (resolves cam2 with `camera_resolve`, the fleet credential) runs
`scripts/rig_marker_mirror.py`: a long-running `--user` service holding ONE ssh connection to cam2
that streams `tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv`. Complete rows are assembled in
memory (a `# qpsk-params` header line = a new painter session = a fresh copy) and written into the
rig-lease server's serve dir by temp + atomic rename, at most every write interval and only when
something changed. ONE login per connection instead of one every 10 s: a cam2 login writes 11
lines into its persistent journal on the USB stick (measured, issue 1404 review).

Tests run the real stream loop against a fake ssh (a python subprocess that scripts the stream),
and the bash entry with a fake `sshpass` + `ssh` first on PATH -- no test ever reaches the rig.
"""
from __future__ import annotations

import os
import pathlib
import re
import stat
import subprocess
import sys
import textwrap

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_marker_mirror as rmm  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

MIRROR = _SCRIPTS / "rig-marker-mirror.sh"
SYSTEMD = _ROOT / "systemd"

HDR_A = b"# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n"
ROWS_A = b"39,39,1791277986683585749\n21,2174485,1791314226995587253\n"
HDR_B = b"# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\nindex,frame_id,emit_ts_ns\n"
ROWS_B = b"0,100,1791400000000000000\n"


def _fleet_default_pass():
    """The fleet credential default the existing fleet scripts use (deploy-fleet.sh), read from
    that script so the mirror is pinned to the SAME default without restating it here."""
    src = (_SCRIPTS / "deploy-fleet.sh").read_text(encoding="utf-8")
    m = re.search(r'^SSH_PASS="\$\{SSH_PASS:-([^}]*)\}"$', src, re.M)
    assert m, "deploy-fleet.sh no longer declares its SSH_PASS default"
    return m.group(1)


# ---------------------------------------------------------------------------------------------
# MarkerLog -- the in-memory copy
# ---------------------------------------------------------------------------------------------


def test_marker_log_keeps_only_complete_rows():
    m = rmm.MarkerLog()
    m.feed(HDR_A + b"1,1,1\n2,2")
    assert m.content() == HDR_A + b"1,1,1\n"
    m.feed(b",2\n")
    assert m.content() == HDR_A + b"1,1,1\n2,2,2\n"


def test_marker_log_starts_a_fresh_copy_at_every_session_header():
    m = rmm.MarkerLog()
    m.feed(HDR_A + ROWS_A)
    m.feed(HDR_B + ROWS_B)
    assert m.content() == HDR_B + ROWS_B
    assert m.sessions == 2


def test_marker_log_drops_bytes_before_the_first_header():
    m = rmm.MarkerLog()
    m.feed(b"7,7,7\n" + HDR_A + ROWS_A)
    assert m.content() == HDR_A + ROWS_A
    assert m.dropped_bytes == len(b"7,7,7\n")


def test_marker_log_is_valid_only_with_the_column_header():
    m = rmm.MarkerLog()
    assert not m.valid()
    m.feed(b"# qpsk-params x\n<html>\n")
    assert not m.valid()
    m2 = rmm.MarkerLog()
    m2.feed(HDR_A)
    assert m2.valid()


def test_marker_log_version_moves_only_on_a_change():
    m = rmm.MarkerLog()
    v0 = m.version
    m.feed(b"")
    m.feed(HDR_A[:5])
    assert m.version == v0
    m.feed(HDR_A[5:])
    assert m.version > v0


# ---------------------------------------------------------------------------------------------
# the remote side + reconnect policy
# ---------------------------------------------------------------------------------------------


def test_remote_command_follows_by_name_and_dies_with_the_connection():
    assert rmm.remote_command("/run/rig-qpsk-markers.csv") == (
        "exec tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv")
    assert rmm.REMOTE_PATH == "/run/rig-qpsk-markers.csv"


def test_ssh_argv_takes_the_password_from_the_environment_never_argv():
    argv = rmm.ssh_argv("10.77.9.62", "/run/rig-qpsk-markers.csv")
    assert argv[:3] == ["sshpass", "-e", "ssh"]
    assert "-p" not in argv[:3]
    assert "root@10.77.9.62" in argv
    assert "ServerAliveInterval=15" in argv
    assert "StrictHostKeyChecking=no" in argv
    assert argv[-1] == rmm.remote_command("/run/rig-qpsk-markers.csv")


def test_reconnect_backoff_doubles_to_the_cap_and_resets_after_a_stable_connection():
    b = rmm.RECONNECT_MIN_S
    seen = []
    for _ in range(8):
        b = rmm.next_backoff(b, lived_s=1.0)
        seen.append(b)
    assert seen[0] == 2 * rmm.RECONNECT_MIN_S
    assert max(seen) == rmm.RECONNECT_MAX_S
    assert rmm.next_backoff(rmm.RECONNECT_MAX_S, lived_s=rmm.STABLE_CONNECTION_S) == rmm.RECONNECT_MIN_S


def test_the_constants_are_pinned():
    assert rmm.WRITE_INTERVAL_S == 10.0
    assert rmm.RECONNECT_MIN_S == 10.0
    assert rmm.RECONNECT_MAX_S == 300.0
    assert rmm.STABLE_CONNECTION_S == 300.0


# ---------------------------------------------------------------------------------------------
# the stream loop, against a fake ssh process
# ---------------------------------------------------------------------------------------------


def _fake_stream(tmp_path, script_body):
    """A python stand-in for `sshpass ssh ... tail -F`: writes scripted chunks to stdout."""
    p = tmp_path / "fake_stream.py"
    p.write_text("import sys, time\nout = sys.stdout.buffer\n" + textwrap.dedent(script_body))
    return [sys.executable, str(p)]


def _run(tmp_path, script_body, *, max_runtime_s=3.0, write_interval_s=0.2, idle_s=0.05, sleeps=None,
         logs=None):
    serve = tmp_path / "serve"
    argv = _fake_stream(tmp_path, script_body)
    writes = []

    def spawn(_host, _path):
        return subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                stdin=subprocess.DEVNULL, start_new_session=True)

    rc = rmm.run("10.77.9.62", str(serve), spawn=spawn, write_interval_s=write_interval_s,
                 idle_s=idle_s, max_runtime_s=max_runtime_s,
                 sleep=(lambda s: sleeps.append(s)) if sleeps is not None else (lambda s: None),
                 log=(logs.append if logs is not None else (lambda m: None)),
                 on_write=writes.append)
    return rc, serve, writes


def test_run_serves_the_streamed_log_by_temp_and_rename(tmp_path):
    body = f"""
    out.write({HDR_A + ROWS_A!r}); out.flush()
    time.sleep(5)
    """
    rc, serve, writes = _run(tmp_path, body, max_runtime_s=1.5)
    assert rc == 0
    f = serve / rsf.MARKERS_NAME
    assert f.read_bytes() == HDR_A + ROWS_A
    assert stat.S_IMODE(f.stat().st_mode) == 0o644
    assert stat.S_IMODE(serve.stat().st_mode) == 0o700
    assert sorted(p.name for p in serve.iterdir()) == [rsf.MARKERS_NAME]
    assert len(writes) == 1


def test_run_follows_appends_at_most_once_per_write_interval(tmp_path):
    body = f"""
    out.write({HDR_A!r}); out.flush()
    for i in range(10):
        time.sleep(0.1)
        out.write(b"%d,%d,%d\\n" % (i, i, i)); out.flush()
    time.sleep(5)
    """
    rc, serve, writes = _run(tmp_path, body, max_runtime_s=2.5, write_interval_s=0.4, idle_s=0.03)
    assert rc == 0
    expected = HDR_A + b"".join(b"%d,%d,%d\n" % (i, i, i) for i in range(10))
    assert (serve / rsf.MARKERS_NAME).read_bytes() == expected
    # ~1 s of appends at a 0.4 s write interval: a few writes, never one per row
    assert 2 <= len(writes) <= 6


def test_run_serves_the_new_session_after_a_painter_restart(tmp_path):
    body = f"""
    out.write({HDR_A + ROWS_A!r}); out.flush()
    time.sleep(0.6)
    out.write({HDR_B + ROWS_B!r}); out.flush()
    time.sleep(5)
    """
    rc, serve, _w = _run(tmp_path, body, max_runtime_s=2.0)
    assert (serve / rsf.MARKERS_NAME).read_bytes() == HDR_B + ROWS_B


def test_run_never_serves_a_half_row(tmp_path):
    body = f"""
    out.write({HDR_A + ROWS_A + b"99,99,17913"!r}); out.flush()
    time.sleep(5)
    """
    _rc, serve, _w = _run(tmp_path, body, max_runtime_s=1.5)
    assert (serve / rsf.MARKERS_NAME).read_bytes() == HDR_A + ROWS_A


def test_run_keeps_the_previous_mirror_and_backs_off_when_ssh_fails(tmp_path):
    serve = tmp_path / "serve"
    serve.mkdir(mode=0o700)
    (serve / rsf.MARKERS_NAME).write_bytes(HDR_A + b"PREVIOUS\n")
    sleeps, logs = [], []
    body = """
    sys.stderr.write("ssh: connect to host 10.77.9.62 port 22: No route to host\\n")
    sys.exit(255)
    """
    _rc, serve, writes = _run(tmp_path, body, max_runtime_s=1.0, sleeps=sleeps, logs=logs)
    assert writes == []
    assert (serve / rsf.MARKERS_NAME).read_bytes() == HDR_A + b"PREVIOUS\n"
    assert sleeps[:3] == [rmm.RECONNECT_MIN_S, 2 * rmm.RECONNECT_MIN_S, 4 * rmm.RECONNECT_MIN_S]
    errors = [m for m in logs if m.startswith("ERROR")]
    assert errors and "No route to host" in errors[0] and "10.77.9.62" in errors[0]


def test_run_logs_the_remote_tail_notes(tmp_path):
    logs = []
    body = f"""
    out.write({HDR_A!r}); out.flush()
    sys.stderr.write("tail: '/run/rig-qpsk-markers.csv' has become inaccessible: No such file or directory\\n")
    sys.stderr.flush()
    time.sleep(5)
    """
    _run(tmp_path, body, max_runtime_s=1.0, logs=logs)
    assert any("has become inaccessible" in m for m in logs)


def test_run_refuses_a_serve_dir_inside_the_lease_dir(tmp_path, monkeypatch):
    lease = tmp_path / "rig-lease"
    monkeypatch.setenv("RIG_LEASE_DIR", str(lease))
    monkeypatch.setenv("SSHPASS", "x")
    rc = rmm.main(["--host", "10.77.9.62", "--serve-dir", str(lease / "serve"), "--max-runtime", "1"])
    assert rc == 2
    assert not lease.exists()


def test_main_refuses_to_run_without_the_password_in_the_environment(tmp_path, monkeypatch):
    monkeypatch.delenv("SSHPASS", raising=False)
    rc = rmm.main(["--host", "10.77.9.62", "--serve-dir", str(tmp_path / "s"), "--max-runtime", "1"])
    assert rc == 2


# ---------------------------------------------------------------------------------------------
# the bash entry: camera_resolve cam2 + the fleet credential, with a fake sshpass + ssh on PATH
# ---------------------------------------------------------------------------------------------


def _fake_bin(tmp_path, ssh_body):
    fb = tmp_path / "fakebin"
    fb.mkdir()
    sshpass = fb / "sshpass"
    sshpass.write_text(
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        '[ "$1" = "-e" ] || { echo "fake sshpass: expected -e" >&2; exit 97; }\n'
        'printf "%s\\n" "${SSHPASS:-}" > "$FAKE_LOG_DIR/sshpass-env"\n'
        'printf "%s\\n" "$@" > "$FAKE_LOG_DIR/sshpass-argv"\n'
        "shift\n"
        'exec "$@"\n'
    )
    ssh = fb / "ssh"
    ssh.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + ssh_body)
    for p in (sshpass, ssh):
        p.chmod(0o755)
    return fb


SSH_OK = (
    'printf "%s\\n" "$@" > "$FAKE_LOG_DIR/ssh-argv"\n'
    'cat "$FAKE_SRC"\n'
    "sleep 30\n"
)


def _run_entry(tmp_path, ssh_body=SSH_OK, extra_env=None, args=("--max-runtime", "2", "--write-interval", "0.2")):
    logs = tmp_path / "logs"
    logs.mkdir(exist_ok=True)
    src = tmp_path / "src.csv"
    src.write_bytes(HDR_A + ROWS_A)
    serve = tmp_path / "serve"
    fb = _fake_bin(tmp_path, ssh_body)
    env = {
        "PATH": f"{fb}:/usr/bin:/bin",
        "HOME": str(tmp_path),
        "FAKE_LOG_DIR": str(logs),
        "FAKE_SRC": str(src),
        "RIG_LEASE_SERVE_DIR": str(serve),
    }
    if extra_env:
        env.update(extra_env)
    r = subprocess.run(["bash", str(MIRROR), *args], env=env, capture_output=True, text=True, timeout=60)
    return r, serve, logs


def test_entry_mirrors_cam2_with_the_fleet_credential(tmp_path):
    r, serve, logs = _run_entry(tmp_path)
    assert r.returncode == 0, r.stderr
    assert (serve / rsf.MARKERS_NAME).read_bytes() == HDR_A + ROWS_A
    argv = (logs / "ssh-argv").read_text().split("\n")
    assert "root@10.77.9.62" in argv
    assert "exec tail -c +1 -F --pid=$PPID /run/rig-qpsk-markers.csv" in argv
    assert (logs / "sshpass-env").read_text().strip() == _fleet_default_pass()
    assert _fleet_default_pass() not in (logs / "sshpass-argv").read_text().split("\n")


def test_entry_uses_the_ssh_pass_override(tmp_path):
    r, _serve, logs = _run_entry(tmp_path, extra_env={"SSH_PASS": "rotated"})
    assert r.returncode == 0, r.stderr
    assert (logs / "sshpass-env").read_text().strip() == "rotated"


def test_entry_refuses_a_serve_dir_inside_the_lease_dir_and_calls_no_ssh(tmp_path):
    lease = tmp_path / "rig-lease"
    r, _serve, logs = _run_entry(tmp_path, extra_env={
        "RIG_LEASE_DIR": str(lease), "RIG_LEASE_SERVE_DIR": str(lease / "serve")})
    assert r.returncode == 2
    assert "lease dir" in r.stderr
    assert not lease.exists()
    assert not (logs / "ssh-argv").exists()


def test_entry_pins_the_painter_camera_and_the_credential_default():
    src = MIRROR.read_text(encoding="utf-8")
    assert 'PAINTER_CAMERA="cam2"' in src
    assert 'camera_resolve "$PAINTER_CAMERA"' in src
    assert f'SSH_PASS="${{SSH_PASS:-{_fleet_default_pass()}}}"' in src
    assert 'SSHPASS="$SSH_PASS" exec python3 "$HERE/rig_marker_mirror.py" --host "$CAMERA_IP"' in src
    assert "set -euo pipefail" in "\n".join(src.splitlines()[:8])


def test_entry_passes_bash_n():
    r = subprocess.run(["bash", "-n", str(MIRROR)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


# ---------------------------------------------------------------------------------------------
# systemd units -- a long-running service, shipped disabled, private env files
# ---------------------------------------------------------------------------------------------


def test_the_mirror_is_a_long_running_service_not_a_timer():
    s = (SYSTEMD / "rig-marker-mirror.service").read_text(encoding="utf-8")
    assert re.search(r"^Type=simple$", s, re.M)
    assert re.search(r"^ExecStart=%h/devel/camera-box/scripts/rig-marker-mirror.sh$", s, re.M)
    assert re.search(r"^Restart=always$", s, re.M)
    assert re.search(r"^WantedBy=default.target$", s, re.M)
    assert not (SYSTEMD / "rig-marker-mirror.timer").exists()


@pytest.mark.parametrize("unit", ["rig-marker-mirror.service", "program-audio-sampler.service"])
def test_the_units_read_a_private_env_file_never_the_global_environment_d(unit):
    s = (SYSTEMD / unit).read_text(encoding="utf-8")
    env_files = re.findall(r"^EnvironmentFile=(.*)$", s, re.M)
    assert env_files, unit
    for e in env_files:
        assert "environment.d" not in e
        assert e.startswith("-%h/.config/camera-box/")


def test_the_new_units_ship_disabled():
    """No provisioning/install script enables them; the supervisor does, on dev1."""
    hits = []
    for p in list(_SCRIPTS.glob("*.sh")) + list((_SCRIPTS / "lib").glob("*.sh")):
        text = p.read_text(encoding="utf-8", errors="replace")
        for unit in ("rig-marker-mirror", "program-audio-sampler"):
            if re.search(rf"enable[^\n]*{unit}", text):
                hits.append(f"{p.name}: {unit}")
    assert hits == []
