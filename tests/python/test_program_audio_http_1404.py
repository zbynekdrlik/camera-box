"""issue 1404 -- the program-audio sampler serves its OWN read-only HTTP endpoint (host-agnostic).

The owner moves the sampler off dev1 to a rig node (still being chosen), so it can no longer rely on
dev1's rig-lease server (:8890). The sampler process now serves `/program-audio.json` itself:
  * GET/HEAD `/program-audio.json` -> the payload through the SAME `rig_serve_files.
    program_audio_response` the lease server uses (ages recomputed per request, a foreign-owned or
    unreadable file = UNKNOWN, a MEASUREMENT without a marker chain = UNKNOWN), 404 while absent;
  * `/healthz` -> ok; anything else 404; any other method 501 (read-only);
  * port `--http-port` / $PROGRAM_AUDIO_HTTP_PORT, default 8891 (0 = no endpoint), bind
    `--http-bind` / $PROGRAM_AUDIO_HTTP_BIND, default 0.0.0.0, serve dir `--serve-dir` /
    $PROGRAM_AUDIO_SERVE_DIR (else the old default);
  * the response code is ONE handler base shared with the lease server (rig_serve_files), and the
    dev1 lease route keeps working byte-identically (test_rig_serve_routes_1404.py goldens).

A REAL ThreadingHTTPServer on an ephemeral 127.0.0.1 port, never a mock.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import pathlib
import signal
import socket
import sys
import threading
import time
from datetime import datetime, timedelta, timezone

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for _p in (str(_SCRIPTS), str(_HERE)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import program_audio as pa  # noqa: E402
import program_audio_capture as pac  # noqa: E402
import program_audio_http as pah  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402
from qpsk_guard_shim_1404 import build_shim  # noqa: E402
from test_program_audio_capture_1404 import _MainRx  # noqa: E402


def _load_lease_server():
    spec = importlib.util.spec_from_file_location("rig_lease_server_http_1404", _SCRIPTS / "rig-lease-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.fixture(scope="session")
def shim_path(tmp_path_factory):
    return build_shim(tmp_path_factory.mktemp("qpsk-guard-shim-http"))


class _Http:
    def __init__(self, serve_dir):
        self.server = pah.make_server("127.0.0.1", 0, str(serve_dir))
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


def _serve(tmp_path):
    d = tmp_path / "serve"
    d.mkdir(mode=0o700)
    return d


def _write(serve, verdict="MEASUREMENT", chain=7, now=None, **kw):
    now = now or datetime.now(timezone.utc)
    pa.write_payload(str(serve), pa.build_payload(verdict, -35.6, 16.8, now=now, window_s=2.0, source="S",
                                                  marker_chain=chain, **kw))


def test_the_endpoint_serves_the_payload_with_its_age_recomputed(tmp_path):
    serve = _serve(tmp_path)
    _write(serve, now=datetime.now(timezone.utc) - timedelta(seconds=3))
    with _Http(serve) as h:
        status, headers, body = h.request("GET", "/program-audio.json")
        q_status, _qh, q_body = h.request("GET", "/program-audio.json?t=1")
    assert status == 200 and q_status == 200
    assert headers["Content-Type"] == "application/json"
    assert headers["Cache-Control"] == "no-store"
    j = json.loads(body)
    assert j["verdict"] == "MEASUREMENT" and j["marker_chain"] == 7
    assert 2.5 <= j["age_s"] <= 10.0
    assert json.loads(q_body)["verdict"] == "MEASUREMENT"


def test_head_has_the_headers_and_no_body(tmp_path):
    serve = _serve(tmp_path)
    _write(serve)
    with _Http(serve) as h:
        g_status, g_headers, _g = h.request("GET", "/program-audio.json")
        status, headers, body = h.request("HEAD", "/program-audio.json")
    assert status == g_status == 200 and body == b""
    assert headers["Content-Type"] == g_headers["Content-Type"] == "application/json"


def test_absent_is_404_and_healthz_is_ok_and_anything_else_404(tmp_path):
    serve = _serve(tmp_path)
    with _Http(serve) as h:
        assert h.request("GET", "/program-audio.json")[0] == 404
        health = h.request("GET", "/healthz")
        assert health[0] == 200 and health[2] == b"ok"
        assert h.request("GET", "/rig-lease.json")[0] == 404           # not the lease server
        assert h.request("GET", "/rig-qpsk-markers.csv")[0] == 404
        assert h.request("GET", "/")[0] == 404


def test_it_is_read_only(tmp_path):
    serve = _serve(tmp_path)
    with _Http(serve) as h:
        for method in ("POST", "PUT", "DELETE"):
            assert h.request(method, "/program-audio.json")[0] == 501
    assert list(serve.iterdir()) == []


def test_the_shared_payload_rules_apply(tmp_path, monkeypatch):
    """The endpoint serves through rig_serve_files.program_audio_response: a MEASUREMENT without a
    marker chain is UNKNOWN, a file another user owns is UNKNOWN, garbage is UNKNOWN."""
    serve = _serve(tmp_path)
    with _Http(serve) as h:
        _write(serve, chain=None)
        assert json.loads(h.request("GET", "/program-audio.json")[2])["verdict"] == "UNKNOWN"
        _write(serve)
        monkeypatch.setattr(rsf, "owned_by_me", lambda st: False)
        j = json.loads(h.request("GET", "/program-audio.json")[2])
        assert j["verdict"] == "UNKNOWN" and "owner" in j["reason"]
        monkeypatch.undo()
        (serve / rsf.PROGRAM_AUDIO_NAME).write_bytes(b"not json")
        assert json.loads(h.request("GET", "/program-audio.json")[2])["verdict"] == "UNKNOWN"


def test_an_idle_client_is_dropped_after_the_request_timeout(tmp_path):
    """A client that connects and sends nothing must not hold a server thread forever: the endpoint
    listens on 0.0.0.0 on a production box with no firewall (review round 3)."""
    assert rsf.ReadOnlyHandler.timeout == 10
    serve = _serve(tmp_path)
    server = pah.make_server("127.0.0.1", 0, str(serve), timeout=0.5)
    host, port = server.server_address
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    try:
        s = socket.create_connection((host, port), timeout=5)
        t0 = time.monotonic()
        assert s.recv(1024) == b""               # the server closed the idle connection
        assert time.monotonic() - t0 < 4.0
        s.close()
    finally:
        server.shutdown()
        server.server_close()
        t.join(5)


def test_the_server_header_names_the_sampler_and_hides_the_python_version(tmp_path):
    serve = _serve(tmp_path)
    with _Http(serve) as h:
        _status, headers, _body = h.request("GET", "/healthz")
    assert headers["Server"].startswith("program-audio-sampler/1404")
    assert "Python" not in headers["Server"]


def test_one_response_code_for_both_servers():
    """The sampler's endpoint and the dev1 lease server share ONE read-only handler base, so the
    response framing (headers, HEAD, the query string, a client that hangs up) cannot drift."""
    srv = _load_lease_server()
    assert issubclass(pah.ProgramAudioHandler, rsf.ReadOnlyHandler)
    assert issubclass(srv.RigLeaseHandler, rsf.ReadOnlyHandler)
    for name in ("_send", "_request_path", "do_GET", "do_HEAD"):
        assert name not in vars(pah.ProgramAudioHandler), name
        assert name not in vars(srv.RigLeaseHandler), name


# ---------------------------------------------------------------------------------------------
# the sampler's CLI: port, bind, serve dir; the endpoint's lifetime is the sampler's
# ---------------------------------------------------------------------------------------------


def test_the_cli_defaults_and_env_overrides(monkeypatch, tmp_path):
    for var in (pas.HTTP_PORT_ENV, pas.HTTP_BIND_ENV, pas.SERVE_DIR_ENV):
        monkeypatch.delenv(var, raising=False)
    args = pas.build_parser().parse_args([])
    assert args.http_port == pas.DEFAULT_HTTP_PORT == 8891
    assert args.http_bind == "0.0.0.0"
    assert args.serve_dir == rsf.default_serve_dir()
    monkeypatch.setenv(pas.HTTP_PORT_ENV, "18899")
    monkeypatch.setenv(pas.HTTP_BIND_ENV, "127.0.0.1")
    monkeypatch.setenv(pas.SERVE_DIR_ENV, str(tmp_path / "s"))
    args = pas.build_parser().parse_args([])
    assert (args.http_port, args.http_bind, args.serve_dir) == (18899, "127.0.0.1", str(tmp_path / "s"))
    args = pas.build_parser().parse_args(["--http-port", "0", "--serve-dir", str(tmp_path / "t")])
    assert args.http_port == 0 and args.serve_dir == str(tmp_path / "t")


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _get(port, path="/program-audio.json"):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        conn.request("GET", path)
        resp = conn.getresponse()
        return resp.status, resp.read()
    finally:
        conn.close()


def test_the_sampler_serves_while_it_runs_and_stops_serving_when_it_stops(tmp_path, monkeypatch, shim_path):
    monkeypatch.setattr(pan, "NdiAudioReceiver", _MainRx)
    _MainRx.instances.clear()
    port = _free_port()
    seen = {"errors": []}
    before = (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGINT))
    done = threading.Event()

    def probe_then_term():
        end = time.monotonic() + 20.0
        while not done.is_set() and time.monotonic() < end:
            try:
                status, body = _get(port)
            except OSError as exc:  # not listening yet: keep the reason, try again
                seen["errors"].append(repr(exc))
                time.sleep(0.05)
                continue
            if status == 200:
                seen["payload"] = json.loads(body)
                seen["health"] = _get(port, "/healthz")
                os.kill(os.getpid(), signal.SIGTERM)
                return
            time.sleep(0.05)

    t = threading.Thread(target=probe_then_term, daemon=True)
    t.start()
    try:
        rc = pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "S", "--marker-shim", shim_path,
                       "--http-bind", "127.0.0.1", "--http-port", str(port)])
    finally:
        done.set()
        t.join(5)
        signal.signal(signal.SIGTERM, before[0])
        signal.signal(signal.SIGINT, before[1])
    assert rc == 0
    assert "payload" in seen, seen["errors"][-3:]
    assert seen["payload"]["source"] == "S" and seen["payload"]["verdict"] in pa.VERDICTS
    assert seen["health"] == (200, b"ok")
    with pytest.raises(OSError):
        _get(port)                                    # the endpoint went down with the sampler
    assert pac.CAPTURE_THREAD_NAME not in {th.name for th in threading.enumerate()}
    assert pah.HTTP_THREAD_NAME not in {th.name for th in threading.enumerate()}


def test_a_port_in_use_fails_loud_and_leaves_unknown(tmp_path, monkeypatch, shim_path):
    monkeypatch.setattr(pan, "NdiAudioReceiver", _MainRx)
    _MainRx.instances.clear()
    busy = socket.socket()
    busy.bind(("127.0.0.1", 0))
    busy.listen(1)
    lines = []
    monkeypatch.setattr(pas, "log", lines.append)
    try:
        rc = pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "S", "--marker-shim", shim_path,
                       "--http-bind", "127.0.0.1", "--http-port", str(busy.getsockname()[1])])
    finally:
        busy.close()
    assert rc == 1
    assert _MainRx.instances and _MainRx.instances[-1].closed_during_capture is False  # closed again
    j = json.loads((tmp_path / "serve" / rsf.PROGRAM_AUDIO_NAME).read_text(encoding="utf-8"))
    assert j["verdict"] == "UNKNOWN" and "cannot serve" in j["reason"]
    assert any("FATAL" in line and "http" in line for line in lines), lines


def test_port_0_runs_without_an_endpoint(tmp_path, monkeypatch, shim_path):
    monkeypatch.setattr(pan, "NdiAudioReceiver", _MainRx)
    _MainRx.instances.clear()
    made = []
    monkeypatch.setattr(pah, "make_server", lambda *a, **k: made.append(a))
    before = (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGINT))
    done = threading.Event()

    def term_once_capturing():
        end = time.monotonic() + 20.0
        while not done.is_set() and time.monotonic() < end:
            if _MainRx.instances and _MainRx.instances[-1].i > 10:
                os.kill(os.getpid(), signal.SIGTERM)
                return
            time.sleep(0.05)

    t = threading.Thread(target=term_once_capturing, daemon=True)
    t.start()
    try:
        rc = pas.main(["--serve-dir", str(tmp_path / "serve"), "--source", "S", "--marker-shim", shim_path,
                       "--http-port", "0"])
    finally:
        done.set()
        t.join(5)
        signal.signal(signal.SIGTERM, before[0])
        signal.signal(signal.SIGINT, before[1])
    assert rc == 0 and made == []
