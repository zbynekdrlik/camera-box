"""issue 1404 Task 2 -- the dev1 read-only endpoints next to the rig lease (:8890).

* `scripts/rig-lease-server.py` serves two mirrored files from its SERVE dir (never the lease dir:
  the lease dir's mere existence means held=true):
    GET/HEAD /rig-qpsk-markers.csv  -> the cam2 QPSK marker log, text/csv, X-Mirror-Age-S, 404 absent
    GET/HEAD /program-audio.json    -> the program-audio verdict, age_s recomputed per request,
                                       404 absent
  while `/rig-lease.json` and `/healthz` stay byte-identical.
* `scripts/rig-marker-mirror.sh` -- one pass: scp cam2's `/run/rig-qpsk-markers.csv`
  (`camera_resolve cam2`, the fleet credential) into the serve dir via temp + atomic rename,
  failing loud and leaving the previous mirror untouched on any error.

A REAL ThreadingHTTPServer on an ephemeral 127.0.0.1 port (never a mock), the same harness as
test_rig_lease_server_1277.py. The mirror runs with a fake `sshpass` + `scp` first on PATH, so no
test ever reaches the rig.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_serve_files as rsf  # noqa: E402


def _load_server_module():
    spec = importlib.util.spec_from_file_location("rig_lease_server_1404", _SCRIPTS / "rig-lease-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


srv_mod = _load_server_module()

MARKERS_CSV = (
    b"# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1\n"
    b"index,frame_id,emit_ts_ns\n"
    b"39,39,1791277986683585749\n"
    b"21,2174485,1791314226995587253\n"
)


class _Server:
    def __init__(self, lease_dir, serve_dir=None, use_kw=True):
        if use_kw:
            self.server = srv_mod.make_server("127.0.0.1", 0, str(lease_dir), 5400, serve_dir=serve_dir)
        else:
            self.server = srv_mod.make_server("127.0.0.1", 0, str(lease_dir), 5400)
        self.host, self.port = self.server.server_address
        self._thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()
        self._thread.join(timeout=5)

    def request(self, method, path):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=5)
        try:
            conn.request(method, path)
            resp = conn.getresponse()
            return resp.status, dict(resp.getheaders()), resp.read()
        finally:
            conn.close()


def _serve_dir(tmp_path):
    d = tmp_path / "serve"
    d.mkdir()
    return d


# ---------------------------------------------------------------------------------------------
# /rig-qpsk-markers.csv
# ---------------------------------------------------------------------------------------------


def test_markers_route_is_404_while_the_mirror_is_absent(tmp_path):
    with _Server(tmp_path / "lease", str(_serve_dir(tmp_path))) as s:
        status, _h, _b = s.request("GET", "/rig-qpsk-markers.csv")
        hstatus, _h2, hbody = s.request("HEAD", "/rig-qpsk-markers.csv")
    assert status == 404
    assert hstatus == 404 and hbody == b""


def test_markers_route_serves_the_exact_bytes_with_the_mirror_age(tmp_path):
    serve = _serve_dir(tmp_path)
    f = serve / rsf.MARKERS_NAME
    f.write_bytes(MARKERS_CSV)
    past = time.time() - 37
    os.utime(f, (past, past))
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, headers, body = s.request("GET", "/rig-qpsk-markers.csv?t=1")
    assert status == 200
    assert body == MARKERS_CSV
    assert headers["Content-Type"] == "text/csv"
    assert headers["Cache-Control"] == "no-store"
    assert headers["Content-Length"] == str(len(MARKERS_CSV))
    age = int(headers["X-Mirror-Age-S"])
    assert 36 <= age <= 40


def test_markers_head_carries_the_headers_and_no_body(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.MARKERS_NAME).write_bytes(MARKERS_CSV)
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, headers, body = s.request("HEAD", "/rig-qpsk-markers.csv")
    assert status == 200
    assert body == b""
    assert headers["Content-Type"] == "text/csv"
    assert headers["Content-Length"] == str(len(MARKERS_CSV))
    assert "X-Mirror-Age-S" in headers


def test_markers_route_never_reads_the_lease_dir(tmp_path):
    """The lease dir's existence IS held=true, so a mirrored file there would fake a held lease.
    A markers file placed in the lease dir must not be served, and the lease must stay free."""
    lease = tmp_path / "lease"
    with _Server(lease, str(_serve_dir(tmp_path))) as s:
        status, _h, _b = s.request("GET", "/rig-qpsk-markers.csv")
        _s2, _h2, lease_body = s.request("GET", "/rig-lease.json")
    assert status == 404
    assert json.loads(lease_body)["held"] is False
    assert not lease.exists()


def test_default_serve_dir_is_not_inside_the_default_lease_dir(monkeypatch):
    monkeypatch.delenv(rsf.SERVE_DIR_ENV, raising=False)
    monkeypatch.delenv("RIG_LEASE_DIR", raising=False)
    serve = pathlib.PurePosixPath(rsf.default_serve_dir())
    lease = pathlib.PurePosixPath(srv_mod._default_lease_dir())
    assert serve != lease
    assert lease not in serve.parents
    assert rsf.DEFAULT_SERVE_DIR == "/var/tmp/rig-lease-serve"


def test_serve_dir_env_override(monkeypatch, tmp_path):
    monkeypatch.setenv(rsf.SERVE_DIR_ENV, str(tmp_path))
    assert rsf.default_serve_dir() == str(tmp_path)


# ---------------------------------------------------------------------------------------------
# /program-audio.json
# ---------------------------------------------------------------------------------------------


def _ts(dt):
    return rsf.format_ts_utc(dt)


def test_program_audio_route_is_404_while_absent(tmp_path):
    with _Server(tmp_path / "lease", str(_serve_dir(tmp_path))) as s:
        status, _h, _b = s.request("GET", "/program-audio.json")
    assert status == 404


def test_program_audio_age_is_recomputed_at_request_time(tmp_path):
    """The sampler writes age_s 0; the server recomputes it from ts_utc so a consumer on another
    host never compares two clocks and a stopped sampler reads stale, never fresh."""
    serve = _serve_dir(tmp_path)
    payload = {
        "schema": 1, "ts_utc": _ts(datetime.now(timezone.utc) - timedelta(seconds=30)), "age_s": 0.0,
        "verdict": "MEASUREMENT", "rms_dbfs": -35.6, "outside_band_pct": 16.8, "window_s": 2.0,
        "source": "STREAM-SNV (stream)",
    }
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text(json.dumps(payload), encoding="utf-8")
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, headers, body = s.request("GET", "/program-audio.json")
        hstatus, hheaders, hbody = s.request("HEAD", "/program-audio.json")
    assert status == 200
    assert headers["Content-Type"] == "application/json"
    assert headers["Cache-Control"] == "no-store"
    got = json.loads(body)
    assert 29.0 <= got["age_s"] <= 33.0
    for k in ("ts_utc", "verdict", "rms_dbfs", "outside_band_pct", "window_s", "source"):
        assert got[k] == payload[k]
    assert hstatus == 200 and hbody == b"" and hheaders["Content-Type"] == "application/json"


def test_program_audio_unreadable_file_is_served_fail_closed_unknown(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text("{not json", encoding="utf-8")
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, _h, body = s.request("GET", "/program-audio.json")
    assert status == 200
    got = json.loads(body)
    assert got["verdict"] == "UNKNOWN"
    assert got["age_s"] is None
    assert "unreadable" in got["reason"]


def test_program_audio_unparseable_ts_has_null_age(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text(
        json.dumps({"verdict": "MEASUREMENT", "ts_utc": "yesterday", "age_s": 0.0}), encoding="utf-8")
    with _Server(tmp_path / "lease", str(serve)) as s:
        _status, _h, body = s.request("GET", "/program-audio.json")
    assert json.loads(body)["age_s"] is None


# ---------------------------------------------------------------------------------------------
# /rig-lease.json + /healthz stay byte-identical
# ---------------------------------------------------------------------------------------------


def _strip_volatile(headers):
    return {k: v for k, v in headers.items() if k != "Date"}


def test_existing_routes_are_byte_identical_with_and_without_a_serve_dir(tmp_path):
    lease = tmp_path / "lease"
    lease.mkdir()
    holder = {"repo": "zbynekdrlik/camera-box", "run_id": "1", "run_url": "x", "job": "x",
              "acquired_at": "2026-09-02T11:00:00Z", "expected_release_at": "2099-01-01T00:00:00Z"}
    (lease / "holder.json").write_text(json.dumps(holder), encoding="utf-8")
    (lease / "heartbeat").write_text("", encoding="utf-8")
    serve = _serve_dir(tmp_path)
    (serve / rsf.MARKERS_NAME).write_bytes(MARKERS_CSV)
    with _Server(lease, use_kw=False) as old, _Server(lease, str(serve)) as new:
        for method in ("GET", "HEAD"):
            for path in ("/healthz", "/rig-lease.json", "/nope"):
                a = old.request(method, path)
                b = new.request(method, path)
                assert a[0] == b[0], (method, path)
                assert _strip_volatile(a[1]) == _strip_volatile(b[1]), (method, path)
                if path == "/rig-lease.json" and method == "GET":
                    ja, jb = json.loads(a[2]), json.loads(b[2])
                    ja.pop("now"), jb.pop("now")
                    ja.pop("heartbeat_age_s"), jb.pop("heartbeat_age_s")
                    ja.pop("ttl_s"), jb.pop("ttl_s")
                    assert ja == jb
                    assert list(json.loads(a[2]).keys()) == list(json.loads(b[2]).keys())
                else:
                    assert a[2] == b[2], (method, path)


def test_mirror_routes_are_404_when_no_serve_dir_is_configured(tmp_path):
    with _Server(tmp_path / "lease", use_kw=False) as s:
        assert s.request("GET", "/rig-qpsk-markers.csv")[0] == 404
        assert s.request("GET", "/program-audio.json")[0] == 404


def test_post_to_a_mirror_route_is_never_a_write(tmp_path):
    serve = _serve_dir(tmp_path)
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, _h, _b = s.request("POST", "/program-audio.json")
    assert status == 501
    assert list(serve.iterdir()) == []


def test_main_wires_the_serve_dir_flag():
    src = (_SCRIPTS / "rig-lease-server.py").read_text(encoding="utf-8")
    assert "--serve-dir" in src
    assert "rsf.default_serve_dir()" in src


# ---------------------------------------------------------------------------------------------
# scripts/rig-marker-mirror.sh
# ---------------------------------------------------------------------------------------------

MIRROR = _SCRIPTS / "rig-marker-mirror.sh"


def _fleet_default_pass():
    """The fleet credential default the existing fleet scripts use (deploy-fleet.sh), read from
    that script so the mirror is pinned to the SAME default without restating it here."""
    src = (_SCRIPTS / "deploy-fleet.sh").read_text(encoding="utf-8")
    m = re.search(r'^SSH_PASS="\$\{SSH_PASS:-([^}]*)\}"$', src, re.M)
    assert m, "deploy-fleet.sh no longer declares its SSH_PASS default"
    return m.group(1)


def _fake_bin(tmp_path, scp_body):
    fb = tmp_path / "fakebin"
    fb.mkdir()
    sshpass = fb / "sshpass"
    sshpass.write_text(
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        '[ "$1" = "-p" ] || { echo "fake sshpass: expected -p" >&2; exit 97; }\n'
        'printf "%s\\n" "$2" > "$FAKE_LOG_DIR/sshpass-pw"\n'
        "shift 2\n"
        'exec "$@"\n'
    )
    scp = fb / "scp"
    scp.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + scp_body)
    for p in (sshpass, scp):
        p.chmod(0o755)
    return fb


SCP_OK = (
    'printf "%s\\n" "$@" > "$FAKE_LOG_DIR/scp-argv"\n'
    'dest="${@: -1}"\n'
    'printf "%s\\n" "$dest" > "$FAKE_LOG_DIR/scp-dest"\n'
    'cat "$FAKE_SRC" > "$dest"\n'
)


def _run_mirror(tmp_path, scp_body=SCP_OK, src_bytes=MARKERS_CSV, extra_env=None):
    logs = tmp_path / "logs"
    logs.mkdir(exist_ok=True)
    src = tmp_path / "src.csv"
    src.write_bytes(src_bytes)
    serve = tmp_path / "serve"
    fb = _fake_bin(tmp_path, scp_body) if not (tmp_path / "fakebin").exists() else tmp_path / "fakebin"
    env = {
        "PATH": f"{fb}:/usr/bin:/bin",
        "HOME": str(tmp_path),
        "FAKE_LOG_DIR": str(logs),
        "FAKE_SRC": str(src),
        "RIG_LEASE_SERVE_DIR": str(serve),
    }
    if extra_env:
        env.update(extra_env)
    r = subprocess.run(["bash", str(MIRROR)], env=env, capture_output=True, text=True, timeout=60)
    return r, serve, logs


def test_mirror_copies_cam2_marker_log_into_the_serve_dir(tmp_path):
    r, serve, logs = _run_mirror(tmp_path)
    assert r.returncode == 0, r.stderr
    out = serve / rsf.MARKERS_NAME
    assert out.read_bytes() == MARKERS_CSV
    assert stat.S_IMODE(out.stat().st_mode) == 0o644
    argv = (logs / "scp-argv").read_text().split("\n")
    assert "root@10.77.9.62:/run/rig-qpsk-markers.csv" in argv
    assert "StrictHostKeyChecking=no" in argv
    assert (logs / "sshpass-pw").read_text().strip() == _fleet_default_pass()
    # never a leftover temp file
    assert sorted(p.name for p in serve.iterdir()) == [rsf.MARKERS_NAME]


def test_mirror_writes_a_temp_file_then_renames_it(tmp_path):
    r, serve, logs = _run_mirror(tmp_path)
    assert r.returncode == 0, r.stderr
    dest = pathlib.Path((logs / "scp-dest").read_text().strip())
    assert dest.parent == serve
    assert dest.name != rsf.MARKERS_NAME
    assert not dest.exists()


def test_mirror_replaces_the_file_atomically_so_an_open_reader_keeps_the_old_bytes(tmp_path):
    serve = tmp_path / "serve"
    serve.mkdir()
    old = serve / rsf.MARKERS_NAME
    old.write_bytes(b"index,frame_id,emit_ts_ns\nOLD\n")
    with open(old, "rb") as reader:
        r, _serve, _logs = _run_mirror(tmp_path)
        assert r.returncode == 0, r.stderr
        assert reader.read() == b"index,frame_id,emit_ts_ns\nOLD\n"
    assert (serve / rsf.MARKERS_NAME).read_bytes() == MARKERS_CSV


def test_mirror_uses_the_ssh_pass_override(tmp_path):
    r, _serve, logs = _run_mirror(tmp_path, extra_env={"SSH_PASS": "rotated"})
    assert r.returncode == 0, r.stderr
    assert (logs / "sshpass-pw").read_text().strip() == "rotated"


def test_mirror_fails_loud_and_keeps_the_previous_mirror_when_scp_fails(tmp_path):
    serve = tmp_path / "serve"
    serve.mkdir()
    (serve / rsf.MARKERS_NAME).write_bytes(b"index,frame_id,emit_ts_ns\nPREVIOUS\n")
    body = 'dest="${@: -1}"\nprintf "partial" > "$dest"\necho "scp: /run/rig-qpsk-markers.csv: No such file" >&2\nexit 1\n'
    r, serve, _logs = _run_mirror(tmp_path, scp_body=body)
    assert r.returncode != 0
    assert "ERROR" in r.stderr
    assert "10.77.9.62" in r.stderr
    assert (serve / rsf.MARKERS_NAME).read_bytes() == b"index,frame_id,emit_ts_ns\nPREVIOUS\n"
    assert sorted(p.name for p in serve.iterdir()) == [rsf.MARKERS_NAME]


def test_mirror_refuses_a_file_without_the_marker_header(tmp_path):
    serve = tmp_path / "serve"
    serve.mkdir()
    (serve / rsf.MARKERS_NAME).write_bytes(b"index,frame_id,emit_ts_ns\nPREVIOUS\n")
    r, serve, _logs = _run_mirror(tmp_path, src_bytes=b"<html>not a marker log</html>\n")
    assert r.returncode != 0
    assert "ERROR" in r.stderr
    assert (serve / rsf.MARKERS_NAME).read_bytes() == b"index,frame_id,emit_ts_ns\nPREVIOUS\n"
    assert sorted(p.name for p in serve.iterdir()) == [rsf.MARKERS_NAME]


def test_mirror_refuses_an_empty_file(tmp_path):
    r, serve, _logs = _run_mirror(tmp_path, src_bytes=b"")
    assert r.returncode != 0
    assert not (serve / rsf.MARKERS_NAME).exists()


def test_mirror_bounds_the_scp_with_timeout_inside_sshpass():
    src = MIRROR.read_text(encoding="utf-8")
    assert 'sshpass -p "$SSH_PASS" timeout ' in src
    assert "set -euo pipefail" in "\n".join(src.splitlines()[:8])


def test_mirror_defaults_are_pinned_to_the_shared_constants():
    src = MIRROR.read_text(encoding="utf-8")
    assert f'RIG_LEASE_SERVE_DIR:-{rsf.DEFAULT_SERVE_DIR}' in src
    assert f'MIRROR_NAME="{rsf.MARKERS_NAME}"' in src
    assert 'REMOTE_PATH="/run/rig-qpsk-markers.csv"' in src
    assert 'camera_resolve "$PAINTER_CAMERA"' in src
    assert 'PAINTER_CAMERA="cam2"' in src
    assert f'SSH_PASS="${{SSH_PASS:-{_fleet_default_pass()}}}"' in src


def test_mirror_script_passes_bash_n():
    r = subprocess.run(["bash", "-n", str(MIRROR)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


# ---------------------------------------------------------------------------------------------
# systemd units -- shipped disabled, 10 s cadence
# ---------------------------------------------------------------------------------------------

SYSTEMD = _ROOT / "systemd"


def test_mirror_timer_fires_every_10_s_with_1_s_accuracy():
    t = (SYSTEMD / "rig-marker-mirror.timer").read_text(encoding="utf-8")
    assert re.search(r"^OnUnitActiveSec=10s$", t, re.M)
    assert re.search(r"^AccuracySec=1s$", t, re.M)
    assert re.search(r"^WantedBy=timers.target$", t, re.M)


def test_mirror_service_runs_the_script_as_a_oneshot():
    s = (SYSTEMD / "rig-marker-mirror.service").read_text(encoding="utf-8")
    assert re.search(r"^Type=oneshot$", s, re.M)
    assert re.search(r"^ExecStart=%h/devel/camera-box/scripts/rig-marker-mirror.sh$", s, re.M)
    assert re.search(r"^TimeoutStartSec=\d+$", s, re.M)


def test_the_new_units_ship_disabled():
    """No provisioning/install script enables them; the supervisor does, on dev1."""
    hits = []
    for p in list(_SCRIPTS.glob("*.sh")) + list((_SCRIPTS / "lib").glob("*.sh")):
        text = p.read_text(encoding="utf-8", errors="replace")
        for unit in ("rig-marker-mirror", "program-audio-sampler"):
            if re.search(rf"enable[^\n]*{unit}", text):
                hits.append(f"{p.name}: {unit}")
    assert hits == []
