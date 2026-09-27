"""issue 1242 -- the E2E hold takes every `MV NDI camN` monitor twin OFF THE WIRE for the run.

Finding 5859213950: during an E2E the connect-on-show hold keeps the 7 mains connected while the 7
always-connected monitor twins (~58 Mbps each at NDI "lowest") keep streaming, and the strih-lx
uplink tail-drops. Design 5859315296 (the lane's option 1): the SAME hold writes
`{"genlock_fifo": false, "ndi_bw_mode": <audio-only>}` to every twin -- outside the #150 genlock
lockdown the certified coercion (which pins a monitor twin to LOWEST on EVERY update) does not run,
so audio-only sticks and DistroAV's own receiver reset takes the video off the wire. OBS applies the
input update one video tick AFTER the WS overlay, so an immediate read-back lies: every read-back is
a bounded settle poll. The restore puts the recorded original back (genlock on -> the coercion puts
LOWEST back itself) and verifies after the tick.

Covers:
  * the pure twin-hold values/predicates (scripts/strih_bandwidth_roles.py) pinned to the vendored
    DistroAV constants;
  * scripts/obs_phase2.py connect-on-show hold/restore of the twins against a fake OBS that models the
    deferred update + the certified coercion (original recorded BEFORE the write, the settle poll, a
    non-settling twin fails the hold, union with a leftover file keeps the recorded original, restore
    order + verify-after-tick, a deleted twin skipped, the legacy list state file);
  * every received= / lock consumer that runs during the hold reads a held twin as held by design:
    the hold-state reader in scripts/lib/connect-on-show-hold.sh waits on the MAINS only, the
    [4c/8] / live freeze watch / [4j/8settle] / mv-reverify / ndi-cadence input sets never name a twin,
    the in-OBS LOCK widget skips a non-genlock source, hidden_by_design SKIPs a held twin.
"""
import importlib.util
import json
import pathlib
import re
import subprocess
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
NDI_SOURCE_CPP = REPO / "vendor" / "distroav" / "src" / "ndi-source.cpp"
STATUSBAR_CPP = REPO / "vendor" / "obs-studio" / "frontend" / "widgets" / "OBSBasicStatusBar.cpp"


def _load(name, path):
    sys.path.insert(0, str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


roles = _load("strih_bandwidth_roles_twinhold", SCRIPTS / "strih_bandwidth_roles.py")
op = _load("obs_phase2_twinhold", SCRIPTS / "obs_phase2.py")

MAIN_ON = {"genlock_connect_on_show": True, "genlock_fifo": True, "ndi_source_name": "CAM1 (usb)"}
TWIN_ON_WIRE = {"genlock_monitor": True, "genlock_fifo": True, "ndi_bw_mode": 1,
                "genlock_connect_on_show": False, "ndi_source_name": "CAM1 (usb)"}


# ------------------------------------------------------------------------------------------------
# the pure values, pinned to the vendored DistroAV
# ------------------------------------------------------------------------------------------------

def _cpp_define(name):
    m = re.search(r"#define\s+%s\s+(-?\d+)" % name, NDI_SOURCE_CPP.read_text())
    assert m, name
    return int(m.group(1))


def test_twin_hold_values_match_the_vendored_distroav():
    assert roles.NDI_BW_LOWEST == _cpp_define("PROP_BW_LOWEST")
    assert roles.NDI_BW_AUDIO_ONLY == _cpp_define("PROP_BW_AUDIO_ONLY")
    assert roles.E2E_TWIN_HOLD == {"genlock_fifo": False, "ndi_bw_mode": roles.NDI_BW_AUDIO_ONLY}
    assert roles.TWIN_ON_WIRE == {"genlock_fifo": True, "ndi_bw_mode": roles.NDI_BW_LOWEST}
    src = NDI_SOURCE_CPP.read_text()
    # the coercion the hold steps around: gated on the genlock lockdown, LOWEST for a monitor twin
    assert "if (genlock_lockdown) {\n\t\tforce_genlock_certified_settings(settings);" in src
    assert "obs_data_set_int(settings, PROP_BANDWIDTH, PROP_BW_LOWEST);" in src
    # genlock_fifo DEFAULTS to true in this build: an absent key is a genlocked (on-wire) twin
    assert "obs_data_set_default_bool(settings, PROP_GENLOCK_FIFO, true);" in src


def test_twin_targets_are_the_role_marked_monitor_inputs():
    settings = {
        "NDI cam1": dict(MAIN_ON),
        "MV NDI cam1": dict(TWIN_ON_WIRE),
        "MV NDI cam3": {"genlock_monitor": True},
        "MV legacy clone": {"genlock_fifo": True},  # an `MV ` name WITHOUT the monitor role
        "cam monitor": {"genlock_monitor": True},    # a monitor flag without the twin name
        "NDI 2ME PVW": {},
    }
    assert roles.twin_hold_targets(settings) == ["MV NDI cam1", "MV NDI cam3"]


def test_twin_is_held_reads_the_hold_shape_only():
    held = dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=2)
    assert roles.twin_is_held(held)
    assert not roles.twin_is_held(TWIN_ON_WIRE)
    assert not roles.twin_is_held(dict(held, genlock_monitor=False))  # not a twin
    assert not roles.twin_is_held(dict(held, ndi_bw_mode=1))           # genlock off, still video
    no_key = {k: v for k, v in held.items() if k != "genlock_fifo"}
    assert not roles.twin_is_held(no_key), "absent genlock_fifo = the build default (true) = on the wire"


def test_twin_hold_original_never_records_the_held_values():
    assert roles.twin_hold_original(TWIN_ON_WIRE) == {"genlock_fifo": True, "ndi_bw_mode": 1}
    # a genlocked monitor twin: the coercion owns the bandwidth -> LOWEST, whatever was read
    assert roles.twin_hold_original(dict(TWIN_ON_WIRE, ndi_bw_mode=0)) == roles.TWIN_ON_WIRE
    # already HELD (a leftover hold whose state file was lost): the role values, never "held"
    assert roles.twin_hold_original(dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=2)) == \
        roles.TWIN_ON_WIRE
    # a twin that was NOT genlocked keeps its own bandwidth
    assert roles.twin_hold_original(dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=0)) == \
        {"genlock_fifo": False, "ndi_bw_mode": 0}


# ------------------------------------------------------------------------------------------------
# a fake OBS: the WS overlay lands at once, the input UPDATE (and the #150 coercion) one tick later
# ------------------------------------------------------------------------------------------------

DEFAULTS = {"genlock_fifo": True, "ndi_bw_mode": 0, "genlock_monitor": False,
            "genlock_connect_on_show": False}


class FakeObsTick:
    """obs-websocket over a clock. SetInputSettings overlays the explicit settings at once (libobs
    obs_data_apply) and marks the input for a deferred update; the update runs on the next video tick
    (any clock advance) and applies the vendored lockdown: genlock on -> bandwidth HIGHEST, then LOWEST
    for a monitor twin. `always_coerce` models an OBS whose coercion would ignore genlock_fifo=False;
    `unreadable` inputs answer GetInputSettings with a request error (ignore_err -> {}), and
    `unreadable_after_write` ones start doing so once written; `read_cost` is the fake seconds one
    GetInputSettings round trip takes (a slow WebSocket)."""

    def __init__(self, inputs, always_coerce=(), ignore_writes=(), unreadable=(),
                 unreadable_after_write=(), read_cost=0.0):
        self.inputs = {n: dict(s) for n, s in inputs.items()}
        self.t = 0.0
        self.pending = set()
        self.always_coerce = set(always_coerce)
        self.ignore_writes = set(ignore_writes)
        self.unreadable = set(unreadable)
        self.unreadable_after_write = set(unreadable_after_write)
        self.read_cost = float(read_cost)
        self.written = set()
        self.calls = []
        self.reads = []  # (t, name) of every GetInputSettings

    def clock(self):
        return self.t

    def sleep(self, dt):
        self.t += max(float(dt), 0.0)
        for n in sorted(self.pending):
            s = self.inputs.get(n)
            if s is None:
                continue
            eff = dict(DEFAULTS, **s)
            if eff["genlock_fifo"] or n in self.always_coerce:
                s["ndi_bw_mode"] = 0
                if eff["genlock_monitor"]:
                    s["ndi_bw_mode"] = 1
        self.pending.clear()

    def rpc(self, ws, rt, rdata=None, ignore_err=False, timeout_s=None):
        rdata = rdata or {}
        self.calls.append((self.t, rt, dict(rdata)))
        if rt == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": "ndi_source"} for n in self.inputs]}
        if rt == "GetInputSettings":
            self.reads.append((self.t, rdata["inputName"]))
            self.t += self.read_cost
            n = rdata["inputName"]
            if n in self.unreadable or (n in self.unreadable_after_write and n in self.written):
                if ignore_err:
                    return {}
                raise RuntimeError("request failed")
            s = self.inputs.get(n)
            if s is None:
                if ignore_err:
                    return {}
                raise RuntimeError("no such input")
            return {"inputSettings": dict(s)}
        if rt == "GetInputDefaultSettings":
            return {"defaultInputSettings": dict(DEFAULTS)}
        if rt == "SetInputSettings":
            n = rdata["inputName"]
            if n not in self.inputs or n in self.ignore_writes:
                return {}
            self.inputs[n].update(rdata["inputSettings"])
            self.pending.add(n)
            self.written.add(n)
            return {}
        raise AssertionError(rt)


def _rig(**over):
    inputs = {
        "NDI cam1": dict(MAIN_ON),
        "NDI cam3": dict(MAIN_ON, ndi_source_name="CAM3 (usb)"),
        "MV NDI cam1": dict(TWIN_ON_WIRE),
        "MV NDI cam3": dict(TWIN_ON_WIRE, ndi_source_name="CAM3 (usb)"),
        "NDI 2ME PVW": {"genlock_fifo": False, "ndi_bw_mode": 0},
    }
    inputs.update(over)
    return inputs


@pytest.fixture
def fake(monkeypatch):
    def make(inputs=None, **kw):
        f = FakeObsTick(inputs if inputs is not None else _rig(), **kw)
        monkeypatch.setattr(op, "_rpc", f.rpc)
        monkeypatch.setattr(op, "_settle_sleep", f.sleep)
        monkeypatch.setattr(op, "_settle_clock", f.clock)
        return f
    return make


def _twin(f, name):
    return dict(DEFAULTS, **f.inputs[name])


def _settle_gap(f):
    """Fake seconds between the LAST SetInputSettings and the first GetInputSettings after it (call
    order, not time: the enumeration reads happen at the same fake instant as the writes)."""
    idx = max(i for i, c in enumerate(f.calls) if c[1] == "SetInputSettings")
    t_write = f.calls[idx][0]
    t_read = next(c[0] for c in f.calls[idx + 1:] if c[1] == "GetInputSettings")
    return t_read - t_write


# ------------------------------------------------------------------------------------------------
# obs_phase2: hold + restore of the twins
# ------------------------------------------------------------------------------------------------

def test_hold_takes_every_monitor_twin_off_the_wire(fake, tmp_path):
    f = fake()
    sf = tmp_path / "hold.json"
    mains, twins, failed = op.connect_on_show_hold(None, str(sf))
    assert mains == ["NDI cam1", "NDI cam3"]
    assert twins == ["MV NDI cam1", "MV NDI cam3"]
    assert failed == []
    for tw in twins:
        # AFTER the deferred update ran: audio-only stuck because the lockdown is off
        assert _twin(f, tw)["genlock_fifo"] is False
        assert _twin(f, tw)["ndi_bw_mode"] == roles.NDI_BW_AUDIO_ONLY
        assert _twin(f, tw)["genlock_monitor"] is True, "the role flag is never touched"
    assert f.inputs["NDI cam1"]["genlock_connect_on_show"] is False
    assert f.inputs["NDI 2ME PVW"] == {"genlock_fifo": False, "ndi_bw_mode": 0}
    state = json.loads(sf.read_text())
    assert state["connect_on_show"] == ["NDI cam1", "NDI cam3"]
    assert state["twins"] == {"MV NDI cam1": {"genlock_fifo": True, "ndi_bw_mode": 1},
                              "MV NDI cam3": {"genlock_fifo": True, "ndi_bw_mode": 1}}


def test_hold_records_the_originals_before_any_write(fake, tmp_path, monkeypatch):
    f = fake()
    sf = tmp_path / "hold.json"
    seen = {}
    real = f.rpc

    def rpc(ws, rt, rdata=None, ignore_err=False, timeout_s=None):
        if rt == "SetInputSettings" and "first" not in seen:
            seen["first"] = json.loads(sf.read_text()) if sf.exists() else None
        return real(ws, rt, rdata, ignore_err, timeout_s)

    monkeypatch.setattr(op, "_rpc", rpc)
    op.connect_on_show_hold(None, str(sf))
    assert seen["first"] is not None, "the state file must exist before the first flag write"
    assert set(seen["first"]["twins"]) == {"MV NDI cam1", "MV NDI cam3"}
    assert seen["first"]["connect_on_show"] == ["NDI cam1", "NDI cam3"]


def test_hold_waits_for_the_input_update_and_fails_a_twin_that_does_not_settle(fake, tmp_path):
    # an OBS whose coercion still pins MV NDI cam3: the overlay reads back 2 at once (the lie), the
    # deferred update puts LOWEST back one tick later -> the hold must NOT accept it
    f = fake(always_coerce={"MV NDI cam3"})
    mains, twins, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert failed == ["MV NDI cam3"]
    assert _twin(f, "MV NDI cam3")["ndi_bw_mode"] == 1
    # the read-back starts at least one settle interval after the writes (never an immediate read)
    assert _settle_gap(f) >= op._SETTLE_MIN_S > 0


def test_hold_settles_the_mains_before_any_twin_goes_off_the_wire(fake, tmp_path):
    f = fake()
    op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    sets = [(i, c[2]["inputName"]) for i, c in enumerate(f.calls) if c[1] == "SetInputSettings"]
    last_main = max(i for i, n in sets if not n.startswith("MV "))
    first_twin = min(i for i, n in sets if n.startswith("MV "))
    assert last_main < first_twin
    main_reads = [c for c in f.calls[last_main + 1:first_twin]
                  if c[1] == "GetInputSettings" and not c[2]["inputName"].startswith("MV ")]
    assert len(main_reads) >= 4, "both mains read back twice before a twin goes off the wire"
    assert f.calls[first_twin][0] - f.calls[last_main][0] >= op._SETTLE_MIN_S


def test_hold_keeps_a_twin_on_the_wire_when_its_main_did_not_settle(fake, tmp_path):
    f = fake(ignore_writes={"NDI cam1"})
    sf = tmp_path / "hold.json"
    mains, twins, failed = op.connect_on_show_hold(None, str(sf))
    assert failed == ["NDI cam1"]
    assert "MV NDI cam1" not in twins and _twin(f, "MV NDI cam1")["genlock_fifo"] is True
    assert "MV NDI cam3" in twins and _twin(f, "MV NDI cam3")["genlock_fifo"] is False
    assert "MV NDI cam1" in json.loads(sf.read_text())["twins"], "recorded before the writes"


def test_restore_keeps_the_main_held_while_its_twin_is_still_off_the_wire(fake, tmp_path):
    f = fake()
    sf = tmp_path / "hold.json"
    op.connect_on_show_hold(None, str(sf))
    f.ignore_writes.add("MV NDI cam1")
    restored, failed, held_back = op.connect_on_show_restore(None, str(sf))
    assert failed == ["MV NDI cam1"]
    assert held_back == ["NDI cam1"], "the restore names the main it kept held"
    # never a camera with neither receiver: full bandwidth on the main is the fail-safe
    assert f.inputs["NDI cam1"]["genlock_connect_on_show"] is False
    assert f.inputs["NDI cam3"]["genlock_connect_on_show"] is True
    assert "NDI cam1" not in restored and "NDI cam3" in restored
    state = json.loads(sf.read_text())
    assert "NDI cam1" in state["connect_on_show"] and "MV NDI cam1" in state["twins"]
    f.ignore_writes.clear()
    restored, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert failed == [] and not sf.exists()
    assert f.inputs["NDI cam1"]["genlock_connect_on_show"] is True


def test_an_unreadable_input_at_enumeration_fails_the_hold(fake, tmp_path):
    f = fake(unreadable={"MV NDI cam3"})
    _, twins, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert "MV NDI cam3" in failed, "a twin the hold could not read may still be on the wire"
    assert "MV NDI cam3" not in twins and "MV NDI cam1" in twins
    assert "MV NDI cam3" not in f.written, "an input of unknown role is never written"


def test_the_twin_of_an_unreadable_main_stays_on_the_wire(fake, tmp_path):
    # review round 2: a main the hold could not read may still be parked -- its twin stays its one
    # configured receiver
    f = fake(unreadable={"NDI cam3"})
    _, twins, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert "NDI cam3" in failed
    assert "MV NDI cam3" not in twins and "MV NDI cam3" not in f.written
    assert "MV NDI cam1" in twins


def _cli(monkeypatch, capsys, **ns):
    class _Ws:
        def close(self):
            pass

    monkeypatch.setattr(op, "_conn", lambda host, pw: _Ws())
    args = dict({"host": "h", "password": "", "hold": None, "restore": None}, **ns)
    code = 0
    try:
        op.connect_on_show(type("A", (), args))
    except SystemExit as e:
        code = e.code
    out = capsys.readouterr()
    return code, out.out, out.err


def test_hold_cli_names_the_held_inputs_and_the_failures_apart(fake, tmp_path, monkeypatch, capsys):
    fake(ignore_writes={"NDI cam1"})
    code, out, err = _cli(monkeypatch, capsys, hold=str(tmp_path / "h.json"))
    assert code == 1
    held_line = out.split("held")[1]
    assert "NDI cam1" not in held_line.split(";")[0], "a main that failed is not reported held"
    assert "NDI cam3" in out and "NDI cam1" in err
    assert "restore" not in err, "a hold failure never prints the restore's text"


def test_restore_cli_names_the_mains_it_kept_held(fake, tmp_path, monkeypatch, capsys):
    f = fake()
    sf = tmp_path / "h.json"
    op.connect_on_show_hold(None, str(sf))
    f.ignore_writes.add("MV NDI cam1")
    code, out, err = _cli(monkeypatch, capsys, restore=str(sf))
    assert code == 1
    assert "MV NDI cam1" in err
    assert "NDI cam1" in err.replace("MV NDI cam1", ""), "the held-back main is named"


def test_an_unreadable_read_back_never_counts_as_settled(fake, tmp_path):
    # the main's hold target equals the type default, so a failed read must not look like a match
    f = fake(unreadable_after_write={"NDI cam3"})
    _, _, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert failed == ["NDI cam3"]
    assert "MV NDI cam3" not in f.written, "its twin stays on the wire (the main is unconfirmed)"


def test_a_slow_websocket_still_gets_two_full_sweeps(fake, tmp_path):
    f = fake(read_cost=3.0)  # one sweep of the mains alone outlasts the 5 s budget
    _, twins, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert failed == [] and twins == ["MV NDI cam1", "MV NDI cam3"]
    assert f.t > op._SETTLE_BUDGET_S, "the scenario really ran past the budget"


def test_the_settle_poll_reads_the_type_defaults_once_per_settle(fake, tmp_path):
    f = fake()
    op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    # one for the enumeration, one per settle (mains, twins) -- never one per input per sweep
    assert sum(1 for c in f.calls if c[1] == "GetInputDefaultSettings") <= 3


def test_the_settle_poll_is_bounded(fake, tmp_path):
    f = fake(always_coerce={"MV NDI cam1", "MV NDI cam3"})
    start = f.t
    _, _, failed = op.connect_on_show_hold(None, str(tmp_path / "hold.json"))
    assert failed == ["MV NDI cam1", "MV NDI cam3"]
    # the mains settle quickly, the never-settling twins end at the budget
    one = op._SETTLE_BUDGET_S + op._SETTLE_POLL_S + op._SETTLE_MIN_S
    assert f.t - start <= 2 * one


def test_hold_cli_exits_non_zero_when_a_twin_does_not_settle(fake, tmp_path, monkeypatch, capsys):
    fake(always_coerce={"MV NDI cam1"})

    class _Ws:
        def close(self):
            pass

    monkeypatch.setattr(op, "_conn", lambda host, pw: _Ws())
    ns = type("A", (), {"host": "h", "password": "", "hold": str(tmp_path / "h.json"), "restore": None})
    with pytest.raises(SystemExit) as e:
        op.connect_on_show(ns)
    assert e.value.code == 1
    assert "MV NDI cam1" in capsys.readouterr().err


def test_a_rehold_keeps_the_recorded_original_of_a_leftover(fake, tmp_path):
    # a SIGKILLed run left its state file AND its twins held
    f = fake(_rig(**{"MV NDI cam3": dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=2)}))
    sf = tmp_path / "hold.json"
    sf.write_text(json.dumps({"connect_on_show": ["NDI cam9"],
                              "twins": {"MV NDI cam3": {"genlock_fifo": True, "ndi_bw_mode": 1},
                                        "MV NDI cam9": {"genlock_fifo": True, "ndi_bw_mode": 1}}}))
    mains, twins, failed = op.connect_on_show_hold(None, str(sf))
    assert failed == []
    state = json.loads(sf.read_text())
    assert state["connect_on_show"] == ["NDI cam1", "NDI cam3", "NDI cam9"]  # the union
    assert state["twins"]["MV NDI cam3"] == {"genlock_fifo": True, "ndi_bw_mode": 1}
    assert "MV NDI cam9" in state["twins"], "a leftover twin stays recorded for the restore"
    # only PRESENT inputs are written (a deleted leftover never fails the hold)
    written = {d["inputName"] for _, rt, d in f.calls if rt == "SetInputSettings"}
    assert "MV NDI cam9" not in written and "NDI cam9" not in written
    assert "NDI cam9" not in mains and "MV NDI cam9" not in twins


def test_a_legacy_list_state_file_is_read_as_the_held_mains(fake, tmp_path):
    fake()
    sf = tmp_path / "hold.json"
    sf.write_text(json.dumps(["NDI cam1"]))
    assert op._read_hold_state(str(sf)) == (["NDI cam1"], {})
    op.connect_on_show_hold(None, str(sf))
    assert json.loads(sf.read_text())["connect_on_show"] == ["NDI cam1", "NDI cam3"]


def test_restore_puts_the_original_back_twins_first_and_verifies_after_the_tick(fake, tmp_path):
    f = fake()
    sf = tmp_path / "hold.json"
    op.connect_on_show_hold(None, str(sf))
    f.calls.clear()
    restored, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert failed == []
    assert set(restored) == {"NDI cam1", "NDI cam3", "MV NDI cam1", "MV NDI cam3"}
    for tw in ("MV NDI cam1", "MV NDI cam3"):
        assert _twin(f, tw)["genlock_fifo"] is True and _twin(f, tw)["ndi_bw_mode"] == 1
    assert f.inputs["NDI cam1"]["genlock_connect_on_show"] is True
    assert not sf.exists(), "a clean restore removes the state file"
    writes = [d["inputName"] for _, rt, d in f.calls if rt == "SetInputSettings"]
    # twins first: a camera never has neither a live main nor a live twin
    assert max(writes.index(t) for t in ("MV NDI cam1", "MV NDI cam3")) < \
        min(writes.index(m) for m in ("NDI cam1", "NDI cam3"))
    assert _settle_gap(f) >= op._SETTLE_MIN_S > 0


def test_twin_restore_values_let_the_lockdown_own_a_genlocked_bandwidth():
    # review round 1: the restore also re-asserts the monitor ROLE -- the lockdown pins LOWEST only for
    # a genlock_monitor source, so a twin that lost the flag would read back HIGHEST forever
    on_wire = dict(roles.TWIN_ON_WIRE, genlock_monitor=True)
    assert roles.twin_restore_values({"genlock_fifo": True, "ndi_bw_mode": 1}) == on_wire
    # a genlocked original with any other recorded bandwidth could never read back (the lockdown
    # pins LOWEST): it is restored on the wire
    assert roles.twin_restore_values({"genlock_fifo": True, "ndi_bw_mode": 0}) == on_wire
    assert roles.twin_restore_values({}) == on_wire
    # a HELD-shaped original (a hand-edited / older file) is never restored as "held"
    assert roles.twin_restore_values({"genlock_fifo": False, "ndi_bw_mode": 2}) == on_wire
    assert roles.twin_restore_values({"genlock_fifo": False, "ndi_bw_mode": 0}) == \
        {"genlock_fifo": False, "ndi_bw_mode": 0}


def test_restore_reasserts_the_monitor_role_so_the_lockdown_pins_lowest(fake, tmp_path):
    lost = {"genlock_fifo": False, "ndi_bw_mode": 2, "ndi_source_name": "CAM1 (usb)"}  # no monitor flag
    f = fake(_rig(**{"MV NDI cam1": lost}))
    sf = tmp_path / "hold.json"
    sf.write_text(json.dumps({"connect_on_show": [],
                              "twins": {"MV NDI cam1": {"genlock_fifo": True, "ndi_bw_mode": 1}}}))
    restored, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert failed == [] and restored == ["MV NDI cam1"]
    assert _twin(f, "MV NDI cam1")["genlock_monitor"] is True
    assert _twin(f, "MV NDI cam1")["ndi_bw_mode"] == 1 and _twin(f, "MV NDI cam1")["genlock_fifo"] is True


def test_restore_of_a_hand_edited_original_still_lands(fake, tmp_path):
    f = fake(_rig(**{"MV NDI cam1": dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=2)}))
    sf = tmp_path / "hold.json"
    sf.write_text(json.dumps({"connect_on_show": [],
                              "twins": {"MV NDI cam1": {"genlock_fifo": True, "ndi_bw_mode": 0}}}))
    restored, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert restored == ["MV NDI cam1"] and failed == []
    assert _twin(f, "MV NDI cam1")["genlock_fifo"] is True and _twin(f, "MV NDI cam1")["ndi_bw_mode"] == 1
    assert not sf.exists()


def test_restore_keeps_the_state_file_when_a_twin_does_not_settle(fake, tmp_path):
    f = fake()
    sf = tmp_path / "hold.json"
    op.connect_on_show_hold(None, str(sf))
    f.ignore_writes.add("MV NDI cam1")
    _, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert failed == ["MV NDI cam1"]
    assert sf.exists(), "an unlanded restore keeps the list for the next run's cleanup"


def test_restore_skips_a_deleted_twin(fake, tmp_path):
    f = fake()
    sf = tmp_path / "hold.json"
    op.connect_on_show_hold(None, str(sf))
    del f.inputs["MV NDI cam3"]
    restored, failed, _ = op.connect_on_show_restore(None, str(sf))
    assert failed == [] and "MV NDI cam3" not in restored
    assert not sf.exists()


# ------------------------------------------------------------------------------------------------
# the received= / lock consumers that run during the hold read a held twin as held by design
# ------------------------------------------------------------------------------------------------

LIB = SCRIPTS / "lib" / "connect-on-show-hold.sh"
PARK_LIB = SCRIPTS / "lib" / "genlock-park.sh"


def _bash(body, env=None):
    return subprocess.run(["bash", "-c", f"set -euo pipefail; . '{PARK_LIB}'; . '{LIB}'; {body}"],
                          capture_output=True, text=True, check=False, timeout=60,
                          env=env or {"PATH": "/usr/bin:/bin", "HOME": "/tmp"})


def test_wait_live_waits_on_the_held_mains_never_on_a_held_twin(tmp_path):
    state = tmp_path / "hold.json"
    state.write_text(json.dumps({"connect_on_show": ["NDI cam1"],
                                 "twins": {"MV NDI cam1": {"genlock_fifo": True, "ndi_bw_mode": 1}}}))
    cnt = tmp_path / "n"
    reader = tmp_path / "reader.sh"
    # the main advances; the held twin (non-genlock, audio-only) never logs an audit line again
    reader.write_text("#!/usr/bin/env bash\n"
                      f"n=$(cat '{cnt}' 2>/dev/null || echo 0); n=$((n+1)); printf '%s' \"$n\" > '{cnt}'\n"
                      "printf \"12:00:00.000: genlock-fifo audit 'NDI cam1': received=%s consumed=1\\n\" "
                      "\"$((100 + n * 60))\"\n")
    reader.chmod(0o755)
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "CONNECT_ON_SHOW_LOG_READ_CMD": str(reader),
           "CONNECT_ON_SHOW_LIVE_POLL_S": "0", "CONNECT_ON_SHOW_LIVE_WAIT_S": "3"}
    out = _bash(f"connect_on_show_e2e_wait_live /x 10.0.0.1 '{state}'; echo RC=$?", env=env)
    assert "RC=0" in out.stdout, out.stderr
    assert "every held input is delivering again" in out.stdout, out.stdout + out.stderr
    assert "MV NDI cam1" not in out.stderr and "twins" not in out.stderr


def test_the_bash_state_reader_and_obs_phase2_agree_on_the_held_mains(tmp_path):
    for body in (["NDI cam3", "NDI cam1"],
                 {"connect_on_show": ["NDI cam1", "NDI cam3"], "twins": {"MV NDI cam1": {}}}):
        sf = tmp_path / "s.json"
        sf.write_text(json.dumps(body))
        out = _bash(f"connect_on_show_held_mains '{sf}'")
        assert out.returncode == 0, out.stderr
        assert out.stdout.split("\n")[:-1] == op._read_hold_state(str(sf))[0] == ["NDI cam1", "NDI cam3"]
    assert _bash(f"connect_on_show_held_mains '{tmp_path}/absent.json'; echo RC=$?").stdout == "RC=0\n"


def _camera_set(fn, *args):
    cmd = f". '{SCRIPTS / 'camera-set.sh'}'; {fn} {' '.join(args)}"
    out = subprocess.run(["bash", "-c", cmd], capture_output=True, text=True, check=True, timeout=30,
                         env={"PATH": "/usr/bin:/bin", "HOME": "/tmp"})
    return out.stdout


def test_hold_time_input_sets_never_name_a_twin():
    # [4c/8] + the live freeze watch + [4j/8settle] derive their inputs from these helpers
    for fn, args in (("camera_active_ndi_sources_csv", ()),
                     ("camera_active_ndi_sources_excluding_csv", ("''",)),
                     ("camera_align_ndi_sources_excluding_csv", ("''",))):
        names = [n for n in _camera_set(fn, *args).split(",") if n]
        assert names and all(n.startswith("NDI cam") for n in names), (fn, names)
    e2e = (SCRIPTS / "recording-e2e.sh").read_text()
    assert '_FROZEN_CAM_SOURCES_EFFECTIVE="${FROZEN_CAM_SOURCES:-$(camera_active_ndi_sources_csv)}"' in e2e
    assert 'FREEZE_WATCH_SOURCES="$(camera_active_ndi_sources_excluding_csv' in e2e
    assert 'GENLOCK_SETTLE_WATCHED="$(camera_align_ndi_sources_excluding_csv' in e2e
    # mv-reverify-escalate reads the main of the camera it re-verifies
    assert 'local src="NDI cam${cam_n}"' in (SCRIPTS / "lib" / "mv-reverify-escalate.sh").read_text()
    # ndi-cadence-heal (cleanup) defaults to the mains
    cad = (SCRIPTS / "lib" / "ndi-cadence-heal.sh").read_text()
    default = re.search(r'NDI_CADENCE_INPUTS="\$\{NDI_CADENCE_INPUTS:-([^}]*)\}"', cad).group(1)
    assert "MV " not in default and "NDI cam1|60" in default
    assert "NDI_CADENCE_INPUTS" not in e2e, "the E2E cleanup keeps the mains-only default"


def test_the_lock_widget_never_counts_a_non_genlock_source():
    # a held twin runs with genlock_fifo=false: the LOCK indicator skips it (never unlocked/absent/
    # idle, never a recent_event offender) -- the genlock-lock facet is derived from this scan
    assert "if (!obs_source_get_genlock_stats(source, &st) || !st.genlock_fifo)" in STATUSBAR_CPP.read_text()


def test_hidden_by_design_skips_a_held_twin(monkeypatch):
    held = dict(TWIN_ON_WIRE, genlock_fifo=False, ndi_bw_mode=2)
    assert op.hidden_by_design(held, showing=True), "the multiview shows it, it is held by design"
    assert not op.hidden_by_design(TWIN_ON_WIRE, showing=True)

    def rpc(ws, rt, rdata=None, ignore_err=False, timeout_s=None):
        if rt == "GetInputSettings":
            return {"inputSettings": dict(held)}
        if rt == "GetSourceActive":
            return {"videoActive": True, "videoShowing": True}
        raise AssertionError(rt)

    monkeypatch.setattr(op, "_rpc", rpc)
    assert op.input_hidden_by_design(None, "MV NDI cam1")
