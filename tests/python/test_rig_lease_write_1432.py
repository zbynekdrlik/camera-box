"""issue 1432 -- the authenticated rig-lease WRITE routes on dev1's rig-lease-server (:8890).

restreamer's OBS-streaming CI runs on the stream box and could only READ the lease, so a 45-min CI
stream at night (14.10.2026) was invisible to camera-box as a holder. These tests run the REAL
ThreadingHTTPServer on an ephemeral 127.0.0.1 port against a temp lease dir, with the REAL bash
lib (scripts/lib/rig-lease.sh) doing every lock operation, and drive it with http.client.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import pathlib
import secrets
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone

ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = ROOT / "scripts"
LIB = _SCRIPTS / "lib" / "rig-lease.sh"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_lease_state as rls  # noqa: E402

# A fresh random bearer per test session: the real token never appears in the repo.
TOKEN = secrets.token_urlsafe(24)


def _load_server_module():
    spec = importlib.util.spec_from_file_location("rig_lease_server_1432", _SCRIPTS / "rig-lease-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


srv_mod = _load_server_module()


class _Server:
    def __init__(self, lease_dir, token_file):
        self.server = srv_mod.make_server("127.0.0.1", 0, str(lease_dir), 5400, token_file=str(token_file))
        self.host, self.port = self.server.server_address
        self._t = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()
        self._t.join(timeout=5)

    def post(self, path, body, token=TOKEN, raw=None):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=30)
        try:
            data = raw if raw is not None else json.dumps(body).encode()
            headers = {"Content-Type": "application/json"}
            if token is not None:
                headers["Authorization"] = f"Bearer {token}"
            conn.request("POST", path, body=data, headers=headers)
            resp = conn.getresponse()
            text = resp.read()
            try:
                payload = json.loads(text) if text else None
            except ValueError:
                payload = None
            return resp.status, payload
        finally:
            conn.close()

    def get(self, path):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=10)
        try:
            conn.request("GET", path)
            resp = conn.getresponse()
            return resp.status, resp.read()
        finally:
            conn.close()


def _token_file(tmp_path, value=TOKEN):
    p = tmp_path / "rig-lease-token"
    p.write_text(value + "\n")
    p.chmod(0o600)
    return p


def _acquire_body(**over):
    body = {
        "repo": "zbynekdrlik/restreamer",
        "run_id": "38632386935",
        "run_url": "https://github.com/zbynekdrlik/restreamer/actions/runs/38632386935",
        "job": "e2e-obs-youtube-test",
        "expected_secs": 2700,
    }
    body.update(over)
    return body


def _holder(lease_dir):
    return json.loads((lease_dir / "holder.json").read_text())


def _bash(lease_dir, script, *args):
    env = dict(os.environ, RIG_LEASE_DIR=str(lease_dir))
    return subprocess.run(
        ["bash", "-c", 'set -uo pipefail; . "$0"; ' + script, str(LIB), *args],
        capture_output=True, text=True, env=env, timeout=60,
    )


# --- auth ---------------------------------------------------------------------------------------

def test_post_without_token_is_401_and_writes_nothing(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        status, _ = s.post("/rig-lease/acquire", _acquire_body(), token=None)
    assert status == 401
    assert not lease.exists()


def test_post_with_wrong_token_is_401(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        status, _ = s.post("/rig-lease/acquire", _acquire_body(), token="wrong")
    assert status == 401
    assert not lease.exists()


def test_no_token_file_means_every_write_is_refused(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, tmp_path / "absent-token") as s:
        status, _ = s.post("/rig-lease/acquire", _acquire_body(), token="")
        status2, _ = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 401 and status2 == 401
    assert not lease.exists()


def test_a_token_file_readable_by_others_is_refused(tmp_path):
    lease = tmp_path / "rig-lease"
    tf = _token_file(tmp_path)
    tf.chmod(0o644)
    with _Server(lease, tf) as s:
        status, _ = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 401
    assert not lease.exists()


# --- acquire ------------------------------------------------------------------------------------

def test_acquire_takes_the_lease_and_records_the_remote_holder(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 200, payload
    assert payload["acquired"] is True
    assert len(payload["lease_id"]) >= 32
    assert payload["holder"]["repo"] == "zbynekdrlik/restreamer"
    h = _holder(lease)
    assert h["lease_id"] == payload["lease_id"]
    assert h["stale_secs"] == 180
    assert h["max_hold_s"] == 2700
    assert h["run_id"] == "38632386935"
    # declared release = the rolling look-ahead (<= 900 s), never the whole run
    rel = rls.parse_ts(h["expected_release_at"])
    acq = rls.parse_ts(h["acquired_at"])
    assert 0 < (rel - acq).total_seconds() <= 905
    assert payload["expected_release_at"] == h["expected_release_at"]


def test_a_short_run_declares_its_own_release(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, payload = s.post("/rig-lease/acquire", _acquire_body(expected_secs=120))
    h = _holder(lease)
    rel = rls.parse_ts(h["expected_release_at"])
    acq = rls.parse_ts(h["acquired_at"])
    assert 115 <= (rel - acq).total_seconds() <= 125


def test_second_acquire_is_409_naming_the_live_holder(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, first = s.post("/rig-lease/acquire", _acquire_body())
        status, payload = s.post("/rig-lease/acquire", _acquire_body(run_id="999"))
    assert status == 409
    assert payload["acquired"] is False
    assert payload["holder"]["run_id"] == "38632386935"
    assert isinstance(payload["heartbeat_age_s"], int)
    assert "lease_id" not in json.dumps(payload)
    assert _holder(lease)["lease_id"] == first["lease_id"]


def test_acquire_is_409_while_camera_box_holds_the_rig(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _bash(lease, 'rig_lease_acquire "$@"', "zbynekdrlik/camera-box", "555", "https://x", "full-path-e2e",
              "2099-01-01T00:00:00Z")
    assert "RIG_LEASE_ACQUIRED" in r.stdout, r.stderr
    with _Server(lease, _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 409
    assert payload["holder"]["repo"] == "zbynekdrlik/camera-box"
    assert _holder(lease)["run_id"] == "555"


def test_bad_input_is_400_and_writes_nothing(tmp_path):
    lease = tmp_path / "rig-lease"
    bad = [
        _acquire_body(expected_secs=59),
        _acquire_body(expected_secs=5401),
        _acquire_body(expected_secs="2700"),
        _acquire_body(expected_secs=True),
        _acquire_body(repo="bad repo; rm -rf /"),
        _acquire_body(run_id=""),
        _acquire_body(run_id="1$(touch x)"),
        _acquire_body(run_url="file:///etc/passwd"),
        _acquire_body(job="x" * 101),
        {k: v for k, v in _acquire_body().items() if k != "job"},
    ]
    with _Server(lease, _token_file(tmp_path)) as s:
        for body in bad:
            status, _ = s.post("/rig-lease/acquire", body)
            assert status == 400, body
        status, _ = s.post("/rig-lease/acquire", None, raw=b"not json")
        assert status == 400
        status, _ = s.post("/rig-lease/acquire", None, raw=b"[1,2]")
        assert status == 400
        status, _ = s.post("/rig-lease/acquire", None, raw=b"{" + b" " * 9000 + b"}")
        assert status == 400
    assert not lease.exists()
    assert not (pathlib.Path.cwd() / "x").exists()


def test_unknown_write_path_is_404(tmp_path):
    with _Server(tmp_path / "rig-lease", _token_file(tmp_path)) as s:
        status, _ = s.post("/rig-lease/steal", {"lease_id": "x"})
    assert status == 404


# --- heartbeat ----------------------------------------------------------------------------------

def test_heartbeat_rolls_the_release_and_bumps_the_heartbeat(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, acq = s.post("/rig-lease/acquire", _acquire_body(expected_secs=120))
        old = os.stat(lease / "heartbeat").st_mtime
        os.utime(lease / "heartbeat", (old - 100, old - 100))
        status, payload = s.post("/rig-lease/heartbeat", {"lease_id": acq["lease_id"]})
    assert status == 200, payload
    assert payload["refreshed"] is True
    h = _holder(lease)
    rel = rls.parse_ts(h["expected_release_at"])
    assert (rel - rls.parse_ts(acq["expected_release_at"])).total_seconds() > 600
    assert payload["expected_release_at"] == h["expected_release_at"]
    assert os.stat(lease / "heartbeat").st_mtime > old - 50


def test_heartbeat_with_a_foreign_lease_id_is_404_and_changes_nothing(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        s.post("/rig-lease/acquire", _acquire_body())
        before = (lease / "holder.json").read_text()
        status, payload = s.post("/rig-lease/heartbeat", {"lease_id": "0" * 32})
    assert status == 404
    assert payload["error"] == "not_holder"
    assert (lease / "holder.json").read_text() == before


def test_heartbeat_with_no_lease_is_404(tmp_path):
    with _Server(tmp_path / "rig-lease", _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/heartbeat", {"lease_id": "a" * 32})
    assert status == 404


def test_heartbeat_past_the_hold_ceiling_is_refused(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, acq = s.post("/rig-lease/acquire", _acquire_body(expected_secs=60))
        h = _holder(lease)
        h["acquired_at"] = "2020-01-01T00:00:00Z"
        (lease / "holder.json").write_text(json.dumps(h))
        status, payload = s.post("/rig-lease/heartbeat", {"lease_id": acq["lease_id"]})
    assert status == 409
    assert payload["error"] == "over_hold"


# --- release ------------------------------------------------------------------------------------

def test_release_frees_the_rig_and_is_idempotent(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, acq = s.post("/rig-lease/acquire", _acquire_body())
        status, payload = s.post("/rig-lease/release", {"lease_id": acq["lease_id"]})
        assert status == 200 and payload["released"] is True
        assert not lease.exists()
        status, payload = s.post("/rig-lease/release", {"lease_id": acq["lease_id"]})
    assert status == 200 and payload["released"] is False


def test_release_with_a_foreign_lease_id_never_removes_the_holder(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _bash(lease, 'rig_lease_acquire "$@"', "zbynekdrlik/camera-box", "555", "https://x", "full-path-e2e",
              "2099-01-01T00:00:00Z")
    assert "RIG_LEASE_ACQUIRED" in r.stdout
    with _Server(lease, _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/release", {"lease_id": "b" * 32})
    assert status == 200 and payload["released"] is False
    assert _holder(lease)["run_id"] == "555"


# --- staleness: a remote holder is reclaimable after 180 s --------------------------------------

def test_a_silent_remote_holder_is_reclaimed_by_the_camera_box_gate_after_180s(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        s.post("/rig-lease/acquire", _acquire_body())
    hb = lease / "heartbeat"
    t = time.time() - 200
    os.utime(hb, (t, t))
    r = _bash(lease, 'rig_lease_acquire "$@"', "zbynekdrlik/camera-box", "777", "https://x", "full-path-e2e",
              "2099-01-01T00:00:00Z", "5400")
    assert "RIG_LEASE_RECLAIMED" in r.stdout, (r.stdout, r.stderr)
    assert _holder(lease)["run_id"] == "777"


def test_a_beating_remote_holder_is_not_reclaimed(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        s.post("/rig-lease/acquire", _acquire_body())
    t = time.time() - 60
    os.utime(lease / "heartbeat", (t, t))
    r = _bash(lease, 'rig_lease_acquire "$@"', "zbynekdrlik/camera-box", "777", "https://x", "full-path-e2e",
              "2099-01-01T00:00:00Z", "5400")
    assert "RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#38632386935" in r.stdout, (r.stdout, r.stderr)


def test_the_json_view_grades_the_holders_own_stale_secs_and_hides_the_lease_id(tmp_path):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token_file(tmp_path)) as s:
        _, acq = s.post("/rig-lease/acquire", _acquire_body())
        t = time.time() - 200
        os.utime(lease / "heartbeat", (t, t))
        status, body = s.get("/rig-lease.json")
    assert status == 200
    state = json.loads(body)
    assert state["held"] is True and state["stale"] is True
    assert acq["lease_id"] not in body.decode()


def test_a_local_holder_keeps_the_callers_threshold(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _bash(lease, 'rig_lease_acquire "$@"', "zbynekdrlik/camera-box", "555", "https://x", "full-path-e2e",
              "2099-01-01T00:00:00Z")
    assert "RIG_LEASE_ACQUIRED" in r.stdout
    h = _holder(lease)
    assert set(h) == {"repo", "run_id", "run_url", "job", "acquired_at", "expected_release_at"}
    t = time.time() - 200
    os.utime(lease / "heartbeat", (t, t))
    r = _bash(lease, 'rig_lease_is_stale 5400; echo rc=$?')
    assert "rc=1" in r.stdout


def test_a_garbage_stale_secs_falls_back_to_the_callers_threshold(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    h = {"repo": "r", "run_id": "1", "run_url": "u", "job": "j", "acquired_at": "2099-01-01T00:00:00Z",
         "expected_release_at": "2099-01-01T00:00:00Z", "stale_secs": "1; rm -rf /"}
    (lease / "holder.json").write_text(json.dumps(h))
    (lease / "heartbeat").touch()
    t = time.time() - 200
    os.utime(lease / "heartbeat", (t, t))
    r = _bash(lease, 'rig_lease_is_stale 5400; echo rc=$?')
    assert "rc=1" in r.stdout
    state = rls.lease_state(str(lease), datetime.now(timezone.utc), 5400)
    assert state["stale"] is False


# --- review round 1 (issue 1432) ----------------------------------------------------------------

def test_an_acquire_never_steals_a_lockdir_another_acquirer_just_made(tmp_path):
    """A camera-box gate that has just done its mkdir has a lockdir with no holder.json for a few
    ms. The HTTP acquire must answer 409, never reclaim it (review: it returned 200 reclaimed)."""
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    with _Server(lease, _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 409, payload
    assert not (lease / "holder.json").exists()


def test_a_holderless_lockdir_past_the_grace_is_reclaimed(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    t = time.time() - (rls.NO_HOLDER_GRACE_SECS + 5)
    os.utime(lease, (t, t))
    with _Server(lease, _token_file(tmp_path)) as s:
        status, payload = s.post("/rig-lease/acquire", _acquire_body())
    assert status == 200, payload
    assert payload["reclaimed"] is True


def test_a_string_stale_secs_is_ignored_by_bash_like_python(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    h = {"repo": "r", "run_id": "1", "run_url": "u", "job": "j", "acquired_at": "2099-01-01T00:00:00Z",
         "expected_release_at": "2099-01-01T00:00:00Z", "stale_secs": "180"}
    (lease / "holder.json").write_text(json.dumps(h))
    (lease / "heartbeat").touch()
    t = time.time() - 200
    os.utime(lease / "heartbeat", (t, t))
    r = _bash(lease, 'rig_lease_is_stale 5400; echo rc=$?')
    assert "rc=1" in r.stdout, r.stdout
    assert rls.lease_state(str(lease), datetime.now(timezone.utc), 5400)["stale"] is False


def test_release_never_removes_a_holder_with_the_same_run_id_but_another_lease_id(tmp_path):
    lease = tmp_path / "rig-lease"
    r = _bash(lease, 'rig_lease_release "$@"; echo rc=$?', "555", "a" * 32)
    assert "rc=0" in r.stdout  # no lease: a no-op
    with _Server(lease, _token_file(tmp_path)) as s:
        _, acq = s.post("/rig-lease/acquire", _acquire_body(run_id="555"))
    r = _bash(lease, 'rig_lease_release "$@"', "555", "b" * 32)
    assert _holder(lease)["lease_id"] == acq["lease_id"], r.stderr
    r = _bash(lease, 'rig_lease_release "$@"', "555", acq["lease_id"])
    assert not lease.exists(), r.stderr


def test_handle_answers_503_instead_of_raising(tmp_path, monkeypatch):
    import rig_lease_write as rlw
    def boom(*_a, **_k):
        raise OSError("disk gone")
    monkeypatch.setattr(rlw, "read_token", boom)
    reply = rlw.handle("/rig-lease/acquire", "Bearer x", b"{}", str(tmp_path / "l"), 5400,
                       str(tmp_path / "t"))
    assert reply.status == 503 and reply.payload["error"] == "lease_io"


def test_the_no_holder_grace_is_one_number_in_bash_and_python():
    text = LIB.read_text()
    assert f'RIG_LEASE_NOHOLDER_GRACE_SECS:-{rls.NO_HOLDER_GRACE_SECS}' in text


def test_the_python_grace_honours_the_same_env_override_as_bash(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    t = time.time() - 20
    os.utime(lease, (t, t))
    code = ("import sys; sys.path.insert(0, sys.argv[1]); import rig_lease_state as r; "
            "from datetime import datetime, timezone; "
            "print(r.lease_state(sys.argv[2], datetime.now(timezone.utc), 5400)['stale'])")
    out = subprocess.run([sys.executable, "-c", code, str(_SCRIPTS), str(lease)], capture_output=True,
                         text=True, env=dict(os.environ, RIG_LEASE_NOHOLDER_GRACE_SECS="60")).stdout
    assert out.strip() == "False"
    r = _bash(lease, 'rig_lease_is_stale 5400; echo rc=$?')
    assert "rc=0" in r.stdout  # bash default grace 10 s: 20 s old = stale
