"""#1299 — the genlock_lock feeder facet in scripts/rig-health-audit.py + its generic render.

rig-status-page.md: a facet lives in the FEEDER (rig-health-audit.py), and rig-status's GENERIC
key=value parser renders it as a chip with zero renderer code. These pin (a) the pure
genlock_lock_state_from_log helper (newest `genlock-lock: state=X` line wins; absent -> ""), and
(b) that a feeder detail carrying a `genlock_lock=<state>` token surfaces as a rig-status chip.
"""
import importlib.util
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load(name, fname):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / fname)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_audit = _load("rig_health_audit", "rig-health-audit.py")
_status = _load("rig_status", "rig-status.py")


def test_state_helper_absent_is_empty():
    assert _audit.genlock_lock_state_from_log("") == ""
    assert _audit.genlock_lock_state_from_log("some unrelated\nlog lines\n") == ""


def test_state_helper_reads_newest_line():
    log = (
        "10:00:00.000: genlock-lock: state=LOCKED inputs=7/7 latency_ms=3 clock=locked output=stamping reason=none (#1298)\n"
        "10:05:00.000: unrelated line\n"
        "10:10:00.000: genlock-lock: state=UNLOCKED inputs=0/7 latency_ms=0 clock=absent output=not-stamping reason=clock (#1298)\n"
    )
    assert _audit.genlock_lock_state_from_log(log) == "UNLOCKED"


def test_state_helper_single_line():
    log = "x: genlock-lock: state=DEGRADED inputs=6/7 latency_ms=3 clock=locked output=stamping reason=input_unlocked (#1298)\n"
    assert _audit.genlock_lock_state_from_log(log) == "DEGRADED"


def test_rig_status_renders_genlock_lock_chip_generically():
    # a feeder detail carrying the token must surface as a plain key=value chip (no renderer code).
    line = "[PASS] strih   obs64=1 render=60.0fps/5.0ms skip=0.00% audio_buf=0ms arrivals[CAM1=60] steprate=n/a genlock_lock=LOCKED"
    recs = _status.parse_audit(line)
    assert len(recs) == 1
    chips = {f["key"]: f["value"] for f in recs[0]["facets"] if "key" in f}
    assert chips.get("genlock_lock") == "LOCKED", chips


def test_state_helper_prefers_the_newer_heartbeat_json_over_a_stale_change_line():
    # issue 1360: the audit fetches head 600 + tail N of a long strih-lx log. The change-driven
    # `genlock-lock: state=` line can sit between the two windows, so the only one in the text is
    # the startup UNLOCKED from the head. The 30 s `genlock-lock-json:` heartbeat is always in the
    # tail and carries the current decided state -- the LAST line of either kind wins.
    log = (
        "12:37:01.000: genlock-lock: state=UNLOCKED inputs=0/9 latency_ms=3 clock=absent output=not-stamping reason=clock (#1298)\n"
        "13:27:00.503: genlock-lock-json: {\"v\":6,\"state\":\"LOCKED\",\"reason\":\"none\",\"n_inputs\":9}\n"
        "13:27:30.503: genlock-lock-json: {\"v\":6,\"state\":\"LOCKED\",\"reason\":\"none\",\"n_inputs\":9}\n"
    )
    assert _audit.genlock_lock_state_from_log(log) == "LOCKED"


def test_state_helper_newer_change_line_beats_an_older_heartbeat():
    log = (
        "13:27:00.503: genlock-lock-json: {\"v\":6,\"state\":\"LOCKED\",\"reason\":\"none\"}\n"
        "13:27:10.000: genlock-lock: state=DEGRADED inputs=9/9 latency_ms=3 clock=locked output=stamping reason=recent_event (#1298)\n"
    )
    assert _audit.genlock_lock_state_from_log(log) == "DEGRADED"
