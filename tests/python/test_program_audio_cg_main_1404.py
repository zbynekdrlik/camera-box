"""issue 1404 -- the CG arm as main() wires it (review of the CG arm: the wiring was untested).

main() is driven with fakes for the NDI receiver class, the marker decoder, the home probe and the
consumer loop, so every step of the production wiring is pinned: the CG receivers are created before
the stream one, the arm handed to run() is started BEFORE run() reads it, every receiver is closed
after its capture thread left the SDK call, a CG box with unknown NDI names is FATAL before any
receiver exists, and a failed stream receiver closes the CG receivers made before it.
"""
from __future__ import annotations

import json
import os
import pathlib
import sys
import time

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio_cg as pcg  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

SP = "RESOLUME-SNV (SP-program)"
CGOBS = "RESOLUME-SNV (cg-obs)"
STREAM = "STREAM-SNV (stream)"


class _Rx:
    made: list = []
    fail_on: set = set()

    def __init__(self, source, lib_path=None):
        if source in _Rx.fail_on:
            raise RuntimeError(f"NDIlib_recv_create_v3 failed for source {source!r}")
        self.source = source
        self.closed = False
        _Rx.made.append(self)

    def capture(self, timeout_ms):
        time.sleep(0.005)
        return None

    def connections(self):
        return 0

    def close(self):
        self.closed = True


class _Decoder:
    def __init__(self, path=None):
        self.path = "/fake/shim.so"
        self.params = {}
        self.built_sha256 = "0" * 12

    def sources_stale(self):
        return False


class _Probe:
    made: list = []

    def __init__(self, box):
        self.box = box
        self.home = True
        self.started = False
        self.stopped = False
        _Probe.made.append(self)

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True


@pytest.fixture
def wired(monkeypatch, tmp_path):
    _Rx.made, _Rx.fail_on, _Probe.made = [], set(), []
    monkeypatch.setattr(pan, "NdiAudioReceiver", _Rx)
    monkeypatch.setattr(pam, "MarkerDecoder", _Decoder)
    monkeypatch.setattr(pcg, "HomeProbe", _Probe)
    for k in (pcg.CG_SOURCES_ENV, pcg.CG_BOX_ENV, "OBS_FLEET_IDENTITY"):
        monkeypatch.delenv(k, raising=False)
    seen = {}

    def fake_run(receiver, serve_dir, **kw):
        cg = kw.get("cg")
        seen["receiver"] = receiver
        seen["cg"] = cg
        seen["started"] = (None if cg is None else
                           [m._thread is not None and m._thread.is_alive() for m in cg.monitors])
        seen["probe_started"] = None if cg is None else cg.home_probe.started

    monkeypatch.setattr(pas, "run", fake_run)
    serve = tmp_path / "serve"
    return seen, serve


def _main(serve):
    return pas.main(["--serve-dir", str(serve), "--source", STREAM, "--http-port", "0"])


def _payload(serve):
    return json.loads((serve / rsf.PROGRAM_AUDIO_NAME).read_text(encoding="utf-8"))


def test_main_creates_the_cg_receivers_then_the_stream_one_and_runs_a_started_arm(wired):
    seen, serve = wired
    assert _main(serve) == 0
    assert [r.source for r in _Rx.made] == [SP, CGOBS, STREAM]
    cg = seen["cg"]
    assert isinstance(cg, pcg.CgArm) and cg.box == "resolume-snv" and cg.sources == [SP, CGOBS]
    assert seen["receiver"] is _Rx.made[2]
    assert seen["started"] == [True, True] and seen["probe_started"] is True  # started BEFORE run()
    assert [m.receiver for m in cg.monitors] == _Rx.made[:2]
    assert all(r.closed for r in _Rx.made)        # every receiver closed after its thread stopped
    assert _Probe.made[0].stopped is True
    assert _payload(serve)["reason"] == "sampler stopped"


def test_main_refuses_a_cg_box_without_ndi_names_before_any_receiver(wired, monkeypatch):
    _seen, serve = wired
    monkeypatch.setenv("OBS_FLEET_IDENTITY", "strih-pp=strih-pp")
    assert _main(serve) == 1
    assert _Rx.made == []
    p = _payload(serve)
    assert p["verdict"] == "UNKNOWN" and "sampler cannot listen to the CG chain" in p["reason"]


def test_a_failed_stream_receiver_closes_the_cg_receivers_made_before_it(wired):
    _seen, serve = wired
    _Rx.fail_on = {STREAM}
    assert _main(serve) == 1
    assert [r.source for r in _Rx.made] == [SP, CGOBS] and all(r.closed for r in _Rx.made)
    assert "sampler cannot receive" in _payload(serve)["reason"]


def test_a_failed_cg_receiver_closes_the_ones_before_it(wired):
    _seen, serve = wired
    _Rx.fail_on = {CGOBS}
    assert _main(serve) == 1
    assert [r.source for r in _Rx.made] == [SP] and _Rx.made[0].closed


def test_an_http_port_that_cannot_be_served_closes_every_receiver(wired, monkeypatch):
    _seen, serve = wired
    import program_audio_http as pah

    def busy(bind, port, serve_dir):
        raise OSError(98, "Address already in use")

    monkeypatch.setattr(pah, "make_server", busy)
    assert pas.main(["--serve-dir", str(serve), "--source", STREAM, "--http-port", "18891"]) == 1
    assert [r.source for r in _Rx.made] == [SP, CGOBS, STREAM] and all(r.closed for r in _Rx.made)
    assert "cannot serve http" in _payload(serve)["reason"]


def test_the_box_env_picks_the_cg_sources(wired, monkeypatch):
    seen, serve = wired
    monkeypatch.setenv(pcg.CG_SOURCES_ENV, "X (a)|Y (b)")
    monkeypatch.setenv(pcg.CG_BOX_ENV, "resolume-pp")
    assert _main(serve) == 0
    assert seen["cg"].box == "resolume-pp" and seen["cg"].sources == ["X (a)", "Y (b)"]
    assert _Probe.made[0].box == "resolume-pp"


def test_close_arm_never_closes_a_receiver_still_inside_a_capture():
    class _Stuck:
        def __init__(self, rx):
            self.receiver, self.source = rx, rx.source

        def stop(self, join_s=3.0):
            return False

    class _Done(_Stuck):
        def stop(self, join_s=3.0):
            return True

    a, b = _Rx("A (x)"), _Rx("B (y)")
    probe = _Probe("resolume-snv")
    arm = pcg.CgArm("resolume-snv", [_Stuck(a), _Done(b)], probe)
    logged = []
    pcg.close_arm(arm, logged.append)
    assert a.closed is False and b.closed is True and probe.stopped
    assert len(logged) == 1 and "'A (x)'" in logged[0] and "left to the process exit" in logged[0]


def test_the_sampler_main_reads_no_cg_literal():
    src = (_SCRIPTS / "program_audio_sampler.py").read_text(encoding="utf-8")
    code = "\n".join(line for line in src.splitlines() if not line.lstrip().startswith("#"))
    body = code.split("def main(", 1)[1]
    assert "RESOLUME" not in body and "SP-program" not in body and "cg-obs" not in body
    assert os.path.basename(pcg.__file__) == "program_audio_cg.py"
