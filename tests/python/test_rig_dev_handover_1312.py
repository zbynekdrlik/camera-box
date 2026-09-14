"""#1312 -- tests for the PURE decision engine of the "development" handover check
(`scripts/rig_dev_handover_decision.py`).

The bash orchestrator (`scripts/rig-dev-handover-check.sh`) is thin I/O: it runs each EXISTING
read-only probe bounded + drain-safe and writes each probe's merged stdout+stderr to
`<work-dir>/<name>.out` + its exit code to `<name>.rc`. ALL logic -- parsing the watchdog
`verdict=`/`-> REACHABLE` lines, the exit-code probes, the rig-mode bare token, the cam-box
uniform-version reduction, the OK/FORGOT-BY-OWNER/UNKNOWN decision table, the checklist summary
and exit code -- lives in the pure module and is verified here against captured dry-run fixtures
(Tier-0 #557: no cargo, no live box). Fixtures reproduce the REAL log-line shapes each probe emits
(confirmed from the probe sources / decision modules).
"""

import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_dev_handover_decision as d


# --- realistic captured probe output (the exact log-line shapes the probes emit) ----------------
MIC_SILENT = ("2026-09-14 03:00:00 [measurement-audio-alert-watchdog] pass start\n"
              "2026-09-14 03:00:01 [measurement-audio-alert-watchdog] rig mode "
              "(cam2 painter probe @ 10.77.9.62): TEST\n"
              "2026-09-14 03:00:02 [measurement-audio-alert-watchdog] stream "
              "(10.77.9.204:4455): reachable=1 verdict=SILENT peak_db=-71.4 meter_present=1\n")
MIC_PRESENT = ("2026-09-14 03:00:02 [measurement-audio-alert-watchdog] stream "
               "(10.77.9.204:4455): reachable=1 verdict=PRESENT peak_db=-14.2 meter_present=1\n")
MIC_SKIP = ("2026-09-14 03:00:02 [measurement-audio-alert-watchdog] stream "
            "(10.77.9.204:4455): reachable=0 verdict=SKIP peak_db= meter_present=\n")

OPTICAL_HEALTHY = ("2026-09-14 [optical-chain-alert-watchdog] painter_expected=1 painter_alive=1 "
                   "optical=OK rig_busy=0 -> verdict=healthy\n")
OPTICAL_DEAD = ("2026-09-14 [optical-chain-alert-watchdog] painter_expected=1 painter_alive=0 "
                "optical=UNKNOWN rig_busy=0 -> verdict=alert:PAINTER-DEAD\n")
OPTICAL_BLACK = ("2026-09-14 [optical-chain-alert-watchdog] painter_expected=1 painter_alive=1 "
                 "optical=BLACK rig_busy=0 -> verdict=alert:OPTICAL-BLACK\n")
OPTICAL_LOGONLY = ("2026-09-14 [optical-chain-alert-watchdog] painter_expected=1 painter_alive=0 "
                   "optical=OK rig_busy=0 -> verdict=log-only:PAINTER-DEAD-optical-ok\n")
OPTICAL_NODATA = ("2026-09-14 [optical-chain-alert-watchdog] no painter probe from cam2 -- "
                  "nothing to decide this pass\n")

CLOCK_ALL_OK = ("2026-09-14 [dantesync-clock-alert-watchdog] cam1 (10.77.9.61): reachable=1 "
                "verdict=OK reason=\n"
                "2026-09-14 [dantesync-clock-alert-watchdog] strih (10.77.9.202): reachable=1 "
                "verdict=OK reason=\n")
CLOCK_ONE_NOCLOCK = ("2026-09-14 [dantesync-clock-alert-watchdog] cam1 (10.77.9.61): reachable=1 "
                     "verdict=OK reason=\n"
                     "2026-09-14 [dantesync-clock-alert-watchdog] strih (10.77.9.202): reachable=1 "
                     "verdict=NO_CLOCK reason=not_locked\n")
CLOCK_ALL_SKIP = ("2026-09-14 [dantesync-clock-alert-watchdog] cam1 (10.77.9.61): reachable=0 "
                  "verdict=SKIP reason=\n"
                  "2026-09-14 [dantesync-clock-alert-watchdog] strih (10.77.9.202): reachable=0 "
                  "verdict=SKIP reason=\n")
# #1313: the watchdog now ALSO probes dev1 as the `local` node (loopback 127.0.0.1:8898, up by
# definition -- ALWAYS a real OK/NO_CLOCK/NO_DANTESYNC token, never SKIP). So the clock item's live
# output now carries a dev1 line. Two intended readings this pins (consistent with the documented
# single-node-verdict SKIP-defer invariant -- a readable node contributes, remote UNreachability is
# the `net` item's job):
CLOCK_DEV1_OK_REMOTES_SKIP = (
    "2026-09-14 [dantesync-clock-alert-watchdog] dev1 (127.0.0.1): reachable=1 verdict=OK reason=\n"
    "2026-09-14 [dantesync-clock-alert-watchdog] cam1 (10.77.9.61): reachable=0 verdict=SKIP reason=\n"
    "2026-09-14 [dantesync-clock-alert-watchdog] strih (10.77.9.202): reachable=0 verdict=SKIP reason=\n")
CLOCK_DEV1_NODANTESYNC = (
    "2026-09-14 [dantesync-clock-alert-watchdog] dev1 (127.0.0.1): reachable=0 verdict=NO_DANTESYNC "
    "reason=no_dantesync_http\n"
    "2026-09-14 [dantesync-clock-alert-watchdog] cam1 (10.77.9.61): reachable=1 verdict=OK reason=\n")

OBS_ALL_HEALTHY = ("2026-09-14 [obs-liveness-watchdog] strih: verdict=HEALTHY reasons=''\n"
                   "2026-09-14 [obs-liveness-watchdog] stream: verdict=HEALTHY reasons=''\n")
OBS_ONE_WEDGED = ("2026-09-14 [obs-liveness-watchdog] strih: verdict=HEALTHY reasons=''\n"
                  "2026-09-14 [obs-liveness-watchdog] stream: verdict=WEDGED-RENDER-LAG "
                  "reasons='renderSkipped 16.0%'\n")

NET_ALL_REACHABLE = ("2026-09-14 [network-reach-alert-watchdog] strih (10.77.9.202): ping=1 "
                     "ws:4455=1 bundle:8899=1 report_only=0 -> REACHABLE\n"
                     "2026-09-14 [network-reach-alert-watchdog] stream (10.77.9.204): ping=1 "
                     "ws:4455=1 bundle:8899=1 report_only=0 -> REACHABLE\n")
NET_ONE_DOWN = ("2026-09-14 [network-reach-alert-watchdog] strih (10.77.9.202): ping=1 ws:4455=1 "
                "bundle:8899=1 report_only=0 -> REACHABLE\n"
                "2026-09-14 [network-reach-alert-watchdog] stream (10.77.9.204): ping=0 ws:4455=0 "
                "bundle:8899=0 report_only=0 -> UNREACHABLE\n")

AUDIOLAG_OK = ("2026-09-14 [audio-lag-alert-watchdog] strih (10.77.9.202): reachable=1 "
               "verdict=HEALTHY lag_ms=3 src=mbc age_s=1\n"
               "2026-09-14 [audio-lag-alert-watchdog] strih band (10.77.9.202): reachable=1 "
               "verdict=HEALTHY src=mbc base_ms=3 high_ms=4 low_ms=2 duty_pct=0 n=30\n")
AUDIOLAG_LAGGING = ("2026-09-14 [audio-lag-alert-watchdog] stream (10.77.9.204): reachable=1 "
                    "verdict=LAGGING lag_ms=7200 src=mbc age_s=1\n")

GENLOCK_OK = ("2026-09-14 [genlock-lock-alert-watchdog] strih (10.77.9.202): reachable=1 "
              "verdict=HEALTHY state=LOCKED reason=\n"
              "2026-09-14 [genlock-lock-alert-watchdog] stream (10.77.9.204): reachable=1 "
              "verdict=HEALTHY state=LOCKED reason=\n")
GENLOCK_UNLOCKED = ("2026-09-14 [genlock-lock-alert-watchdog] strih (10.77.9.202): reachable=1 "
                    "verdict=UNLOCKED state=UNLOCKED reason=no_inputs_locked\n")
GENLOCK_ABSENT = ("2026-09-14 [genlock-lock-alert-watchdog] strih (10.77.9.202): reachable=1 "
                  "verdict=UNKNOWN state= reason=\n")

# #1312 avlatency: the standalone scripts/measurement-chain-latency.sh probe emits a key=value block
# (carrying the ONE `verdict=` token) + a human log line that uses `result=` (never a 2nd verdict
# token). The item is verdict-kind: good={ALIGNED}, forgot={DRIFTED}, else (UNKNOWN/SKIP/NO-BASELINE)
# -> UNKNOWN.
AVLAT_ALIGNED = ("box_reachable=1\nmarkers=6\nonsets=6\npaired=6\nlatency_ms=118.0\nbaseline_ms=120.0\n"
                 "tolerance_ms=90.0\ndelta_ms=2.0\nreason=aligned\nverdict=ALIGNED\n"
                 "2026-09-14T13:00:00Z [measurement-chain-latency] stream (10.77.9.204): reachable=1 "
                 "result=ALIGNED latency_ms=118.0 baseline_ms=120.0 paired=6 reason=aligned\n")
AVLAT_DRIFTED = ("box_reachable=1\nmarkers=6\nonsets=6\npaired=6\nlatency_ms=-22.0\nbaseline_ms=118.0\n"
                 "tolerance_ms=90.0\ndelta_ms=140.0\nreason=latency-step\nverdict=DRIFTED\n"
                 "2026-09-14T13:00:00Z [measurement-chain-latency] stream (10.77.9.204): reachable=1 "
                 "result=DRIFTED latency_ms=-22.0 baseline_ms=118.0 paired=6 reason=latency-step\n")
AVLAT_UNKNOWN_MONO = ("box_reachable=1\nmarkers=6\nonsets=6\npaired=0\nlatency_ms=\nbaseline_ms=120.0\n"
                      "tolerance_ms=90.0\ndelta_ms=\nreason=monotonic-emit\nverdict=UNKNOWN\n"
                      "2026-09-14T13:00:00Z [measurement-chain-latency] stream (10.77.9.204): reachable=1 "
                      "result=UNKNOWN latency_ms= baseline_ms=120.0 paired=0 reason=monotonic-emit\n")
AVLAT_NO_BASELINE = ("box_reachable=1\nmarkers=6\nonsets=6\npaired=6\nlatency_ms=118.0\nbaseline_ms=\n"
                     "tolerance_ms=90.0\ndelta_ms=\nreason=no-baseline\nverdict=NO-BASELINE\n"
                     "2026-09-14T13:00:00Z [measurement-chain-latency] stream (10.77.9.204): reachable=1 "
                     "result=NO-BASELINE latency_ms=118.0 baseline_ms= paired=6 reason=no-baseline\n")
AVLAT_SKIP = ("box_reachable=0\nmarkers=0\nonsets=0\npaired=0\nlatency_ms=\nbaseline_ms=120.0\n"
              "tolerance_ms=90.0\ndelta_ms=\nreason=stream-obs-unreachable\nverdict=SKIP\n"
              "2026-09-14T13:00:00Z [measurement-chain-latency] stream (10.77.9.204): reachable=0 "
              "result=SKIP latency_ms= baseline_ms=120.0 paired=0 reason=stream-obs-unreachable\n")


# --- low-level parsers ---------------------------------------------------------------------------
def test_verdict_tokens_extracts_all_in_order():
    assert d.verdict_tokens(CLOCK_ONE_NOCLOCK) == ["OK", "NO_CLOCK"]
    assert d.verdict_tokens(MIC_SILENT) == ["SILENT"]
    # a prefixed token is captured whole
    assert d.verdict_tokens(OPTICAL_LOGONLY) == ["log-only:PAINTER-DEAD-optical-ok"]
    assert d.verdict_tokens("") == []
    assert d.verdict_tokens(OPTICAL_NODATA) == []


def test_net_tokens_only_matches_the_arrow_form():
    assert d.net_tokens(NET_ALL_REACHABLE) == ["REACHABLE", "REACHABLE"]
    assert d.net_tokens(NET_ONE_DOWN) == ["REACHABLE", "UNREACHABLE"]
    # the verdict= watchdogs must NOT be picked up by the net parser
    assert d.net_tokens(CLOCK_ALL_OK) == []


def test_bare_token_reads_last_nonempty_line():
    assert d.bare_token("TEST\n") == "TEST"
    assert d.bare_token("  \nEVENT\n\n") == "EVENT"
    assert d.bare_token("") is None
    assert d.bare_token("\n \n") is None


def test_reduce_tokens_fails_loud_toward_forgot():
    assert d.reduce_tokens(["OK", "NO_CLOCK"], {"OK"}, {"NO_CLOCK"}) == d.FORGOT
    assert d.reduce_tokens(["OK", "OK"], {"OK"}, {"NO_CLOCK"}) == d.OK
    # only skip/unknown-class tokens -> UNKNOWN
    assert d.reduce_tokens(["SKIP", "UNKNOWN"], {"OK"}, {"NO_CLOCK"}) == d.UNKNOWN
    assert d.reduce_tokens([], {"OK"}, {"NO_CLOCK"}) == d.UNKNOWN


def test_reduce_tokens_honours_prefixes():
    assert d.reduce_tokens(["alert:PAINTER-DEAD"], {"healthy"}, set(),
                           forgot_prefixes=("alert:",)) == d.FORGOT
    assert d.reduce_tokens(["log-only:PAINTER-DEAD-optical-ok"], {"healthy"}, set(),
                           good_prefixes=("log-only:",), forgot_prefixes=("alert:",)) == d.OK


def test_status_from_exit():
    # burns: sweep-check exit 1 = burns ON = OK; 0 = none = FORGOT; 2 = enum fail = UNKNOWN
    assert d.status_from_exit(1, {1}, {0}) == d.OK
    assert d.status_from_exit(0, {1}, {0}) == d.FORGOT
    assert d.status_from_exit(2, {1}, {0}) == d.UNKNOWN
    # a timeout kill / missing sentinel -> UNKNOWN
    assert d.status_from_exit(124, {0}, {1}) == d.UNKNOWN
    assert d.status_from_exit(d.RC_MISSING, {0}, {1}) == d.UNKNOWN
    assert d.status_from_exit("nonsense", {0}, {1}) == d.UNKNOWN


# --- per-item decisions --------------------------------------------------------------------------
def _item(key):
    return next(i for i in d.ITEMS if i.key == key)


def test_mic_item():
    assert _item("mic").decide({"mic": (MIC_PRESENT, 0)})["status"] == d.OK
    e = _item("mic").decide({"mic": (MIC_SILENT, 0)})
    assert e["status"] == d.FORGOT
    assert "Abletone" in e["message"]
    assert _item("mic").decide({"mic": (MIC_SKIP, 0)})["status"] == d.UNKNOWN


def test_mode_item():
    assert _item("mode").decide({"mode": ("TEST\n", 0)})["status"] == d.OK
    e = _item("mode").decide({"mode": ("EVENT\n", 0)})
    assert e["status"] == d.FORGOT
    assert "rig-mode.sh test" in e["message"]
    assert _item("mode").decide({"mode": ("UNKNOWN\n", 0)})["status"] == d.UNKNOWN
    assert _item("mode").decide({"mode": ("", d.RC_MISSING)})["status"] == d.UNKNOWN


def test_painter_item():
    assert _item("painter").decide({"painter": (OPTICAL_HEALTHY, 0)})["status"] == d.OK
    assert _item("painter").decide({"painter": (OPTICAL_LOGONLY, 0)})["status"] == d.OK
    assert _item("painter").decide({"painter": (OPTICAL_DEAD, 0)})["status"] == d.FORGOT
    assert _item("painter").decide({"painter": (OPTICAL_BLACK, 0)})["status"] == d.FORGOT
    assert _item("painter").decide({"painter": (OPTICAL_NODATA, 0)})["status"] == d.UNKNOWN


def test_burns_item_two_boxes():
    ok = {"burns_strih": ("", 1), "burns_stream": ("", 1)}
    assert _item("burns").decide(ok)["status"] == d.OK
    # one box has no burns -> FORGOT (combine: any FORGOT wins)
    mixed = {"burns_strih": ("", 1), "burns_stream": ("", 0)}
    assert _item("burns").decide(mixed)["status"] == d.FORGOT
    # one box OK, the other UNVERIFIABLE (enum-fail) -> UNKNOWN, NOT a false OK (finding #2)
    half = {"burns_strih": ("", 1), "burns_stream": ("", 2)}
    assert _item("burns").decide(half)["status"] == d.UNKNOWN
    # both unreadable -> UNKNOWN
    unk = {"burns_strih": ("", 2), "burns_stream": ("", d.RC_MISSING)}
    assert _item("burns").decide(unk)["status"] == d.UNKNOWN


def test_combine_statuses_strict_no_masking():
    # FORGOT dominates; a single UNKNOWN makes the item UNKNOWN (never masked by an OK sibling)
    assert d.combine_statuses([d.OK, d.OK]) == d.OK
    assert d.combine_statuses([d.OK, d.UNKNOWN]) == d.UNKNOWN
    assert d.combine_statuses([d.OK, d.FORGOT]) == d.FORGOT
    assert d.combine_statuses([d.FORGOT, d.UNKNOWN]) == d.FORGOT
    assert d.combine_statuses([]) == d.UNKNOWN


def test_mapping_and_pins_items():
    assert _item("mapping").decide({"mapping_strih": ("PASS", 0)})["status"] == d.OK
    assert _item("mapping").decide({"mapping_strih": ("DRIFT", 1)})["status"] == d.FORGOT
    assert _item("mapping").decide({"mapping_strih": ("ERROR", 2)})["status"] == d.UNKNOWN
    pins_ok = {"pins_strih": ("", 0), "pins_stream": ("", 0), "pins_imag": ("", 0)}
    assert _item("pins").decide(pins_ok)["status"] == d.OK
    pins_drift = {"pins_strih": ("", 0), "pins_stream": ("", 1), "pins_imag": ("", 0)}
    assert _item("pins").decide(pins_drift)["status"] == d.FORGOT
    # one box unreadable (connect-fail) among OK boxes -> UNKNOWN, not a false OK (finding #2)
    pins_half = {"pins_strih": ("", 0), "pins_stream": ("", 0), "pins_imag": ("", 2)}
    assert _item("pins").decide(pins_half)["status"] == d.UNKNOWN


def test_clock_obs_net_audiolag_genlock_items():
    assert _item("clock").decide({"clock": (CLOCK_ALL_OK, 0)})["status"] == d.OK
    assert _item("clock").decide({"clock": (CLOCK_ONE_NOCLOCK, 0)})["status"] == d.FORGOT
    assert _item("clock").decide({"clock": (CLOCK_ALL_SKIP, 0)})["status"] == d.UNKNOWN
    # #1313: dev1 is now the `local` node, always readable on the loopback. A pass where every REMOTE
    # node is unreachable (SKIP) but dev1 is locked reads OK (dev1's real reading), NOT UNKNOWN --
    # remote UNreachability is the `net` item's job; the clock LOCK item reflects the readable nodes.
    assert _item("clock").decide({"clock": (CLOCK_DEV1_OK_REMOTES_SKIP, 0)})["status"] == d.OK
    # and a dev1 daemon crash (NO_DANTESYNC) is a real FORGOT even though a remote node is OK.
    assert _item("clock").decide({"clock": (CLOCK_DEV1_NODANTESYNC, 0)})["status"] == d.FORGOT

    assert _item("obs").decide({"obs": (OBS_ALL_HEALTHY, 0)})["status"] == d.OK
    assert _item("obs").decide({"obs": (OBS_ONE_WEDGED, 0)})["status"] == d.FORGOT

    assert _item("net").decide({"net": (NET_ALL_REACHABLE, 0)})["status"] == d.OK
    assert _item("net").decide({"net": (NET_ONE_DOWN, 0)})["status"] == d.FORGOT
    assert _item("net").decide({"net": ("", d.RC_MISSING)})["status"] == d.UNKNOWN

    assert _item("audiolag").decide({"audiolag": (AUDIOLAG_OK, 0)})["status"] == d.OK
    assert _item("audiolag").decide({"audiolag": (AUDIOLAG_LAGGING, 0)})["status"] == d.FORGOT

    assert _item("genlock").decide({"genlock": (GENLOCK_OK, 0)})["status"] == d.OK
    assert _item("genlock").decide({"genlock": (GENLOCK_UNLOCKED, 0)})["status"] == d.FORGOT
    # absent facet (stock OBS) -> UNKNOWN, NEVER forgot (ticket: UNKNOWN-but-not-forgot)
    assert _item("genlock").decide({"genlock": (GENLOCK_ABSENT, 0)})["status"] == d.UNKNOWN


def test_avlatency_item():
    # #1312 14th item: ALIGNED -> OK, DRIFTED -> FORGOT, everything else -> UNKNOWN (never a false
    # forgot). The forgot message names the mbc/Ableton chain + the baseline re-seed.
    assert _item("avlatency").decide({"avlatency": (AVLAT_ALIGNED, 0)})["status"] == d.OK
    e = _item("avlatency").decide({"avlatency": (AVLAT_DRIFTED, 0)})
    assert e["status"] == d.FORGOT
    assert "baseline" in e["message"]
    # monotonic emit_ts (today's permanent painter) -> UNKNOWN, NEVER forgot
    assert _item("avlatency").decide({"avlatency": (AVLAT_UNKNOWN_MONO, 0)})["status"] == d.UNKNOWN
    # not seeded yet -> UNKNOWN
    assert _item("avlatency").decide({"avlatency": (AVLAT_NO_BASELINE, 0)})["status"] == d.UNKNOWN
    # stream OBS unreachable -> UNKNOWN (SKIP token is neither good nor forgot)
    assert _item("avlatency").decide({"avlatency": (AVLAT_SKIP, 0)})["status"] == d.UNKNOWN
    # a MISSING probe on an older base -> UNKNOWN, forward-compatible
    assert _item("avlatency").decide({"avlatency": ("MISSING: x\n", d.RC_MISSING)})["status"] == d.UNKNOWN


def test_version_items():
    # both version items reuse an existing gate (dantesync-version-gate.sh /
    # camera-box-version-gate.sh) whose exit convention is 0=OK, 20=DRIFT, 11=UNKNOWN
    assert _item("dantesync").decide({"dantesync": ("", 0)})["status"] == d.OK
    assert _item("dantesync").decide({"dantesync": ("", 20)})["status"] == d.FORGOT
    assert _item("dantesync").decide({"dantesync": ("", 11)})["status"] == d.UNKNOWN
    # a usage/env error (exit 1) or any other code -> UNKNOWN, never a false OK (finding #5)
    assert _item("dantesync").decide({"dantesync": ("", 1)})["status"] == d.UNKNOWN
    assert _item("cambox").decide({"cambox": ("", 0)})["status"] == d.OK
    assert _item("cambox").decide({"cambox": ("", 20)})["status"] == d.FORGOT
    assert _item("cambox").decide({"cambox": ("", 11)})["status"] == d.UNKNOWN
    assert _item("cambox").decide({"cambox": ("", 1)})["status"] == d.UNKNOWN


# --- checklist assembly + exit codes -------------------------------------------------------------
def test_build_checklist_all_ok():
    entries = [{"key": "mic", "label": "mic", "status": d.OK, "message": "ok"}]
    lines, summary, code = d.build_checklist(entries)
    assert code == 0
    assert lines[0].startswith("✅")
    assert "všetko je v development stave" in summary


def test_build_checklist_forgot_wins_and_lists_labels():
    entries = [
        {"key": "mic", "label": "merací mikrofón (mbc)", "status": d.FORGOT, "message": "m"},
        {"key": "mode", "label": "rig režim (TEST/EVENT)", "status": d.FORGOT, "message": "r"},
        {"key": "genlock", "label": "genlock LOCK", "status": d.UNKNOWN, "message": "g"},
    ]
    lines, summary, code = d.build_checklist(entries)
    assert code == 1
    assert summary.startswith("zabudol si: merací mikrofón (mbc), rig režim (TEST/EVENT)")
    assert "neoverené: genlock LOCK" in summary
    assert lines[0].startswith("❌")
    assert lines[2].startswith("❔")


def test_build_checklist_unknown_only_is_exit_2():
    entries = [{"key": "obs", "label": "OBS", "status": d.UNKNOWN, "message": "x"}]
    _, summary, code = d.build_checklist(entries)
    assert code == 2
    assert summary.startswith("nič si nezabudol, ale neoverené: OBS")


def test_evaluate_over_a_work_dir(tmp_path):
    # a full all-OK capture set -> exit 0
    caps = {
        "mic": (MIC_PRESENT, 0),
        "mode": ("TEST\n", 0),
        "painter": (OPTICAL_HEALTHY, 0),
        "burns_strih": ("", 1), "burns_stream": ("", 1),
        "mapping_strih": ("PASS", 0),
        "pins_strih": ("", 0), "pins_stream": ("", 0), "pins_imag": ("", 0),
        "clock": (CLOCK_ALL_OK, 0),
        "obs": (OBS_ALL_HEALTHY, 0),
        "net": (NET_ALL_REACHABLE, 0),
        "audiolag": (AUDIOLAG_OK, 0),
        "genlock": (GENLOCK_OK, 0),
        "dantesync": ("", 0),
        "cambox": ("", 0),
        "avlatency": (AVLAT_ALIGNED, 0),
    }
    for name, (text, rc) in caps.items():
        (tmp_path / (name + ".out")).write_text(text, encoding="utf-8")
        (tmp_path / (name + ".rc")).write_text(str(rc), encoding="utf-8")
    entries, lines, summary, code = d.evaluate(str(tmp_path))
    assert code == 0, summary
    assert len(entries) == len(d.ITEMS)
    assert all(e["status"] == d.OK for e in entries)

    # now break the mic (owner left it muted) and put the rig in EVENT -> exit 1, both listed
    (tmp_path / "mic.out").write_text(MIC_SILENT, encoding="utf-8")
    (tmp_path / "mode.out").write_text("EVENT\n", encoding="utf-8")
    _, _, summary2, code2 = d.evaluate(str(tmp_path))
    assert code2 == 1
    assert "merací mikrofón (mbc)" in summary2
    assert "rig režim (TEST/EVENT)" in summary2


def test_evaluate_missing_captures_are_unknown_not_forgot(tmp_path):
    # an empty work-dir: every capture missing -> every item UNKNOWN -> exit 2, nothing FORGOT
    entries, _, summary, code = d.evaluate(str(tmp_path))
    assert code == 2
    assert all(e["status"] == d.UNKNOWN for e in entries)
    assert summary.startswith("nič si nezabudol, ale neoverené:")
