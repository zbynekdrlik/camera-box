"""issue 1450 slice A -- the :8890 view tells a reader WHICH boxes the lease holds.

GET /rig-lease.json keeps `schema: 1` and every v1 key, first and unchanged, and APPENDS
`lease_model: 2`, a per-atom `resources` map and the `groups`. A holder.json without `resources` (every
holder before issue 1450, and every holder that declares nothing) covers ALL atoms.
GET /rig-lease.json?resources=<csv> answers the v1 shape for those atoms only (+ `scope`), so
SongPlayer (`?resources=resolume-snv`) no longer waits for a camera-box soak on the SNV cameras. An
unknown scope answers the whole-rig view + `scope_error`: fail-closed, never a 400 (a consumer that
gets an error fails OPEN). POST /rig-lease/acquire takes `resources` as a list or a CSV string.

The real ThreadingHTTPServer on an ephemeral 127.0.0.1 port and the real bash lib, on temp lease dirs.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import pathlib
import secrets
import sys
import threading
from datetime import datetime, timedelta, timezone

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import rig_lease_state as rls  # noqa: E402

ATOMS = ["snv-cams", "strih-snv", "stream-snv", "imag", "resolume-snv", "pp-cams", "strih-pp", "stream-pp",
         "resolume-pp"]
SNV_VIDEO = ["snv-cams", "strih-snv", "stream-snv", "imag"]
V1_KEYS = ["schema", "now", "held", "holder", "heartbeat_age_s", "stale", "expected_release_at", "ttl_s"]
NOW = datetime(2026, 10, 17, 3, 0, 0, tzinfo=timezone.utc)
TOKEN = secrets.token_urlsafe(24)


def _load_server():
    spec = importlib.util.spec_from_file_location("rig_lease_server_1450", SCRIPTS / "rig-lease-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


srv_mod = _load_server()


def _seed(lease, resources="absent", age_s=5, **over):
    lease.mkdir()
    h = {"repo": "camera-box-av-soak", "run_id": "av-soak-1", "run_url": "", "job": "av-soak",
         "acquired_at": "2026-10-17T01:27:00Z", "expected_release_at": "2026-10-17T10:00:00Z"}
    if resources != "absent":
        h["resources"] = resources
    h.update(over)
    (lease / "holder.json").write_text(json.dumps(h))
    hb = lease / "heartbeat"
    hb.touch()
    t = NOW.timestamp() - age_s
    os.utime(hb, (t, t))
    return h


def _view(lease, scope=None):
    return rls.lease_view(str(lease), NOW, 5400, scope=scope)


# --- the whole-rig view ------------------------------------------------------------------------------

def test_schema_stays_1_restreamer_fails_open_on_any_other():
    """restreamer's reader treats `schema != 1` as "unknown format -> proceed" (scripts/ci/
    rig-lease-wait.ps1:88, Get-RigLeaseHolder in obs-ws.ps1:160): a bump would silently switch its
    guard off. The box-scoped data is APPENDED instead (`lease_model`)."""
    assert rls.SCHEMA_VERSION == 1
    assert rls.LEASE_MODEL == 2


def test_a_free_rig_appends_the_model_resources_and_groups_after_the_v1_keys(tmp_path):
    v = _view(tmp_path / "rig-lease")
    assert list(v)[:8] == V1_KEYS
    assert list(v)[8:] == ["lease_model", "resources", "groups"]
    assert {k: v[k] for k in V1_KEYS} == rls.lease_state(str(tmp_path / "rig-lease"), NOW, 5400)
    assert list(v["resources"]) == ATOMS
    assert all(e == {"held": False, "via": None, "holder": None, "declared": None, "heartbeat_age_s": None,
                     "stale": None, "expected_release_at": None, "ttl_s": None} for e in v["resources"].values())
    assert v["groups"]["snv-video"] == SNV_VIDEO and v["groups"]["all"] == ATOMS


def test_a_scoped_holder_holds_only_its_atoms(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease, SNV_VIDEO)
    v = _view(lease)
    assert v["held"] is True and v["holder"]["repo"] == "camera-box-av-soak"
    assert {k: v[k] for k in V1_KEYS} == rls.lease_state(str(lease), NOW, 5400)
    cams = v["resources"]["snv-cams"]
    assert cams["held"] is True and cams["via"] == "scoped" and cams["declared"] == SNV_VIDEO
    assert cams["holder"] == v["holder"] and cams["ttl_s"] == v["ttl_s"] == 25200
    assert cams["heartbeat_age_s"] == v["heartbeat_age_s"] and cams["stale"] is False
    assert [a for a, e in v["resources"].items() if e["held"]] == SNV_VIDEO
    assert v["resources"]["resolume-snv"]["held"] is False


def test_a_holder_without_resources_holds_every_atom(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease)
    v = _view(lease)
    assert all(e["held"] and e["via"] == "legacy" and e["declared"] is None for e in v["resources"].values())


@pytest.mark.parametrize("bad", [[], "snv-cams", ["snv-cams", "nope"], [1], {"a": 1}, None])
def test_an_unreadable_declaration_holds_every_atom(tmp_path, bad):
    lease = tmp_path / "rig-lease"
    _seed(lease, bad)
    assert all(e["held"] for e in _view(lease)["resources"].values())


def test_a_corrupt_or_missing_holder_json_holds_every_atom(tmp_path):
    lease = tmp_path / "rig-lease"
    lease.mkdir()
    (lease / "holder.json").write_text("{not json")
    assert all(e["held"] for e in _view(lease)["resources"].values())
    (lease / "holder.json").unlink()  # an acquire in progress (inside the no-holder grace)
    assert all(e["held"] for e in rls.lease_view(str(lease), datetime.now(timezone.utc), 5400)["resources"].values())


def test_a_holder_naming_a_group_is_resolved(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease, ["snv"])
    held = [a for a, e in _view(lease)["resources"].items() if e["held"]]
    assert held == SNV_VIDEO + ["resolume-snv"]


def test_the_v1_holder_never_leaks_the_declaration_or_the_lease_id(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease, ["snv-cams"], lease_id="0" * 32)
    v = _view(lease)
    assert set(v["holder"]) == set(rls._HOLDER_FIELDS)
    assert "0" * 32 not in json.dumps(v)


# --- the scoped view (?resources=) -------------------------------------------------------------------

def test_a_scope_the_holder_does_not_touch_reads_free(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease, SNV_VIDEO)
    v = _view(lease, "resolume-snv")
    assert list(v)[:8] == V1_KEYS
    assert v["held"] is False and all(v[k] is None for k in V1_KEYS[3:])
    assert v["schema"] == 1 and v["now"] == "2026-10-17T03:00:00Z"
    assert v["scope"] == ["resolume-snv"] and list(v["resources"]) == ["resolume-snv"]
    assert "scope_error" not in v


def test_a_scope_inside_the_holders_set_reads_the_holder(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease, SNV_VIDEO)
    whole = rls.lease_state(str(lease), NOW, 5400)
    for scope in ("stream-snv", "snv", "resolume-snv,imag", " imag ,, "):
        v = _view(lease, scope)
        assert {k: v[k] for k in V1_KEYS} == whole, scope


def test_an_undeclared_holder_blocks_every_scope(tmp_path):
    lease = tmp_path / "rig-lease"
    _seed(lease)
    assert _view(lease, "resolume-snv")["held"] is True
    assert _view(lease, "pp")["held"] is True


@pytest.mark.parametrize("scope", ["resolume", "resolume-snv,nope", "", " , "])
def test_an_unknown_or_empty_scope_answers_the_whole_rig_with_an_error(tmp_path, scope):
    lease = tmp_path / "rig-lease"
    _seed(lease, SNV_VIDEO)
    v = _view(lease, scope)
    assert v["held"] is True and v["holder"]["repo"] == "camera-box-av-soak"
    assert "scope" not in v and v["scope_error"]
    assert list(v["resources"]) == ATOMS


def test_a_broken_table_fails_closed(tmp_path, monkeypatch):
    bad = tmp_path / "t.txt"
    bad.write_text("atom|a|x\natom|a|y\n")
    monkeypatch.setenv("RIG_LEASE_RESOURCES_TABLE", str(bad))
    with pytest.raises(Exception):
        _view(tmp_path / "rig-lease")
    v = rls.fail_closed_view(NOW, "resolume-snv")
    assert v["schema"] == 1 and v["held"] is True and v["holder"] is None
    assert v["lease_model"] == 2 and v["scope_error"]


# --- the server ---------------------------------------------------------------------------------------

class _Server:
    def __init__(self, lease, token_file=None):
        self.server = srv_mod.make_server("127.0.0.1", 0, str(lease), 5400,
                                          token_file=str(token_file) if token_file else None)
        self.host, self.port = self.server.server_address
        self._t = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self.server.shutdown()
        self.server.server_close()
        self._t.join(timeout=5)

    def call(self, method, path, body=None):
        conn = http.client.HTTPConnection(self.host, self.port, timeout=30)
        try:
            headers = {}
            if body is not None:
                headers = {"Content-Type": "application/json", "Authorization": f"Bearer {TOKEN}"}
            conn.request(method, path, body=None if body is None else json.dumps(body).encode(), headers=headers)
            resp = conn.getresponse()
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
        finally:
            conn.close()


def _live_seed(lease, resources):
    """A holder whose heartbeat is fresh in REAL time (the server reads the wall clock)."""
    now = datetime.now(timezone.utc)
    lease.mkdir()
    (lease / "holder.json").write_text(json.dumps({
        "repo": "camera-box-av-soak", "run_id": "s1", "run_url": "", "job": "av-soak",
        "acquired_at": rls.format_ts(now), "expected_release_at": rls.format_ts(now + timedelta(hours=7)),
        "resources": resources}))
    (lease / "heartbeat").touch()


def test_the_server_serves_the_scoped_view(tmp_path):
    lease = tmp_path / "rig-lease"
    _live_seed(lease, SNV_VIDEO)
    with _Server(lease) as s:
        st, whole = s.call("GET", "/rig-lease.json")
        assert st == 200 and whole["held"] is True and whole["lease_model"] == 2
        st, scoped = s.call("GET", "/rig-lease.json?resources=resolume-snv")
        assert st == 200 and scoped["held"] is False and scoped["scope"] == ["resolume-snv"]
        _, busted = s.call("GET", "/rig-lease.json?t=1&resources=resolume-snv")
        assert busted["held"] is False
        _, two = s.call("GET", "/rig-lease.json?resources=resolume-snv&resources=imag")
        assert two["held"] is True and two["scope"] == ["imag", "resolume-snv"]
        _, bad = s.call("GET", "/rig-lease.json?resources=resolum")
        assert bad["held"] is True and bad["scope_error"] and "scope" not in bad
        assert s.call("HEAD", "/rig-lease.json?resources=resolume-snv") == (200, None)


def test_the_server_serves_fail_closed_on_a_broken_table(tmp_path, monkeypatch):
    bad = tmp_path / "t.txt"
    bad.write_text("group|g|nope\n")
    monkeypatch.setenv("RIG_LEASE_RESOURCES_TABLE", str(bad))
    with _Server(tmp_path / "rig-lease") as s:
        _, whole = s.call("GET", "/rig-lease.json")
        _, scoped = s.call("GET", "/rig-lease.json?resources=resolume-snv")
    assert whole["schema"] == 1 and whole["held"] is True and whole["holder"] is None
    assert scoped["held"] is True


# --- POST /rig-lease/acquire takes `resources` --------------------------------------------------------

def _token(tmp_path):
    p = tmp_path / "token"
    p.write_text(TOKEN + "\n")
    p.chmod(0o600)
    return p


def _acquire_body(**over):
    b = {"repo": "zbynekdrlik/restreamer", "run_id": "38632386935",
         "run_url": "https://github.com/zbynekdrlik/restreamer/actions/runs/38632386935",
         "job": "e2e-obs-youtube-test", "expected_secs": 2700}
    b.update(over)
    return b


@pytest.mark.parametrize("value, atoms", [
    (["snv-video"], SNV_VIDEO),
    ("snv-video", SNV_VIDEO),
    ("stream-snv, snv-cams", ["snv-cams", "stream-snv"]),
    (["all"], ATOMS),
])
def test_post_acquire_declares_the_resources(tmp_path, value, atoms):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token(tmp_path)) as s:
        st, acq = s.call("POST", "/rig-lease/acquire", _acquire_body(resources=value))
        assert st == 200 and acq["acquired"] is True and acq["resources"] == atoms, acq
        _, scoped = s.call("GET", "/rig-lease.json?resources=resolume-pp")
    assert json.loads((lease / "holder.json").read_text())["resources"] == atoms
    assert scoped["held"] is ("resolume-pp" in atoms)


def test_post_acquire_without_resources_holds_everything_even_with_the_env_set(tmp_path, monkeypatch):
    monkeypatch.setenv("RIG_LEASE_RESOURCES", "snv-cams")  # the server's own env never leaks in
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token(tmp_path)) as s:
        st, acq = s.call("POST", "/rig-lease/acquire", _acquire_body())
    assert st == 200 and acq["resources"] is None
    assert "resources" not in json.loads((lease / "holder.json").read_text())


@pytest.mark.parametrize("value", ["snv-vid", ["snv-video", "nope"], 5, [5], {"a": "b"}, [""], ""])
def test_post_acquire_refuses_a_bad_declaration_with_400_and_writes_nothing(tmp_path, value):
    lease = tmp_path / "rig-lease"
    with _Server(lease, _token(tmp_path)) as s:
        st, reply = s.call("POST", "/rig-lease/acquire", _acquire_body(resources=value))
    assert st == 400 and reply["field"] == "resources", reply
    assert not lease.exists()
