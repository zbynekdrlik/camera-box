"""#1197 — unit tests for set-ndi-mapping.py's bounded cold-finder discovery-wait heal: the pure
per-input probe (_discover_reenforce_once), the bounded-wall-clock loop (heal_wait_active_mapping)
and its exit-code contract (_heal_wait_exit_code). No live OBS and no real sleep — the loop takes an
injected op/ws/get_binding/log_err plus a fake now()/sleep(), and the fake finder list warms up over
"time" to model a cold DistroAV finder re-discovering a genuinely-live sender.
"""
import importlib.util
import pathlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"


def _load():
    spec = importlib.util.spec_from_file_location("set_ndi_mapping_heal_wait", _SCRIPTS / "set-ndi-mapping.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


m = _load()


class _Clock:
    """A fake monotonic clock: now() reads t; sleep(s) advances t by s (no real wall time)."""

    def __init__(self):
        self.t = 0.0
        self.slept = []

    def now(self):
        return self.t

    def sleep(self, s):
        self.slept.append(s)
        self.t += s


class _FakeOp:
    """Stand-in for obs_phase2: REENFORCE_* constants, a scripted reenforce_ndi_name, and a
    _ndi_source_list whose contents can DEPEND on the clock (a sender that appears after warm_at)."""

    REENFORCE_HEALED = "healed"
    REENFORCE_OFFLINE = "offline"
    REENFORCE_VERIFY_FAILED = "verify_failed"

    def __init__(self, finder_at, statuses, clock):
        # finder_at: input -> (baseline_name, appears_at_t)  (baseline is in the finder once t>=appears_at)
        self.finder_at = dict(finder_at)
        self.statuses = dict(statuses)  # input -> reenforce status (only consulted when a set is attempted)
        self.clock = clock
        self.reenforce_calls = []
        self.list_calls = []

    def _ndi_source_list(self, ws, inp):
        self.list_calls.append((inp, self.clock.t))
        name, appears_at = self.finder_at.get(inp, (None, None))
        return [name] if (name is not None and self.clock.t >= appears_at) else []

    def reenforce_ndi_name(self, ws, inp, desired):
        self.reenforce_calls.append((inp, desired, self.clock.t))
        return self.statuses.get(inp, self.REENFORCE_HEALED)


def _get_binding_from(current):
    def _g(ws, inp):
        return current.get(inp, "")
    return _g


# --- _heal_wait_exit_code contract -----------------------------------------------------------

def test_heal_wait_exit_code_verify_fail_is_1_even_with_others_done():
    assert m._heal_wait_exit_code(done=2, waiting=0, failed=1) == 1


def test_heal_wait_exit_code_all_done_is_0():
    assert m._heal_wait_exit_code(done=3, waiting=0, failed=0) == 0


def test_heal_wait_exit_code_timed_out_is_3():
    assert m._heal_wait_exit_code(done=1, waiting=2, failed=0) == 3


# --- _discover_reenforce_once ----------------------------------------------------------------

def test_probe_baseline_absent_is_waiting_and_never_sets():
    clk = _Clock()
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 999)}, {}, clk)  # never discoverable within reach
    r = m._discover_reenforce_once(op, object(), "NDI cam1", "CAM1 (usb)", _get_binding_from({"NDI cam1": ""}))
    assert r == "waiting"
    assert op.reenforce_calls == []  # never blind-set a name absent from the finder (#795 mangle ban)


def test_probe_discoverable_and_already_correct_is_done_without_setting():
    clk = _Clock()
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 0)}, {}, clk)  # in the finder now
    r = m._discover_reenforce_once(op, object(), "NDI cam1", "CAM1 (usb)",
                                   _get_binding_from({"NDI cam1": "CAM1 (usb)"}))
    assert r == "done"
    assert op.reenforce_calls == []  # never fight a healthy mapping


def test_probe_discoverable_but_empty_reenforces_and_is_done():
    clk = _Clock()
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 0)}, {"NDI cam1": "healed"}, clk)
    r = m._discover_reenforce_once(op, object(), "NDI cam1", "CAM1 (usb)", _get_binding_from({"NDI cam1": ""}))
    assert r == "done"
    assert [c[0] for c in op.reenforce_calls] == ["NDI cam1"]  # emptied name re-enforced


def test_probe_discoverable_drifted_but_verify_fails_is_failed():
    clk = _Clock()
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 0)}, {"NDI cam1": "verify_failed"}, clk)
    r = m._discover_reenforce_once(op, object(), "NDI cam1", "CAM1 (usb)",
                                   _get_binding_from({"NDI cam1": "CAM1 (usb) MANGLED"}))
    assert r == "failed"


# --- heal_wait_active_mapping (the bounded loop) ----------------------------------------------

def test_empty_want_returns_all_zero_no_sleep_no_probe():
    # #1197 review 🔵-3: an empty active set (--active "" from an unset CAMERA_ACTIVE_SET) has nothing
    # to warm -> (0,0,0) with zero probes/sleeps (the CLI mode additionally short-circuits before
    # opening a WS connection).
    clk = _Clock()
    op = _FakeOp({}, {}, clk)
    logs = []
    done, waiting, failed = m.heal_wait_active_mapping(
        op, object(), [], _get_binding_from({}), logs.append,
        deadline_s=90, interval_s=4, now=clk.now, sleep=clk.sleep)
    assert (done, waiting, failed) == (0, 0, 0)
    assert clk.slept == [] and op.list_calls == [] and op.reenforce_calls == []


def test_warm_finder_all_correct_returns_immediately_no_sleep_no_set():
    clk = _Clock()
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 0), "NDI cam2": ("CAM2 (usb)", 0)}, {}, clk)
    current = {"NDI cam1": "CAM1 (usb)", "NDI cam2": "CAM2 (usb)"}
    logs = []
    done, waiting, failed = m.heal_wait_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append,
        deadline_s=90, interval_s=4, now=clk.now, sleep=clk.sleep)
    assert (done, waiting, failed) == (2, 0, 0)
    assert clk.slept == []          # a warm finder pays no wait
    assert op.reenforce_calls == []  # nothing to heal


def test_cold_finder_that_warms_recovers_an_emptied_input():
    clk = _Clock()
    # cam1 was left EMPTY by the reattach; its sender re-appears in the finder only after t>=12s.
    want = [("NDI cam1", "CAM1 (usb)")]
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 12)}, {"NDI cam1": "healed"}, clk)
    current = {"NDI cam1": ""}
    logs = []
    done, waiting, failed = m.heal_wait_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append,
        deadline_s=90, interval_s=4, now=clk.now, sleep=clk.sleep)
    assert (done, waiting, failed) == (1, 0, 0)
    assert op.reenforce_calls, "the baseline must be re-enforced once the finder warmed up"
    # it must NOT have set before the sender was discoverable (every reenforce call is at t>=12)
    assert all(t >= 12 for _, _, t in op.reenforce_calls)


def test_cold_finder_never_warms_times_out_and_logs_loud():
    clk = _Clock()
    want = [("NDI cam1", "CAM1 (usb)")]
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 10000)}, {}, clk)  # never within the bound
    current = {"NDI cam1": ""}
    logs = []
    done, waiting, failed = m.heal_wait_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append,
        deadline_s=20, interval_s=4, now=clk.now, sleep=clk.sleep)
    assert (done, waiting, failed) == (0, 1, 0)
    assert clk.t <= 24  # bounded by the wall-clock deadline (deadline_s + at most one interval)
    assert any("NDI cam1" in ln and "STILL absent" in ln for ln in logs)
    assert op.reenforce_calls == []  # never mangled an undiscoverable name


def test_mixed_one_recovers_one_never_discoverable():
    clk = _Clock()
    want = [("NDI cam1", "CAM1 (usb)"), ("NDI cam2", "CAM2 (usb)")]
    op = _FakeOp({"NDI cam1": ("CAM1 (usb)", 8), "NDI cam2": ("CAM2 (usb)", 10000)},
                 {"NDI cam1": "healed"}, clk)
    current = {"NDI cam1": "", "NDI cam2": ""}
    logs = []
    done, waiting, failed = m.heal_wait_active_mapping(
        op, object(), want, _get_binding_from(current), logs.append,
        deadline_s=20, interval_s=4, now=clk.now, sleep=clk.sleep)
    assert (done, waiting, failed) == (1, 1, 0)
    # cam1 healed exactly once (not re-probed after it was marked done)
    assert [c[0] for c in op.reenforce_calls] == ["NDI cam1"]
