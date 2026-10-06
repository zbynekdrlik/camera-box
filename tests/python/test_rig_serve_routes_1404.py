"""issue 1404 Task 2 -- the rig-lease server's two read-only file routes on dev1 (:8890).

`scripts/rig-lease-server.py` serves two files from its SERVE dir (`scripts/rig_serve_files.py`;
never the lease dir, whose mere existence means held=true):
  GET/HEAD /rig-qpsk-markers.csv  -> cam2's QPSK marker log, text/csv, X-Mirror-Age-S, 404 absent
  GET/HEAD /program-audio.json    -> the program-audio verdict, age_s (+ last_foreign_age_s)
                                     recomputed per request, 404 absent, unreadable = UNKNOWN
while `/rig-lease.json`, `/healthz` and the 404 stay byte-identical to the pre-change server
(golden bytes captured from it at 9e06ea0b3).

A REAL ThreadingHTTPServer on an ephemeral 127.0.0.1 port (never a mock), the harness of
test_rig_lease_server_1277.py.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import pathlib
import socket
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

    def raw(self, method, path):
        """(status line, header lines in wire order without Date, body) -- byte-level view."""
        s = socket.create_connection((self.host, self.port), timeout=5)
        try:
            s.sendall(f"{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".encode())
            data = b""
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                data += chunk
        finally:
            s.close()
        head, _, body = data.partition(b"\r\n\r\n")
        lines = head.decode().split("\r\n")
        return lines[0], [ln for ln in lines[1:] if not ln.startswith("Date:")], body


def _serve_dir(tmp_path):
    d = tmp_path / "serve"
    d.mkdir(mode=0o700)
    return d


def _ts(dt):
    return rsf.format_ts_utc(dt)


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
    """The lease dir's existence IS held=true, so a mirrored file there would fake a held lease."""
    lease = tmp_path / "lease"
    with _Server(lease, str(_serve_dir(tmp_path))) as s:
        status, _h, _b = s.request("GET", "/rig-qpsk-markers.csv")
        _s2, _h2, lease_body = s.request("GET", "/rig-lease.json")
    assert status == 404
    assert json.loads(lease_body)["held"] is False
    assert not lease.exists()


def test_an_unreadable_markers_file_is_a_404_never_a_dropped_connection(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.MARKERS_NAME).mkdir()  # a directory where the file should be: open() raises
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, _h, _b = s.request("GET", "/rig-qpsk-markers.csv")
    assert status == 404


def test_a_markers_file_owned_by_another_user_is_never_served(tmp_path, monkeypatch):
    """Only a file this user wrote is served: another account must not be able to plant one."""
    assert rsf.owned_by_me(os.stat(__file__)) is True
    fake = os.stat_result((0o100644, 0, 0, 1, os.geteuid() + 1, 0, 0, 0, 0, 0))
    assert rsf.owned_by_me(fake) is False
    f = tmp_path / rsf.MARKERS_NAME
    f.write_bytes(MARKERS_CSV)
    assert rsf.read_mirror(str(f), time.time()) is not None
    monkeypatch.setattr(rsf, "owned_by_me", lambda st: False)
    assert rsf.read_mirror(str(f), time.time()) is None


# ---------------------------------------------------------------------------------------------
# the serve dir
# ---------------------------------------------------------------------------------------------


def test_default_serve_dir_is_on_the_user_runtime_tmpfs(monkeypatch):
    monkeypatch.delenv(rsf.SERVE_DIR_ENV, raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/4242")
    assert rsf.default_serve_dir() == "/run/user/4242/rig-lease-serve"
    monkeypatch.delenv("XDG_RUNTIME_DIR")
    assert rsf.default_serve_dir() == f"/run/user/{os.geteuid()}/rig-lease-serve"


def test_default_serve_dir_is_not_inside_the_default_lease_dir(monkeypatch):
    monkeypatch.delenv(rsf.SERVE_DIR_ENV, raising=False)
    monkeypatch.delenv("RIG_LEASE_DIR", raising=False)
    serve = pathlib.PurePosixPath(rsf.default_serve_dir())
    lease = pathlib.PurePosixPath(srv_mod._default_lease_dir())
    assert serve != lease
    assert lease not in serve.parents


def test_serve_dir_env_override(monkeypatch, tmp_path):
    monkeypatch.setenv(rsf.SERVE_DIR_ENV, str(tmp_path))
    assert rsf.default_serve_dir() == str(tmp_path)


def test_one_lease_dir_default_for_the_server_and_the_writers(monkeypatch):
    """An EMPTY RIG_LEASE_DIR means the default everywhere, as in scripts/lib/rig-lease.sh's
    `${RIG_LEASE_DIR:-/var/tmp/rig-lease}` -- never a cwd-relative empty path."""
    monkeypatch.setenv("RIG_LEASE_DIR", "")
    assert srv_mod._default_lease_dir() == rsf.DEFAULT_LEASE_DIR == "/var/tmp/rig-lease"
    assert rsf.default_lease_dir() == "/var/tmp/rig-lease"


def test_serve_dir_problem_refuses_the_lease_dir_and_anything_inside_it(tmp_path):
    lease = tmp_path / "rig-lease"
    assert rsf.serve_dir_problem(str(lease), str(lease))
    assert rsf.serve_dir_problem(str(lease / "serve"), str(lease))
    assert rsf.serve_dir_problem(str(tmp_path / "rig-lease-serve"), str(lease)) is None


def test_serve_dir_problem_refuses_a_dir_others_can_write(tmp_path):
    d = tmp_path / "shared"
    d.mkdir()
    os.chmod(d, 0o777)
    assert "writable" in rsf.serve_dir_problem(str(d), str(tmp_path / "rig-lease"))
    os.chmod(d, 0o700)
    assert rsf.serve_dir_problem(str(d), str(tmp_path / "rig-lease")) is None


def test_ensure_serve_dir_creates_a_private_dir(tmp_path):
    d = tmp_path / "a" / "rig-lease-serve"
    rsf.ensure_serve_dir(str(d), str(tmp_path / "rig-lease"))
    assert d.is_dir()
    assert (d.stat().st_mode & 0o077) == 0


def test_server_main_refuses_a_serve_dir_inside_the_lease_dir(tmp_path):
    """A bounded subprocess: if the refusal ever regresses, the server starts serving and the test
    fails on the timeout instead of hanging the suite."""
    lease = tmp_path / "rig-lease"
    r = subprocess.run(
        [sys.executable, str(_SCRIPTS / "rig-lease-server.py"), "--bind", "127.0.0.1", "--port", "0",
         "--lease-dir", str(lease), "--serve-dir", str(lease / "serve")],
        capture_output=True, text=True, timeout=20,
    )
    assert r.returncode == 2
    assert "held=true" in r.stderr
    assert not lease.exists()


def test_main_wires_the_serve_dir_flag():
    src = (_SCRIPTS / "rig-lease-server.py").read_text(encoding="utf-8")
    assert "--serve-dir" in src
    assert "rsf.default_serve_dir()" in src


# ---------------------------------------------------------------------------------------------
# /program-audio.json
# ---------------------------------------------------------------------------------------------


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
        "source": "STREAM-SNV (stream)", "last_foreign_ts_utc": None,
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
    assert got["last_foreign_age_s"] is None
    for k in ("ts_utc", "verdict", "rms_dbfs", "outside_band_pct", "window_s", "source"):
        assert got[k] == payload[k]
    assert hstatus == 200 and hbody == b"" and hheaders["Content-Type"] == "application/json"


def test_program_audio_last_foreign_age_is_recomputed_too(tmp_path):
    serve = _serve_dir(tmp_path)
    now = datetime.now(timezone.utc)
    payload = {"verdict": "MEASUREMENT", "ts_utc": _ts(now - timedelta(seconds=1)), "age_s": 0.0,
               "last_foreign_ts_utc": _ts(now - timedelta(seconds=7))}
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text(json.dumps(payload), encoding="utf-8")
    with _Server(tmp_path / "lease", str(serve)) as s:
        _status, _h, body = s.request("GET", "/program-audio.json")
    got = json.loads(body)
    assert 6.0 <= got["last_foreign_age_s"] <= 9.0


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


def test_program_audio_os_error_is_served_fail_closed_unknown(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.PROGRAM_AUDIO_NAME).mkdir()  # open() raises IsADirectoryError
    with _Server(tmp_path / "lease", str(serve)) as s:
        status, _h, body = s.request("GET", "/program-audio.json")
    assert status == 200
    got = json.loads(body)
    assert got["verdict"] == "UNKNOWN" and got["age_s"] is None
    assert "unreadable" in got["reason"]


def test_program_audio_unparseable_ts_has_null_age(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text(
        json.dumps({"verdict": "MEASUREMENT", "ts_utc": "yesterday", "age_s": 0.0}), encoding="utf-8")
    with _Server(tmp_path / "lease", str(serve)) as s:
        _status, _h, body = s.request("GET", "/program-audio.json")
    assert json.loads(body)["age_s"] is None


def test_program_audio_from_another_owner_is_served_as_unknown(tmp_path, monkeypatch):
    serve = _serve_dir(tmp_path)
    (serve / rsf.PROGRAM_AUDIO_NAME).write_text(
        json.dumps({"verdict": "MEASUREMENT", "ts_utc": _ts(datetime.now(timezone.utc))}), encoding="utf-8")
    monkeypatch.setattr(rsf, "owned_by_me", lambda st: False)
    got = rsf.program_audio_response(str(serve / rsf.PROGRAM_AUDIO_NAME), datetime.now(timezone.utc))
    assert got["verdict"] == "UNKNOWN"
    assert "owner" in got["reason"]


# ---------------------------------------------------------------------------------------------
# /rig-lease.json, /healthz and the 404 stay byte-identical to the pre-change server
# ---------------------------------------------------------------------------------------------

# Captured from scripts/rig-lease-server.py at 9e06ea0b3 (before issue 1404), free lease dir;
# the Date header is dropped and the lease body's "now" is masked.
_GOLDEN = {
    ("GET", "/healthz"): ("HTTP/1.0 200 OK", ["Server: rig-lease-server/1277 ", "Content-Type: text/plain",
                                              "Content-Length: 2"], b"ok"),
    ("HEAD", "/healthz"): ("HTTP/1.0 200 OK", ["Server: rig-lease-server/1277 ", "Content-Type: text/plain",
                                               "Content-Length: 2"], b""),
    ("GET", "/rig-lease.json"): (
        "HTTP/1.0 200 OK",
        ["Server: rig-lease-server/1277 ", "Content-Type: application/json", "Cache-Control: no-store",
         "Content-Length: 159"],
        b'{"schema": 1, "now": "<now>", "held": false, "holder": null, "heartbeat_age_s": null, '
        b'"stale": null, "expected_release_at": null, "ttl_s": null}'),
    ("HEAD", "/rig-lease.json"): (
        "HTTP/1.0 200 OK",
        ["Server: rig-lease-server/1277 ", "Content-Type: application/json", "Cache-Control: no-store",
         "Content-Length: 159"], b""),
    ("GET", "/nope"): ("HTTP/1.0 404 Not Found", ["Server: rig-lease-server/1277 ", "Content-Type: text/plain",
                                                  "Content-Length: 0"], b""),
    ("HEAD", "/nope"): ("HTTP/1.0 404 Not Found", ["Server: rig-lease-server/1277 ", "Content-Type: text/plain",
                                                   "Content-Length: 0"], b""),
}


def _mask_now(path, body):
    if path == "/rig-lease.json" and body:
        j = json.loads(body)
        assert len(j["now"]) == len("2026-10-06T22:21:44Z")
        j["now"] = "<now>"
        return json.dumps(j).encode()
    return body


def test_existing_routes_match_the_pre_change_golden_bytes(tmp_path):
    serve = _serve_dir(tmp_path)
    (serve / rsf.MARKERS_NAME).write_bytes(MARKERS_CSV)
    with _Server(tmp_path / "lease-absent", use_kw=False) as old_shape, \
            _Server(tmp_path / "lease-absent", str(serve)) as with_serve:
        for srv in (old_shape, with_serve):
            for (method, path), (status, headers, body) in _GOLDEN.items():
                got_status, got_headers, got_body = srv.raw(method, path)
                assert got_status == status, (method, path)
                assert got_headers == headers, (method, path)
                assert _mask_now(path, got_body) == body, (method, path)


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
