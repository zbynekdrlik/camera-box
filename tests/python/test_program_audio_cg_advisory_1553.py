"""issue 1553 -- the CG arm of the program-audio guard becomes ADVISORY (ROZHODNUTÉ 25.11.2026,
issuecomment-7174499060) while BOTH hold: (a) the stream OBS program tree carries no audible input
but the program-audio input (read over OBS-WS, read-only), (b) fohmixer's mbc proof reads ok AND
hand1_sync_only. Either unreadable, stale or false = the strict fail-closed arm exactly as before.

Fakes only: no box, no network (the HTTP read and the OBS-WS client are replaced in-process).
"""
from __future__ import annotations

import json
import pathlib
import sys
import types
import urllib.error

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for p in (str(_SCRIPTS), str(_HERE)):
    if p not in sys.path:
        sys.path.insert(0, p)

import program_audio_cg as pcg  # noqa: E402
import program_audio_cg_advisory as adv  # noqa: E402
import program_audio_cg_corr as pcc  # noqa: E402
import program_audio_guard as guard  # noqa: E402
from test_program_audio_cg_1404 import (  # noqa: E402
    CGOBS, SP, _FakeArm, _guard, _reading, _run, _served_file, _state)

TEXT = "cg advisory (mbc proof: hand1 sync only, program tree: mbc only)"
PROOF = {"ok": True, "ts": "2026-11-25T21:30:00Z", "open": ["Hand1sync #"], "hand1_sync_only": True,
         "outputs": {"OM STREAM": "Ext. Out 3/4"}, "age_ms": 4000}


def _applies(now=100.0):
    return adv.decide(adv.Reading(True, "mbc only", now - 1), adv.Reading(True, "hand1 sync only", now - 1),
                      now, "mbc")


def _strict(now=100.0):
    return adv.decide(None, adv.Reading(True, "hand1 sync only", now - 1), now, "mbc")


def _with(state, advisory):
    return state._replace(advisory=advisory)


# ---------------------------------------------------------------------------------------------
# (a) the program tree
# ---------------------------------------------------------------------------------------------

def _item(name, enabled=True, scene=False, group=False):
    return {"sourceName": name, "sceneItemEnabled": enabled,
            "sourceType": "OBS_SOURCE_TYPE_SCENE" if scene else "OBS_SOURCE_TYPE_INPUT", "isGroup": group}


TREE = {
    ("Development", False): [_item("PRO", scene=True), _item("JXS PGM src"), _item("old", enabled=False)],
    ("PRO", False): [_item("mbc"), _item("NDI 2ME PGM", enabled=False), _item("Overlay", scene=True, group=True)],
    ("Overlay", True): [_item("Browser"), _item("Development", scene=True)],  # a cycle walks once
}


def test_the_tree_descends_nested_scenes_and_groups_enabled_items_only():
    calls = []

    def fetch(name, is_group):
        calls.append((name, is_group))
        return TREE[(name, is_group)]

    assert adv.visible_inputs(fetch, "Development") == ["mbc", "Browser", "JXS PGM src"]
    assert ("Overlay", True) in calls and calls.count(("Development", False)) == 1


def test_an_item_without_a_source_name_is_unreadable():
    with pytest.raises(adv.Unreadable):
        adv.visible_inputs(lambda n, g: [{"sceneItemEnabled": True}], "S")


def test_audible_means_a_track_on_and_not_muted():
    states = {"mbc": (True, False), "JXS PGM src": (False, False), "Zaloha": (True, True), "Browser": None}
    assert adv.audible_inputs(list(states), states.get) == ["mbc"]


def test_the_tree_verdict_names_every_other_audible_input():
    assert adv.tree_verdict(["mbc"], "mbc") == adv.Reading(True, "mbc only")
    assert adv.tree_verdict([], "mbc").ok is True
    r = adv.tree_verdict(["mbc", "SP audio", "Desktop Audio"], "mbc")
    assert r.ok is False and r.why == "also 'SP audio', 'Desktop Audio'"


class _FakeObs:
    """obs_phase2's two primitives over a scripted collection; requests are logged."""

    def __init__(self, program="Development", tracks=None, muted=None, special=None, no_audio=(),
                 transition=None):
        self.program, self.log = program, []
        self.transition = transition if transition is not None else {
            "transitionName": "Fade", "transitionKind": "fade_transition"}
        self.fail_next = None
        self.sessions = 0
        self.override = None
        self.transition_list = [{"transitionName": "Fade", "transitionKind": "fade_transition"},
                                {"transitionName": "Song stinger", "transitionKind": "obs_stinger_transition"}]
        self.tracks = tracks if tracks is not None else {"mbc": {"1": True, "2": False}}
        self.muted = muted if muted is not None else {"mbc": False}
        self.special = special if special is not None else {"desktop1": None, "mic1": None}
        self.no_audio = set(no_audio)
        self.closed = False

    def module(self):
        m = types.ModuleType("obs_phase2")
        fake = self

        class _Ws:
            def settimeout(self, t):
                pass

            def close(self):
                fake.closed = True

        def _conn(host, password="", port=None):
            fake.log.append(("conn", host, password))
            fake.sessions += 1
            return _Ws()
        m._conn = _conn

        def _rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
            fake.log.append((rtype, (rdata or {}).get("sceneName") or (rdata or {}).get("inputName")))
            d = rdata or {}
            if fake.fail_next == rtype:
                fake.fail_next = None
                raise TimeoutError(f"obs-websocket request {rtype!r} got no response")
            if rtype == "GetCurrentSceneTransition":
                return fake.transition
            if rtype == "GetSceneSceneTransitionOverride":
                assert d["sceneName"] == fake.program
                return {"transitionName": fake.override, "transitionDuration": None}
            if rtype == "GetSceneTransitionList":
                return {"transitions": fake.transition_list}
            if rtype == "GetCurrentProgramScene":
                return {"currentProgramSceneName": fake.program}
            if rtype in ("GetSceneItemList", "GetGroupSceneItemList"):
                return {"sceneItems": TREE[(d["sceneName"], rtype == "GetGroupSceneItemList")]}
            if rtype == "GetSpecialInputs":
                return fake.special
            if rtype == "GetInputAudioTracks":
                if d["inputName"] in fake.no_audio:
                    raise RuntimeError("GetInputAudioTracks failed: {'result': False, 'code': 604, "
                                       "'comment': 'The specified input does not support audio.'}")
                if d["inputName"] not in fake.tracks:
                    raise RuntimeError("GetInputAudioTracks failed: {'result': False, 'code': 600}")
                return {"inputAudioTracks": fake.tracks[d["inputName"]]}
            if rtype == "GetInputMute":
                return {"inputMuted": fake.muted[d["inputName"]]}
            raise AssertionError(f"unexpected request {rtype}")
        m._rpc = _rpc
        return m


def test_the_live_tree_read_is_read_only_and_counts_604_as_no_audio(monkeypatch):
    obs = _FakeObs(tracks={"mbc": {"1": True}, "Desktop Audio": {"1": True}},
                   muted={"mbc": False, "Desktop Audio": True},
                   special={"desktop1": "Desktop Audio", "mic1": None}, no_audio=("Browser", "JXS PGM src"))
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    assert adv.read_program_tree("10.77.9.204") == ["mbc"]
    kinds = {k for k, *_ in obs.log}
    assert kinds <= {"conn", "GetCurrentProgramScene", "GetSceneItemList", "GetGroupSceneItemList",
                     "GetSpecialInputs", "GetInputAudioTracks", "GetInputMute", "GetCurrentSceneTransition",
                     "GetSceneSceneTransitionOverride", "GetSceneTransitionList"}
    assert not any(k.startswith("Set") or k.startswith("Trigger") for k in kinds)
    assert obs.closed


def test_a_global_audio_device_counts_as_another_audible_input(monkeypatch):
    obs = _FakeObs(tracks={"mbc": {"1": True}, "Mic/Aux": {"1": True}}, muted={"mbc": False, "Mic/Aux": False},
                   special={"mic1": "Mic/Aux"}, no_audio=("Browser", "JXS PGM src"))
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    assert adv.read_program_tree("h") == ["mbc", "Mic/Aux"]


@pytest.mark.parametrize("kind,audible", [("fade_transition", False), ("cut_transition", False),
                                           ("obs_stinger_transition", True), ("some_plugin_transition", True)])
def test_a_transition_that_may_carry_audio_counts_as_an_audible_input(monkeypatch, kind, audible):
    obs = _FakeObs(no_audio=("Browser", "JXS PGM src"), transition={"transitionName": "T", "transitionKind": kind})
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    got = adv.read_program_tree("h")
    assert got == (["mbc", f"transition 'T' ({kind})"] if audible else ["mbc"])


def test_the_program_scenes_own_transition_override_is_read_too(monkeypatch):
    obs = _FakeObs(no_audio=("Browser", "JXS PGM src"))
    obs.override = "Song stinger"
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    assert adv.read_program_tree("h") == ["mbc", "transition 'Song stinger' (obs_stinger_transition)"]
    obs.override = "Fade"
    assert adv.read_program_tree("h") == ["mbc"]
    obs.override = "Gone"
    with pytest.raises(adv.Unreadable, match="not in the transition list"):
        adv.read_program_tree("h")


def test_a_stopped_probe_never_reconnects(monkeypatch):
    obs = _FakeObs(no_audio=("Browser", "JXS PGM src"))
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    reader = adv.ProgramTreeReader("h")
    probe = adv.AdvisoryProbe("mbc", reader, lambda: PROOF, mono=_Clock())
    probe.poll_once()
    assert obs.sessions == 1 and probe.state().applies
    probe.stop()
    assert obs.closed
    with pytest.raises(adv.Unreadable, match="shut down"):
        reader()
    assert obs.sessions == 1


def test_a_transition_without_a_kind_is_unreadable(monkeypatch):
    obs = _FakeObs(no_audio=("Browser", "JXS PGM src"), transition={"transitionName": "T"})
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    with pytest.raises(adv.Unreadable, match="transitionKind"):
        adv.read_program_tree("h")


def test_the_reader_keeps_one_session_and_reconnects_after_a_failure(monkeypatch):
    obs = _FakeObs(no_audio=("Browser", "JXS PGM src"))
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    reader = adv.ProgramTreeReader("10.77.9.204")
    assert reader() == ["mbc"] and reader() == ["mbc"]
    assert obs.sessions == 1 and not obs.closed
    obs.fail_next = "GetSceneItemList"
    with pytest.raises(TimeoutError):
        reader()
    assert obs.closed  # a failed session is dropped
    assert reader() == ["mbc"] and obs.sessions == 2
    reader.close()


def test_any_other_request_error_is_unreadable_not_no_audio(monkeypatch):
    obs = _FakeObs(no_audio=("Browser",))  # 'JXS PGM src' answers code 600
    monkeypatch.setitem(sys.modules, "obs_phase2", obs.module())
    with pytest.raises(RuntimeError, match="600"):
        adv.read_program_tree("h")


def test_a_missing_websocket_client_is_unreadable_never_a_dead_thread(monkeypatch):
    monkeypatch.setitem(sys.modules, "obs_phase2", None)  # the import fails
    with pytest.raises(adv.Unreadable, match="OBS-WS client cannot load"):
        adv.read_program_tree("h")
    probe = adv.AdvisoryProbe("mbc", lambda: adv.read_program_tree("h"), lambda: PROOF, mono=lambda: 1.0)
    probe.poll_once()
    assert probe.tree.ok is None and probe.state(1.0).applies is False


# ---------------------------------------------------------------------------------------------
# (b) the mbc proof
# ---------------------------------------------------------------------------------------------

def test_the_proof_needs_ok_and_hand1_sync_only_true():
    r = adv.mbc_proof_verdict(PROOF)
    assert r.ok is True and r.extra_age_s == 4.0
    assert adv.mbc_proof_verdict(dict(PROOF, hand1_sync_only=False)).ok is False
    assert adv.mbc_proof_verdict(dict(PROOF, hand1_sync_only="true")).ok is None
    assert adv.mbc_proof_verdict({k: v for k, v in PROOF.items() if k != "hand1_sync_only"}).ok is None
    r = adv.mbc_proof_verdict({"ok": False, "reason": "MBC Live not running"})
    assert r.ok is None and "MBC Live not running" in r.why
    assert adv.mbc_proof_verdict(dict(PROOF, ok="yes")).ok is None
    assert adv.mbc_proof_verdict([PROOF]).ok is None


def test_fohmixer_text_is_cleaned_and_cut():
    r = adv.mbc_proof_verdict({"ok": False, "reason": "line1\nline2\x1b[31m" + "x" * 300})
    assert "\n" not in r.why and "\x1b" not in r.why and len(r.why) < 160 and r.why.endswith("...)")


@pytest.mark.parametrize("age_ms", [None, -1, "4000", float("nan"), True])
def test_a_proof_without_a_valid_age_is_unreadable(age_ms):
    assert adv.mbc_proof_verdict(dict(PROOF, age_ms=age_ms)).ok is None


class _Resp:
    def __init__(self, body):
        self.body = body

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def read(self):
        return self.body


def test_the_fetch_sends_the_bearer_token_from_the_named_file(tmp_path, monkeypatch):
    tok = tmp_path / "token"
    tok.write_text("tok-of-the-test\n")
    seen = {}

    def urlopen(req, timeout):
        seen["auth"], seen["url"], seen["timeout"] = req.get_header("Authorization"), req.full_url, timeout
        return _Resp(json.dumps(PROOF).encode())

    monkeypatch.setattr(adv.urllib.request, "urlopen", urlopen)
    assert adv.fetch_mbc_proof("http://foh-hub.lan/api/mbc/stream-proof", str(tok)) == PROOF
    assert seen == {"auth": "Bearer tok-of-the-test", "url": "http://foh-hub.lan/api/mbc/stream-proof",
                    "timeout": adv.MBC_PROOF_TIMEOUT_S}


def test_a_fetch_failure_never_names_the_token(tmp_path, monkeypatch):
    tok = tmp_path / "token"
    tok.write_text("tok-of-the-test")

    def urlopen(req, timeout):
        raise urllib.error.HTTPError(req.full_url, 401, "Unauthorized", {}, None)

    monkeypatch.setattr(adv.urllib.request, "urlopen", urlopen)
    with pytest.raises(adv.Unreadable) as e:
        adv.fetch_mbc_proof("http://h/p", str(tok))
    assert "401" in str(e.value) and "tok-of-the-test" not in str(e.value)
    monkeypatch.setattr(adv.urllib.request, "urlopen", lambda req, timeout: _Resp(b"<html>"))
    with pytest.raises(adv.Unreadable, match="not JSON"):
        adv.fetch_mbc_proof("http://h/p", str(tok))


@pytest.mark.parametrize("content", [None, "", "two words"])
def test_a_missing_or_bad_token_file_is_unreadable(tmp_path, content):
    tok = tmp_path / "token"
    if content is not None:
        tok.write_text(content)
    with pytest.raises(adv.Unreadable):
        adv.fetch_mbc_proof("http://h/p", str(tok))


# ---------------------------------------------------------------------------------------------
# the decision + its bounds
# ---------------------------------------------------------------------------------------------

def test_both_proofs_make_it_advisory_with_the_documented_line():
    a = _applies()
    assert a.applies is True and a.text == TEXT


def test_the_text_names_the_input_from_the_facts():
    a = adv.decide(adv.Reading(True, "x", 0.0), adv.Reading(True, "y", 0.0), 1.0, "Zvuk z mixu")
    assert a.text == "cg advisory (mbc proof: hand1 sync only, program tree: Zvuk z mixu only)"


@pytest.mark.parametrize("tree,mbc,why", [
    (None, adv.Reading(True, "h", 99.0), "program tree: not read yet"),
    (adv.Reading(True, "t", 99.0), None, "mbc proof: not read yet"),
    (adv.Reading(None, "unreadable: refused", 99.0), adv.Reading(True, "h", 99.0), "unreadable: refused"),
    (adv.Reading(False, "also 'SP audio'", 99.0), adv.Reading(True, "h", 99.0), "also 'SP audio'"),
    (adv.Reading(True, "t", 99.0), adv.Reading(False, "not hand1 sync only", 99.0), "not hand1 sync only"),
    (adv.Reading(True, "t", 99.0), adv.Reading(None, "the mbc proof reads ok=false", 99.0), "ok=false"),
])
def test_either_proof_unreadable_or_false_is_strict(tree, mbc, why):
    a = adv.decide(tree, mbc, 100.0, "mbc")
    assert a.applies is False and a.text.startswith("cg strict (") and why in a.text


def test_the_tree_answer_is_valid_45_s():
    mbc = adv.Reading(True, "h", 100.0)
    assert adv.decide(adv.Reading(True, "t", 100.0), mbc, 100.0 + adv.TREE_MAX_AGE_S, "mbc").applies
    a = adv.decide(adv.Reading(True, "t", 100.0), mbc, 100.1 + adv.TREE_MAX_AGE_S, "mbc")
    assert not a.applies and "program tree: stale" in a.text
    assert adv.TREE_MAX_AGE_S == 45.0 and adv.TREE_INTERVAL_S == 15.0


def test_the_proof_age_adds_the_hubs_own_age_ms():
    tree = adv.Reading(True, "t", 200.0)
    # read 100 s ago, the hub's answer was already 40 s old: 140 s <= 150 s
    ok = adv.decide(tree, adv.Reading(True, "h", 100.0, 40.0), 200.0, "mbc")
    assert ok.applies and ok.mbc_proof_age_s == 140.0
    # read 100 s ago, the hub's answer was already 60 s old: 160 s > 150 s
    a = adv.decide(tree, adv.Reading(True, "h", 100.0, 60.0), 200.0, "mbc")
    assert not a.applies and "mbc proof: stale" in a.text
    assert adv.MBC_PROOF_MAX_AGE_S == 150.0 and adv.MBC_PROOF_INTERVAL_S == 60.0


class _Clock:
    def __init__(self):
        self.t = 1000.0

    def __call__(self):
        return self.t


def test_the_probe_reads_the_proof_at_most_once_a_minute_and_the_tree_every_15_s():
    clock = _Clock()
    n = {"tree": 0, "mbc": 0}

    def tree():
        n["tree"] += 1
        return ["mbc"]

    def mbc():
        n["mbc"] += 1
        return PROOF

    probe = adv.AdvisoryProbe("mbc", tree, mbc, mono=clock)
    for _ in range(120):  # 120 s, one poll a second: reads at 0 + 60 s, the tree at 0, 15, ... 105 s
        probe.poll_once()
        clock.t += 1.0
    assert n["mbc"] == 2 and n["tree"] == 8
    assert probe.state().applies is True


def test_a_failed_attempt_counts_against_the_rate_and_replaces_the_good_answer():
    clock = _Clock()
    answers = [PROOF, ConnectionError("hub down"), PROOF]
    n = {"mbc": 0}

    def mbc():
        n["mbc"] += 1
        a = answers.pop(0)
        if isinstance(a, Exception):
            raise a
        return a

    probe = adv.AdvisoryProbe("mbc", lambda: ["mbc"], mbc, mono=clock)
    probe.poll_once()
    assert probe.state().applies
    clock.t += 60.0
    probe.poll_once()
    st = probe.state()
    assert not st.applies and "hub down" in st.text  # strict at once, never the last good answer
    clock.t += 30.0
    probe.poll_once()
    assert n["mbc"] == 2  # the failed attempt counted: no retry before a minute
    clock.t += 30.0
    probe.poll_once()
    assert n["mbc"] == 3 and probe.state().applies


def test_an_answer_is_as_old_as_its_read_start():
    clock = _Clock()

    def slow_tree():
        clock.t += 40.0  # a read that took 40 s
        return ["mbc"]

    probe = adv.AdvisoryProbe("mbc", slow_tree, lambda: PROOF, mono=clock)
    probe.poll_once()
    assert probe.tree.at == 1000.0
    clock.t = 1000.0 + adv.TREE_MAX_AGE_S + 1
    assert not probe.state().applies


def test_a_system_exit_inside_a_read_is_strict_and_the_thread_keeps_reading():
    calls = {"n": 0}

    def tree():
        calls["n"] += 1
        raise SystemExit("missing dep: pip install websocket-client")

    probe = adv.AdvisoryProbe("mbc", tree, lambda: PROOF, mono=_Clock())
    probe.poll_once()
    assert probe.tree.ok is None and "missing dep" in probe.tree.why and not probe.state().applies


def test_a_dead_probe_thread_is_strict():
    probe = adv.AdvisoryProbe("mbc", lambda: ["mbc"], lambda: PROOF, mono=_Clock())
    probe.poll_once()
    assert probe.state().applies

    class _Dead:
        def is_alive(self):
            return False

    probe._thread = _Dead()
    st = probe.state()
    assert not st.applies and "thread stopped" in st.text


def test_a_tree_with_another_audible_input_is_strict():
    probe = adv.AdvisoryProbe("mbc", lambda: ["mbc", "SP audio"], lambda: PROOF, mono=_Clock())
    probe.poll_once()
    st = probe.state()
    assert not st.applies and "also 'SP audio'" in st.text


def test_the_thread_starts_and_stops():
    probe = adv.AdvisoryProbe("mbc", lambda: ["mbc"], lambda: PROOF, tick_s=0.01)
    probe.start()
    probe._thread.join(0.2)
    probe.stop()
    probe._thread.join(2.0)
    assert not probe._thread.is_alive() and probe.state().applies


# ---------------------------------------------------------------------------------------------
# the config
# ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("env", [{}, {adv.URL_ENV: "http://h/p"}, {adv.TOKEN_FILE_ENV: "/t"},
                                 {adv.URL_ENV: " ", adv.TOKEN_FILE_ENV: "/t"}])
def test_without_the_url_and_the_token_file_there_is_no_probe(env):
    assert adv.from_env(env) is None
    assert "CG arm strict" in adv.start_line(None)


def test_the_program_input_comes_from_the_stream_box_fact_file(tmp_path):
    (tmp_path / "stream-snv.env").write_text("# a comment\nSTRIH_ROLE=stream\nSTRIH_PROGRAM_AUDIO_INPUT=Mix 7\n")
    probe = adv.from_env({adv.URL_ENV: "http://h/p", adv.TOKEN_FILE_ENV: "/t"}, boxes_dir=tmp_path)
    assert probe.program_input == "Mix 7"
    # the real fact files: no literal anywhere
    assert adv.program_input_for("stream-snv") == "mbc"
    assert adv.program_input_for("stream-pp") == "Zvuk z mixu"
    pp = adv.from_env({adv.URL_ENV: "http://h/p", adv.TOKEN_FILE_ENV: "/t", adv.STREAM_BOX_ENV: "stream-pp"})
    assert pp.program_input == "Zvuk z mixu" and isinstance(pp._read_tree, adv.ProgramTreeReader)


def test_an_unknown_stream_box_or_a_missing_fact_fails_loud(tmp_path):
    env = {adv.URL_ENV: "http://h/p", adv.TOKEN_FILE_ENV: "/t"}
    with pytest.raises(ValueError, match="not in the OBS fleet"):
        adv.from_env(dict(env, **{adv.STREAM_BOX_ENV: "no-such-box"}))
    (tmp_path / "stream-snv.env").write_text("STRIH_PROGRAM_AUDIO_INPUT=none\n")
    with pytest.raises(ValueError, match="STRIH_PROGRAM_AUDIO_INPUT"):
        adv.from_env(env, boxes_dir=tmp_path)
    with pytest.raises(ValueError, match="unreadable"):
        adv.program_input_for("stream-snv", tmp_path / "missing")


# ---------------------------------------------------------------------------------------------
# the overlay: advisory keeps the window's own verdict
# ---------------------------------------------------------------------------------------------

_UNDECIDED = pcc.Correlation(pcc.UNDECIDED, why="too little audio buffered")


@pytest.mark.parametrize("state", [
    _state(_reading(SP, -30.0), _reading(CGOBS)),                                    # heard
    _state(_reading(SP, -30.0)._replace(correlation=_UNDECIDED), _reading(CGOBS)),   # not proven absent
    _state(_reading(SP), _reading(CGOBS, None, None, 0)),                             # not receivable
])
def test_advisory_keeps_the_measurement_and_names_what_strict_would_do(state):
    strict = pcg.overlay("MEASUREMENT", None, state)
    assert strict[0] in ("FOREIGN", "UNKNOWN")
    st = _with(state, _applies())
    assert pcg.overlay("MEASUREMENT", None, st) == ("MEASUREMENT", None)
    assert pcg.suppressed("MEASUREMENT", None, st) == f"{strict[0]}: {strict[1]}"
    block = pcg.payload_block(st, pcg.suppressed("MEASUREMENT", None, st))
    assert block["advisory"]["applies"] is True and block["advisory"]["text"] == TEXT
    assert block["advisory"]["suppressed"].startswith(strict[0] + ": ")


def test_the_not_proven_absent_path_is_advisory_too():
    st = _with(_state(_reading(SP, -30.0)._replace(correlation=_UNDECIDED), _reading(CGOBS)), _applies())
    assert "not proven absent from the stream program" in pcg.suppressed("SILENT", None, st)
    assert pcg.overlay("SILENT", None, st) == ("SILENT", None)


def test_a_stream_side_foreign_or_unknown_is_never_touched():
    st = _with(_state(_reading(SP, -30.0)), _applies())
    assert pcg.overlay("FOREIGN", None, st) == ("FOREIGN", None)
    assert pcg.overlay("UNKNOWN", "marker span", st) == ("UNKNOWN", "marker span")


@pytest.mark.parametrize("advisory", [None, "strict"])
def test_strict_is_byte_for_byte_the_old_arm(advisory):
    state = _state(_reading(SP, -30.0), _reading(CGOBS))
    st = _with(state, None if advisory is None else _strict())
    assert pcg.overlay("MEASUREMENT", None, st) == pcg.overlay("MEASUREMENT", None, state)
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"
    assert pcg.suppressed("MEASUREMENT", None, st) is None
    block = pcg.payload_block(st)
    if advisory is None:
        assert "advisory" not in block and block == pcg.payload_block(state)
    else:
        assert block["advisory"]["applies"] is False and block["advisory"]["suppressed"] is None
        assert block["advisory"]["text"].startswith("cg strict (")


def test_nothing_to_suppress_reads_none():
    st = _with(_state(), _applies())
    assert pcg.suppressed("MEASUREMENT", None, st) is None
    assert pcg.payload_block(st)["advisory"]["suppressed"] is None


def test_the_arm_carries_the_probe_state_and_runs_its_thread():
    class _P:
        started = stopped = False

        def state(self, now=None):
            return _applies()

        def start(self):
            self.started = True

        def stop(self):
            self.stopped = True

    class _Home:
        home = True

        def start(self):
            pass

        def stop(self):
            pass

    p = _P()
    arm = pcg.CgArm("resolume-snv", [], _Home(), advisory=p)
    assert arm.state().advisory.applies is True
    arm.start()
    arm.stop()
    assert p.started and p.stopped
    assert pcg.CgArm("resolume-snv", [], _Home()).state().advisory is None


# ---------------------------------------------------------------------------------------------
# the sampler loop + the guard line
# ---------------------------------------------------------------------------------------------

def test_the_loop_keeps_measuring_and_never_latches_while_advisory(tmp_path):
    loud = _with(_state(_reading(SP, -30.0), _reading(CGOBS)), _applies())
    seen = _run(tmp_path, _FakeArm(loud), seconds=6.0)
    assert [p["verdict"] for p in seen[1:]] == ["UNKNOWN", "MEASUREMENT", "MEASUREMENT"]
    assert all(p["last_foreign_ts_utc"] is None for p in seen)
    assert seen[-1]["cg"]["status"] == "cg-audio"
    assert seen[-1]["cg"]["advisory"]["suppressed"].startswith("FOREIGN: cg-audio: ")


def test_the_loop_logs_the_advisory_state_changes_once(tmp_path):
    import program_audio_sampler as pas
    from qpsk_guard_shim_1404 import FixedChain
    from test_program_audio_cg_1404 import _Feed, _tone_blocks

    loud = _state(_reading(SP, -30.0), _reading(CGOBS))
    arm = _FakeArm(_with(loud, _applies()))
    blocks = _tone_blocks(8.0)
    logs = []
    tmp_path.mkdir(exist_ok=True)
    pas.run(_Feed(blocks, arm, {250: _with(loud, _strict())}), str(tmp_path), source="STREAM-SNV (stream)",
            decoder=FixedChain(), max_loops=len(blocks), log=logs.append, cg=arm)
    adv_lines = [m for m in logs if "cg advisory" in m or "CG arm strict" in m]
    assert adv_lines[0] == f"program-audio sampler: {TEXT}"
    assert any("the strict arm would read FOREIGN: cg-audio: " in m for m in adv_lines)
    assert any(m.startswith("program-audio sampler: CG arm strict: cg strict (") for m in adv_lines)
    assert len(adv_lines) == 3  # advisory, suppressing, strict


def test_the_guard_passes_and_names_the_advisory(tmp_path):
    _run(tmp_path / "adv", _FakeArm(_with(_state(_reading(SP, -30.0), _reading(CGOBS)), _applies())), seconds=6.0)
    rc, line = _guard(_served_file(tmp_path, tmp_path / "adv", "adv.json"))
    assert rc == 0 and "verdict=MEASUREMENT" in line
    assert f" {TEXT} -- strict would be FOREIGN: cg-audio: 'RESOLUME-SNV (SP-program)'" in line

    _run(tmp_path / "quiet", _FakeArm(_with(_state(), _applies())), seconds=6.0)
    rc, line = _guard(_served_file(tmp_path, tmp_path / "quiet", "quiet.json"))
    assert rc == 0 and line.endswith(TEXT)


def test_the_guard_stays_strict_when_a_proof_fails(tmp_path):
    _run(tmp_path / "s", _FakeArm(_with(_state(_reading(SP, -30.0), _reading(CGOBS)), _strict())), seconds=6.0)
    rc, line = _guard(_served_file(tmp_path, tmp_path / "s", "s.json"))
    assert rc == 1 and "reason=cg-audio: " in line and "cg advisory" not in line


def test_the_guard_note_is_display_only():
    base = {"verdict": "MEASUREMENT", "age_s": 1.0, "marker_chain": 8, "markers_decoded": 9,
            "rms_dbfs": -40.0, "outside_band_pct": 15.0}
    assert guard.advisory_note(base) is None
    assert guard.advisory_note(dict(base, cg={"advisory": {"applies": False, "text": "cg strict (x)"}})) is None
    payload = dict(base, cg={"advisory": {"applies": True, "text": TEXT, "suppressed": None}})
    code, line = guard.decide(payload, 10.0)
    assert code == 0 and line.endswith(" " + TEXT)
    code, line = guard.decide(dict(payload, verdict="FOREIGN"), 10.0)
    assert code == 1 and "cg advisory" not in line  # never downgraded, never named on a refusal
    code, line = guard.decide(dict(payload, age_s=30.0), 10.0)
    assert code == 2 and "cg advisory" not in line  # a stale reading never shows it
