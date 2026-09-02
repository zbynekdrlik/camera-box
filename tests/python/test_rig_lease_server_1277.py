"""#1277 -- integration test for scripts/rig-lease-server.py: a REAL ThreadingHTTPServer bound to
an ephemeral port on 127.0.0.1, exercised via http.client (never a mock) over
`GET /rig-lease.json`, `GET /healthz`, and an unknown path (404). This is the "restreamer's
stream-box runner polls this over LAN HTTP" contract proven end-to-end, not just the pure
rig_lease_state.lease_state() decision (see test_rig_lease_state_1277.py for that).

Module name has a hyphen (`scripts/rig-lease-server.py`), so it is loaded via importlib from its
file path rather than a plain `import` statement -- the same pattern this repo already needs for
any hyphenated script filename.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import pathlib
import sys
import threading

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))


def _load_server_module():
    spec = importlib.util.spec_from_file_location("rig_lease_server", _SCRIPTS / "rig-lease-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


rls_server = _load_server_module()


class _RunningServer:
    """Starts scripts/rig-lease-server.py's real ThreadingHTTPServer in a background thread on an
    ephemeral 127.0.0.1 port, and tears it down cleanly."""

    def __init__(self, lease_dir: str, stale_secs: int = 5400):
        self.server = rls_server.make_server("127.0.0.1", 0, lease_dir, stale_secs)
        self.host, self.port = self.server.server_address
        self._thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()
        self._thread.join(timeout=5)

    def get(self, path: str):
        return self.request("GET", path)

    def request(self, method: str, path: str):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=5)
        try:
            conn.request(method, path)
            resp = conn.getresponse()
            body = resp.read()
            return resp.status, dict(resp.getheaders()), body
        finally:
            conn.close()


def test_rig_lease_json_free_dir_returns_held_false(tmp_path):
    with _RunningServer(str(tmp_path / "does-not-exist")) as srv:
        status, headers, body = srv.get("/rig-lease.json")

    assert status == 200
    assert headers.get("Content-Type") == "application/json"
    assert headers.get("Cache-Control") == "no-store"
    payload = json.loads(body)
    assert payload["held"] is False
    assert payload["schema"] == 1


def test_rig_lease_json_held_dir_returns_full_shape(tmp_path):
    lease_dir = tmp_path / "rig-lease"
    lease_dir.mkdir()
    holder = {
        "repo": "zbynekdrlik/camera-box",
        "run_id": "999",
        "run_url": "https://example.invalid/999",
        "job": "full-path-e2e",
        "acquired_at": "2026-09-02T11:00:00Z",
        "expected_release_at": "2099-01-01T00:00:00Z",
    }
    (lease_dir / "holder.json").write_text(json.dumps(holder), encoding="utf-8")
    (lease_dir / "heartbeat").write_text("", encoding="utf-8")

    with _RunningServer(str(lease_dir)) as srv:
        status, headers, body = srv.get("/rig-lease.json")

    assert status == 200
    assert headers.get("Cache-Control") == "no-store"
    payload = json.loads(body)
    assert payload["held"] is True
    assert payload["stale"] is False
    assert payload["holder"]["repo"] == "zbynekdrlik/camera-box"
    assert payload["ttl_s"] is not None and payload["ttl_s"] > 0


def test_healthz_returns_200_ok(tmp_path):
    with _RunningServer(str(tmp_path / "does-not-exist")) as srv:
        status, _headers, body = srv.get("/healthz")
    assert status == 200
    assert body == b"ok"


def test_unknown_path_returns_404(tmp_path):
    with _RunningServer(str(tmp_path / "does-not-exist")) as srv:
        status, _headers, _body = srv.get("/nope")
    assert status == 404


def test_lease_state_is_computed_fresh_per_request_never_cached(tmp_path):
    """The ticket's own hard requirement: 'nikdy statický snapshot z timeru -- race okno'. Two
    requests bracketing a lockdir being created must observe the transition, proving there is no
    per-process caching of the computed state."""
    lease_dir = tmp_path / "rig-lease"

    with _RunningServer(str(lease_dir)) as srv:
        status1, _h1, body1 = srv.get("/rig-lease.json")
        assert json.loads(body1)["held"] is False

        lease_dir.mkdir()
        holder = {
            "repo": "zbynekdrlik/camera-box", "run_id": "1", "run_url": "x",
            "job": "x", "acquired_at": "2026-09-02T11:00:00Z",
            "expected_release_at": "2099-01-01T00:00:00Z",
        }
        (lease_dir / "holder.json").write_text(json.dumps(holder), encoding="utf-8")
        (lease_dir / "heartbeat").write_text("", encoding="utf-8")

        status2, _h2, body2 = srv.get("/rig-lease.json")
        assert json.loads(body2)["held"] is True


def test_query_string_is_stripped_before_route_matching(tmp_path):
    """A client-side cache-buster (`?t=1`) must still hit the real route -- a naive exact-path
    match would 404 this and silently make restreamer's consumer contract fail-open."""
    with _RunningServer(str(tmp_path / "does-not-exist")) as srv:
        status, _headers, body = srv.get("/rig-lease.json?t=1")
    assert status == 200
    assert json.loads(body)["held"] is False


def test_post_method_is_never_a_write_and_never_a_5xx(tmp_path):
    """This server implements no do_POST -- BaseHTTPRequestHandler's stdlib default (501 Not
    Implemented) is the honest behavior for an unsupported verb, never an application-level 500,
    and (per the module's own GET-only contract) never any state mutation."""
    lease_dir = tmp_path / "does-not-exist"
    with _RunningServer(str(lease_dir)) as srv:
        status, _headers, _body = srv.request("POST", "/rig-lease.json")
        assert status == 501
        # confirm no write side effect occurred -- the lockdir must still be genuinely absent
        assert not lease_dir.exists()


def test_head_rig_lease_json_returns_headers_no_body(tmp_path):
    lease_dir = tmp_path / "does-not-exist"
    with _RunningServer(str(lease_dir)) as srv:
        status, headers, body = srv.request("HEAD", "/rig-lease.json")
    assert status == 200
    assert headers.get("Content-Type") == "application/json"
    assert headers.get("Cache-Control") == "no-store"
    assert body == b""


def test_server_response_header_does_not_leak_python_version(tmp_path):
    with _RunningServer(str(tmp_path / "does-not-exist")) as srv:
        _status, headers, _body = srv.get("/healthz")
    server_header = headers.get("Server", "")
    assert "rig-lease-server" in server_header
    # BaseHTTPRequestHandler's default Server header is "<server_version> Python/<version>" --
    # sys_version="" on RigLeaseHandler must remove the trailing Python/<version> segment.
    assert "Python/" not in server_header
