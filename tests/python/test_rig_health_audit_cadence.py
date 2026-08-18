"""#1089 -- unit tests for the non-60 source-cadence surfacing in
scripts/rig-health-audit.py (the #787 status-page sweep).

These pin the pure/near-pure helpers the cadence tier is built from:
  * `audit_samples()`      -- parse per-source (raw_ts, received) from the strih
                              OBS log's `genlock-fifo audit` lines.
  * `cadence_check()`      -- derive the camera source set from the log and, for
                              each, REUSE the tested bash kernel
                              scripts/lib/cadence-health.sh (cadence_measure_fps +
                              cadence_classify, the issue-797 phantom-50 avoidance)
                              to produce a per-camera OK/WRONG verdict. NO second
                              divisor implementation lives in the Python.
  * `box_verdict()`        -- the shared PASS/WARN/FAIL three-tier split.

The cadence check shells out to the REAL scripts/lib/cadence-health.sh, so these
tests also prove the Python<->bash reuse wiring end to end (no OBS / no ssh).
"""
import importlib.util
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "rig_health_audit", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


# --------------------------------------------------------------------------
# Synthetic strih OBS-log builder: one `genlock-fifo audit` line per (ts, src,
# received). Only the leading `HH:MM:SS.mmm:` prefix, the quoted source name,
# and `received=N` are load-bearing; the rest mimics a real line (incl. the
# useless `@ 30.000fps` CANVAS decoration the cadence tier must ignore).
# --------------------------------------------------------------------------
def _line(ts, src, recv):
    return (f"{ts}: genlock-fifo audit '{src}': received={recv} consumed=1 "
            f"underruns=0 locked=1 depth=2 latency_ms=3 (foo @ 30.000fps) "
            f"src_latency_ms=3 preload=1 (=33 ms) cap=30 empty_run=0")


# A ~120 s window (>= the 60 s min trustable span): first + last sample per source.
#   cam1 : 1000 -> 8200 over 120 s = 7200/120 = 60.0 fps  -> OK
#   cam2 :  500 -> 6500 over 120 s = 6000/120 = 50.0 fps  -> WRONG (mis-set 50)
#   2ME PGM (mv): 30 fps, but NOT a camera source -> excluded from the @60 check
#   cam3 : frozen (received flat) -> advanced=0 -> UNKNOWN (a freeze is #1052's job)
#   cam4 : 60 fps but only a 30 s window (< 60 s) -> UNKNOWN (never a shaky page)
_LOG = "\n".join([
    _line("14:00:00.000", "NDI cam1", 1000),
    _line("14:00:00.000", "NDI cam2", 500),
    _line("14:00:00.000", "NDI 2ME PGM (mv)", 100),
    _line("14:00:00.000", "NDI cam3", 2000),
    _line("14:00:00.000", "NDI cam4", 3000),
    _line("14:00:30.000", "NDI cam4", 4800),          # cam4: 30 s window
    _line("14:01:00.000", "NDI cam1", 4600),          # intermediate (order sanity)
    _line("14:02:00.000", "NDI cam1", 8200),
    _line("14:02:00.000", "NDI cam2", 6500),
    _line("14:02:00.000", "NDI 2ME PGM (mv)", 3700),
    _line("14:02:00.000", "NDI cam3", 2000),          # still 2000 -> frozen
])


# --------------------------------------------------------------------------
# audit_samples
# --------------------------------------------------------------------------
def test_audit_samples_parses_per_source_in_order():
    s = _mod.audit_samples(_LOG)
    assert set(s) == {"NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4",
                      "NDI 2ME PGM (mv)"}
    # chronological file order; first + last are what cadence uses.
    assert s["NDI cam1"][0] == ("14:00:00.000", 1000)
    assert s["NDI cam1"][-1] == ("14:02:00.000", 8200)
    assert s["NDI cam2"][0] == ("14:00:00.000", 500)
    assert s["NDI cam2"][-1] == ("14:02:00.000", 6500)


def test_audit_samples_ignores_non_audit_lines():
    log = "14:00:00.000: some other obs log line\n" + _line(
        "14:00:05.000", "NDI cam1", 42)
    s = _mod.audit_samples(log)
    assert list(s) == ["NDI cam1"]
    assert s["NDI cam1"] == [("14:00:05.000", 42)]


# --------------------------------------------------------------------------
# cadence_check -- reuses the real bash kernel
# --------------------------------------------------------------------------
def test_cadence_check_flags_only_the_50fps_camera():
    display, problems = _mod.cadence_check(_LOG)
    # cam2 is the mis-set 50 fps camera -> exactly one WARN, naming cam2 + 50.
    warns = [p for p in problems if p.startswith("warn:cadence")]
    assert len(warns) == 1
    assert "cam2" in warns[0]
    assert "50" in warns[0]
    # cam1 (a true 60) is NOT flagged.
    assert not any("cam1" in p for p in problems)


def test_cadence_check_display_shows_measured_fps():
    display, _ = _mod.cadence_check(_LOG)
    assert display.get("cam1") == "60"
    assert display.get("cam2") == "50"


def test_cadence_check_excludes_non_camera_sources():
    display, problems = _mod.cadence_check(_LOG)
    # 2ME PGM is a legit 30 fps program feed, not a camera -> never @60-checked.
    assert not any("2ME" in k for k in display)
    assert not any("2ME" in p for p in problems)


def test_cadence_check_frozen_source_is_unknown_not_wrong():
    display, problems = _mod.cadence_check(_LOG)
    # cam3 never advances -> UNKNOWN -> no display row, no problem (freeze != cadence).
    assert "cam3" not in display
    assert not any("cam3" in p for p in problems)


def test_cadence_check_short_window_is_unknown():
    display, problems = _mod.cadence_check(_LOG)
    # cam4 measures 60 fps but only over 30 s (< 60 s min window) -> UNKNOWN.
    assert "cam4" not in display
    assert not any("cam4" in p for p in problems)


def test_cadence_check_empty_log_is_noop():
    display, problems = _mod.cadence_check("")
    assert display == {}
    assert problems == []


# --------------------------------------------------------------------------
# box_verdict -- the shared three-tier split
# --------------------------------------------------------------------------
def test_box_verdict_pass_warn_fail():
    assert _mod.box_verdict([]) == "PASS"
    assert _mod.box_verdict(["warn:cadence cam2=50fps(!=60)"]) == "WARN"
    assert _mod.box_verdict(["render-fps-low"]) == "FAIL"
    # a hard problem alongside a soft one is still FAIL.
    assert _mod.box_verdict(["warn:cadence cam2=50fps(!=60)",
                             "render-fps-low"]) == "FAIL"


# --------------------------------------------------------------------------
# Integration composition: the exact fold check_windows_box(strih) performs --
# cadence_check problems + box_verdict -- without needing ssh/WS (the glue
# itself is `problems += cad_problems; verdict = box_verdict(problems)`).
# --------------------------------------------------------------------------
def test_strih_fold_a_50fps_camera_drives_the_node_to_warn():
    _, cad_problems = _mod.cadence_check(_LOG)          # cam2 is the mis-set 50 fps
    problems = []                                       # a strih box otherwise clean
    problems += cad_problems
    assert _mod.box_verdict(problems) == "WARN"


def test_strih_fold_cadence_warn_plus_hard_problem_is_fail():
    _, cad_problems = _mod.cadence_check(_LOG)
    problems = ["render-fps-low"]                       # a real hard problem present too
    problems += cad_problems
    assert _mod.box_verdict(problems) == "FAIL"


def test_strih_fold_all_60fps_stays_pass():
    log = "\n".join([
        _line("14:00:00.000", "NDI cam1", 1000),
        _line("14:02:00.000", "NDI cam1", 8200),        # 60.0 fps -> OK
        _line("14:00:00.000", "NDI cam2", 5000),
        _line("14:02:00.000", "NDI cam2", 12200),       # 60.0 fps -> OK
    ])
    _, cad_problems = _mod.cadence_check(log)
    assert cad_problems == []
    assert _mod.box_verdict([] + cad_problems) == "PASS"
