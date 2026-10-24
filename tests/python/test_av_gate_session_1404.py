"""Issue 1404 -- scripts/av_gate_session.py, the client of restreamer's YouTube A/V-gate session API.

Every request runs ON the stream box over ssh with the bearer token read in place. Here a fake
`sshpass` runs the remote bash text locally (real bash + real curl) against a fake API server on
127.0.0.1, so the remote script itself is exercised: the token reaches the server as the header and
never appears in an argv or the output.
"""
import http.server
import json
import os
import socket
import subprocess
import sys
import threading
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
CLIENT = REPO / "scripts" / "av_gate_session.py"
FAKE_BEARER = "fake-bearer-of-the-camera-box-test-not-a-real-credential"


class FakeApi:
    """POST session -> self.start; GET session/<id> -> the next of self.states (the last repeats);
    POST session/<id>/stop -> self.stop_code. Records (method, path, authorization) per request."""

    def __init__(self):
        self.start = (201, {"session_id": "s1", "broadcast_id": "b1"})
        self.states = [{"state": "starting"}, {"state": "ready"}]
        self.stop_code = 202
        self.calls = []
        api = self

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def _send(self, code, body):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(n).decode() if n else ""
                api.calls.append(("POST", self.path, self.headers.get("Authorization"), body))
                if self.path.endswith("/stop"):
                    self._send(api.stop_code, {"state": "processing"})
                elif isinstance(api.start, list):  # a sequence of answers, the last repeats
                    self._send(*(api.start.pop(0) if len(api.start) > 1 else api.start[0]))
                else:
                    self._send(*api.start)

            def do_GET(self):
                api.calls.append(("GET", self.path, self.headers.get("Authorization"), ""))
                st = api.states.pop(0) if len(api.states) > 1 else api.states[0]
                self._send(200, dict({"session_id": "s1"}, **st))

        self.srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.srv.server_address[1]}"
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()


@pytest.fixture
def api():
    a = FakeApi()
    yield a
    a.close()


def _box(tmp_path, token=FAKE_BEARER):
    """A fake sshpass that logs its argv + whether SSHPASS is set, then runs the remote text locally."""
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    log = tmp_path / "argv.log"
    fake = bindir / "sshpass"
    fake.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$@" >> "{log}"\n'
        f'[ -n "${{SSHPASS:-}}" ] && echo SSHPASS-SET >> "{log}"\n'
        'exec bash -c "${@: -1}"\n')
    fake.chmod(0o755)
    tok = tmp_path / "api-token"
    if token is not None:
        tok.write_text(token + "\n")
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}", AV_GATE_SSH_PASSWORD="pw")
    return env, tok, log


def _run(env, api_url, tok, *args, timeout=60):
    cmd = [sys.executable, str(CLIENT), "--host", "box", "--user", "u", "--api", api_url,
           "--token-file", str(tok), "--poll", "0.05", *args]
    r = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)
    return r.returncode, json.loads(r.stdout.strip().splitlines()[-1]), r


def test_start_sends_the_bearer_token_from_the_box_and_never_prints_or_argv_it(tmp_path, api):
    env, tok, log = _box(tmp_path)
    rc, out, r = _run(env, api.url, tok, "start", "--requester", "camera-box E2E", "--title", "t")
    assert rc == 0 and out == {"session_id": "s1", "broadcast_id": "b1", "ok": True}, r.stderr
    method, path, auth, body = api.calls[0]
    assert (method, path) == ("POST", "/api/v1/av-gate/session")
    assert auth == f"Bearer {FAKE_BEARER}"
    assert json.loads(body) == {"requester": "camera-box E2E", "title": "t"}
    argv = log.read_text()
    assert FAKE_BEARER not in argv and FAKE_BEARER not in r.stdout and FAKE_BEARER not in r.stderr
    assert "SSHPASS-SET" in argv and "pw" not in argv.split("\n")  # the password goes via env, not argv
    assert "-n" in argv.split("\n") and "UserKnownHostsFile=/dev/null" in argv


@pytest.mark.parametrize("code,body,needle", [
    (409, {"error": "busy", "holder": {"session_id": "x9", "requester": "restreamer-ci"}}, "restreamer-ci"),
    (409, {"error": "cleanup_pending", "sessions": ["a"]}, "cleanup_pending"),
    (429, {"error": "quota"}, "quota"),
    (503, {"error": "not_provisioned", "detail": "no oauth file"}, "not_provisioned: no oauth file"),
    (502, {"session_id": "s2", "state": "failed", "reason": "VPS boot failed"}, "VPS boot failed"),
])
def test_a_refused_start_is_unknown_with_the_reason(tmp_path, api, code, body, needle):
    env, tok, _ = _box(tmp_path)
    api.start = (code, body)
    rc, out, _ = _run(env, api.url, tok, "start", "--requester", "r")
    assert rc == 2 and out["ok"] is False
    assert needle in out["reason"] and f"HTTP {code}" in out["reason"]


def test_a_missing_token_file_names_the_unprovisioned_box(tmp_path, api):
    env, tok, _ = _box(tmp_path, token=None)
    rc, out, _ = _run(env, api.url, tok, "start", "--requester", "r")
    assert rc == 2 and "not provisioned" in out["reason"] and str(tok) in out["reason"]
    assert api.calls == []


def test_an_unreachable_api_is_unknown(tmp_path):
    env, tok, _ = _box(tmp_path)
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    rc, out, _ = _run(env, f"http://127.0.0.1:{port}", tok, "status", "--id", "s1")
    assert rc == 2 and "unreachable" in out["reason"]


def test_wait_ready_polls_until_ready(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.states = [{"state": "starting"}, {"state": "starting"}, {"state": "ready"}]
    rc, out, _ = _run(env, api.url, tok, "wait-ready", "--id", "s1", "--deadline", "30")
    assert rc == 0 and out["state"] == "ready"
    assert [c[1] for c in api.calls] == ["/api/v1/av-gate/session/s1"] * 3


def test_wait_ready_fails_with_restreamers_reason(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.states = [{"state": "starting"}, {"state": "failed", "reason": "transition to live failed"}]
    rc, out, _ = _run(env, api.url, tok, "wait-ready", "--id", "s1", "--deadline", "30")
    assert rc == 2 and "transition to live failed" in out["reason"]


def test_wait_ready_gives_up_at_its_deadline(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.states = [{"state": "starting"}]
    rc, out, _ = _run(env, api.url, tok, "wait-ready", "--id", "s1", "--deadline", "0.3")
    assert rc == 2 and "did not go live within" in out["reason"] and "starting" in out["reason"]


def test_wait_done_returns_the_vod_and_a_stuck_processing_is_unknown(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.states = [{"state": "processing"}, {"state": "done", "vod_id": "AbCdEfGhIjK"}]
    rc, out, _ = _run(env, api.url, tok, "wait-done", "--id", "s1", "--deadline", "30")
    assert rc == 0 and out["vod_id"] == "AbCdEfGhIjK"
    api.states = [{"state": "processing"}]
    rc, out, _ = _run(env, api.url, tok, "wait-done", "--id", "s1", "--deadline", "0.3")
    assert rc == 2 and "VOD was not processed within" in out["reason"]


def test_stop_accepts_202_and_200_and_refuses_409(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    for code in (202, 200):
        api.stop_code = code
        rc, out, _ = _run(env, api.url, tok, "stop", "--id", "s1")
        assert rc == 0 and out["http"] == code
    api.stop_code = 409
    rc, out, _ = _run(env, api.url, tok, "stop", "--id", "s1")
    assert rc == 2 and "stop refused" in out["reason"]
    assert all(c[1] == "/api/v1/av-gate/session/s1/stop" for c in api.calls)


def test_a_quote_in_the_token_reaches_the_server_intact(tmp_path, api):
    env, tok, _ = _box(tmp_path, token='fake"bearer\\with-a-quote-and-a-backslash')
    rc, _, r = _run(env, api.url, tok, "status", "--id", "s1")
    assert rc == 0, r.stdout
    assert api.calls[0][2] == 'Bearer fake"bearer\\with-a-quote-and-a-backslash'


def test_a_starting_up_restreamer_is_retried_then_started(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.start = [(503, {"error": "starting_up"}), (503, {"error": "starting_up"}),
                 (201, {"session_id": "s9", "broadcast_id": "b9"})]
    rc, out, _ = _run(env, api.url, tok, "start", "--requester", "r")
    assert rc == 0 and out["session_id"] == "s9"
    assert sum(1 for c in api.calls if c[0] == "POST") == 3


def test_a_busy_refusal_names_the_holders_session(tmp_path, api):
    env, tok, _ = _box(tmp_path)
    api.start = (409, {"error": "busy", "holder": {"session_id": "old1", "requester": "camera-box E2E"}})
    rc, out, _ = _run(env, api.url, tok, "start", "--requester", "r")
    assert rc == 2 and out["holder_session_id"] == "old1"
