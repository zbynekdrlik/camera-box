"""#1003 -- unit tests for scripts/apply_latency_pins.py, the deliberate PROMOTE-the-baseline
apply tool (the WRITER counterpart to latency_pins_verify.py's REPORT-ONLY drift check).

The tool reads the committed drift-guard baseline (scripts/latency-pins-baseline.json) and PUSHES
its per-source genlock_latency_ms_src pins onto a live strih/stream box over OBS WS -- DRY-RUN by
default, --execute to write, idempotent, read-back verified, fail-loud on mismatch, imag REFUSED.

Tier-0: all logic is pure or exercised against a FAKE ws stub (the same
`monkeypatch.setattr(mod, "_rpc", fake)` convention test_latency_pins_verify.py /
test_imag_latency_enforce.py use) -- no rig, no cargo. The last class also LOCKS the #1003
promotion: the committed production baseline MUST equal the measurement-eq resolver's output, so a
hand-edit that drifts the baseline off its derivation (or a profile re-derivation without a
re-promotion) fails here rather than silently on the rig.
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import apply_latency_pins as alp  # noqa: E402

GENLOCK = "genlock_latency_ms_src"


# ---------------------------------------------------------------------------
# explicit_pins_for_box -- pure (extract named pins, reject the imag floor sentinel)
# ---------------------------------------------------------------------------
class TestExplicitPinsForBox:
    def test_strih_extracts_named_int_pins_skipping_comments(self):
        box = {"_comment": "note", "NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}
        assert alp.explicit_pins_for_box("strih", box) == {
            "NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}

    def test_stream_extracts_want_ms_from_a_band_spec(self):
        box = {"_comment": "x", "NDI 2ME PGM": {"want_ms": 791, "tolerance_ms": 60}}
        assert alp.explicit_pins_for_box("stream", box) == {"NDI 2ME PGM": 791}

    def test_imag_floor_sentinel_is_refused(self):
        # imag is the 3ms floor mandate (imag-min-latency-3ms-always) -- NEVER promoted here.
        with pytest.raises(SystemExit):
            alp.explicit_pins_for_box("imag", {"_all_ndi_inputs_ms": 3})

    def test_empty_named_pins_is_refused(self):
        # a box with only comments (no real pin) is a config error, never a vacuous no-op apply.
        with pytest.raises(SystemExit):
            alp.explicit_pins_for_box("strih", {"_comment": "only a comment"})


# ---------------------------------------------------------------------------
# plan_pin_changes -- pure (noop vs set decision; None live = still needs set)
# ---------------------------------------------------------------------------
class TestPlanPinChanges:
    def test_all_equal_are_noops(self):
        plan = alp.plan_pin_changes({"NDI cam1": 90}, {"NDI cam1": 90})
        assert plan == [{"source": "NDI cam1", "live_ms": 90, "want_ms": 90, "action": "noop"}]

    def test_different_is_a_set(self):
        plan = alp.plan_pin_changes({"NDI cam1": 90}, {"NDI cam1": 3})
        assert plan[0]["action"] == "set"
        assert plan[0]["live_ms"] == 3 and plan[0]["want_ms"] == 90

    def test_unreadable_live_is_a_set(self):
        plan = alp.plan_pin_changes({"NDI cam1": 90}, {"NDI cam1": None})
        assert plan[0]["action"] == "set" and plan[0]["live_ms"] is None

    def test_missing_live_key_is_a_set(self):
        plan = alp.plan_pin_changes({"NDI cam1": 90}, {})
        assert plan[0]["action"] == "set" and plan[0]["live_ms"] is None


# ---------------------------------------------------------------------------
# apply_pins -- fake ws + monkeypatched _rpc
# ---------------------------------------------------------------------------
class _FakeWs:
    """A fake OBS WS: `pins` is the live {source: value|None}. When `stuck`, a SetInputSettings is
    recorded but NEVER takes effect (simulates a source that won't accept the write)."""
    def __init__(self, pins, stuck=False):
        self.pins = dict(pins)
        self.stuck = stuck
        self.sets = []

    def close(self):
        pass


def _fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
    if rtype == "GetInputSettings":
        v = ws.pins.get(rdata["inputName"])
        return {"inputSettings": ({GENLOCK: v} if v is not None else {})}
    if rtype == "SetInputSettings":
        name = rdata["inputName"]
        val = rdata["inputSettings"][GENLOCK]
        ws.sets.append((name, val))
        if not ws.stuck:
            ws.pins[name] = val
        return {}
    raise AssertionError(f"unexpected rpc {rtype}")


class TestApplyPins:
    def test_dry_run_never_writes(self, monkeypatch):
        monkeypatch.setattr(alp, "_rpc", _fake_rpc)
        ws = _FakeWs({"NDI cam1": 3, "NDI cam2": 6})
        results = alp.apply_pins(ws, {"NDI cam1": 90, "NDI cam2": 160}, execute=False)
        assert ws.sets == []  # DRY-RUN wrote NOTHING
        assert all(r["action"] == "planned" for r in results)
        assert ws.pins == {"NDI cam1": 3, "NDI cam2": 6}  # live untouched

    def test_execute_sets_and_readback_verifies(self, monkeypatch):
        monkeypatch.setattr(alp, "_rpc", _fake_rpc)
        ws = _FakeWs({"NDI cam1": 3, "NDI cam2": 160})
        results = alp.apply_pins(ws, {"NDI cam1": 90, "NDI cam2": 160}, execute=True)
        by = {r["source"]: r for r in results}
        assert by["NDI cam1"]["action"] == "applied" and by["NDI cam1"]["after_ms"] == 90
        assert by["NDI cam2"]["action"] == "noop"      # already on-baseline -> no write
        assert ws.sets == [("NDI cam1", 90)]           # only the drifted one was written
        assert ws.pins["NDI cam1"] == 90

    def test_execute_readback_mismatch_fails_loud(self, monkeypatch):
        monkeypatch.setattr(alp, "_rpc", _fake_rpc)
        ws = _FakeWs({"NDI cam1": 3}, stuck=True)  # the set never takes effect
        with pytest.raises(SystemExit):
            alp.apply_pins(ws, {"NDI cam1": 90}, execute=True)
        assert ws.sets == [("NDI cam1", 90)]  # it TRIED to write, then fail-loud on read-back



# ---------------------------------------------------------------------------
# the committed baseline is the REVERTED shallow drift-guard reference (#1003 owner rework)
# ---------------------------------------------------------------------------
class TestRevertedBaseline:
    """The owner REJECTED + REVERTED the deep promoted 90/160/184 + 791 set (2026-08-20): those
    absolute depths add ~180 ms of needless chain latency. Production alignment is now the per-run
    floor-3 auto-align (scripts/qr_align_pins.py); the committed baseline is the reverted SHALLOW
    3/6/20 drift-guard REFERENCE only, never a hand-baked deep set. This locks the reverted state so
    a future accidental re-promotion of the deep numbers fails HERE, not silently on the rig."""

    def _load(self):
        return json.loads((_SCRIPTS / "latency-pins-baseline.json").read_text(encoding="utf-8"))

    def test_strih_pins_are_the_reverted_shallow_set_not_the_rejected_deep_set(self):
        strih = self._load()["strih"]
        assert strih["NDI cam1"] == 3
        assert strih["NDI cam2"] == 6
        assert strih["NDI cam3"] == 20
        assert (strih["NDI cam1"], strih["NDI cam2"], strih["NDI cam3"]) != (90, 160, 184)

    def test_stream_hold_is_not_the_rejected_deep_reduced_value(self):
        pgm = self._load()["stream"]["NDI 2ME PGM"]
        # never the rejected coherently-lowered 791 -- floor-3 never lowers the operator hold by an
        # absolute depth (the A/V-align hold stays the operator's domain).
        assert pgm["want_ms"] != 791
        assert pgm["tolerance_ms"] == 60

    def test_imag_floor_untouched(self):
        assert self._load()["imag"]["_all_ndi_inputs_ms"] == 3

    def test_extract_on_the_real_baseline(self):
        base = self._load()
        assert alp.explicit_pins_for_box("strih", base["strih"]) == {
            "NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        with pytest.raises(SystemExit):
            alp.explicit_pins_for_box("imag", base["imag"])


# ---------------------------------------------------------------------------
# --pins: push a COMPUTED {source: ms} set (the floor-3 aligner's per-run plan path)
# ---------------------------------------------------------------------------
class TestPinsFromArg:
    def test_inline_json_object(self):
        assert alp.pins_from_arg('{"NDI cam1": 3, "NDI cam2": 23}') == {
            "NDI cam1": 3, "NDI cam2": 23}

    def test_from_file(self, tmp_path):
        p = tmp_path / "pins.json"
        p.write_text('{"NDI cam3": 45}', encoding="utf-8")
        assert alp.pins_from_arg(f"@{p}") == {"NDI cam3": 45}

    def test_float_is_coerced_to_int(self):
        assert alp.pins_from_arg('{"NDI cam1": 3.0}') == {"NDI cam1": 3}

    def test_empty_object_is_refused(self):
        with pytest.raises(SystemExit):
            alp.pins_from_arg("{}")

    def test_non_number_value_is_refused(self):
        with pytest.raises(SystemExit):
            alp.pins_from_arg('{"NDI cam1": "deep"}')

    def test_bool_value_is_refused(self):
        with pytest.raises(SystemExit):
            alp.pins_from_arg('{"NDI cam1": true}')

    def test_imag_floor_sentinel_key_is_refused(self):
        # #1003 review: --pins only pushes NAMED strih inputs; the imag floor sentinel (or any
        # underscore/comment key) must be refused so this writer can never emit an imag floor pin.
        with pytest.raises(SystemExit):
            alp.pins_from_arg('{"_all_ndi_inputs_ms": 3}')
        with pytest.raises(SystemExit):
            alp.pins_from_arg('{"_comment": 5, "NDI cam1": 3}')

    def test_malformed_json_is_refused(self):
        with pytest.raises(SystemExit):
            alp.pins_from_arg("{not json")


class TestMainWithPins:
    def test_execute_pins_bypasses_baseline_and_writes_the_computed_set(self, monkeypatch):
        ws = _FakeWs({"NDI cam1": 3, "NDI cam2": 6})
        monkeypatch.setattr(alp, "_rpc", _fake_rpc)
        monkeypatch.setattr(alp, "_conn", lambda host, pw="": ws)
        rc = alp.main(["--box", "strih", "--host", "1.2.3.4",
                       "--pins", '{"NDI cam1": 3, "NDI cam2": 23}', "--execute"])
        assert rc == 0
        assert ws.pins["NDI cam1"] == 3    # already on -> noop
        assert ws.pins["NDI cam2"] == 23   # the computed floor-3 delta applied

    def test_dry_run_pins_writes_nothing(self, monkeypatch):
        ws = _FakeWs({"NDI cam1": 3, "NDI cam2": 6})
        monkeypatch.setattr(alp, "_rpc", _fake_rpc)
        monkeypatch.setattr(alp, "_conn", lambda host, pw="": ws)
        rc = alp.main(["--box", "strih", "--host", "1.2.3.4",
                       "--pins", '{"NDI cam1": 3, "NDI cam2": 23}'])
        assert rc == 0
        assert ws.sets == []               # DRY-RUN default -- nothing written
        assert ws.pins["NDI cam2"] == 6
