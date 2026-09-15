"""#1319 — absolute A/V offset BAND alarm + dock-live freshness facet.

Owner report 15.9.2026: NO notification reached the owner while the stream-box av-sync dock read a
wandering A/V offset (+13 -> +47 ms over 80 min at a constant pin). The #1267 STEP detector is blind
to a slow absolute drift, and it FALSE-read STALE during the dock's suggestion-dead-band quiet
windows (SUGGESTED lines only emit while |offset| is outside the dead band). This lane adds:
  * bundle_state_gather.av_offset_dock_live_age_from_log -- in-log age of the freshest
    `av-sync-dock: diag ... locked=yes` line, so the decision tells "dock LIVE, offset in the dead
    band" (IN_BAND_QUIET, healthy) from "dock silent" (STALE).
  * bundle_state_gather.av_offset_series_from_log is left BYTE-IDENTICAL. ROZHODNUTÉ (issue 1319):
    the raw `UPDATED/LOCKED offset=` lines are DELIBERATELY NOT folded into the series — folding
    broke the #1267 exclusion test AND pushed a quiet dead-band window OUT_OF_BAND (its transient
    lock-acquisition values). The dead-band case is handled by the dock-live-age facet
    (IN_BAND_QUIET), never by folding — see test_raw_updated_locked_lines_are_deliberately_not...
  * av_step_decision.classify_av_band / analyze_band -- OUT_OF_BAND / IN_BAND / IN_BAND_QUIET /
    STALE / REPIN / SKIP / UNKNOWN against a FIXED E2E-aligned reference.
"""
import importlib.util
import json
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, _SCRIPTS / f"{name}.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


bsg = _load("bundle_state_gather")
asd = _load("av_step_decision")

_PFX = "[obs-audio-video-sync-dock] av-sync-dock:"


def _ts(h, m, s, ms=0):
    return f"{h:02d}:{m:02d}:{s:02d}.{ms:03d}"


def _diag(h, m, s):
    return f"{_ts(h, m, s)}: {_PFX} diag lock_ms=33 err_ms=2 locked=yes state=LIVE"


def _sugg(h, m, s, pin, off):
    tgt = pin - int(round(off))
    return (f"{_ts(h, m, s)}: {_PFX} LOCK-CORRECT SUGGESTED genlock_latency_ms_src "
            f"{pin} -> {tgt}ms (measured offset={off}ms) [monitor-only]")


def _upd(h, m, s, off):
    return f"{_ts(h, m, s)}: {_PFX} UPDATED offset={off}ms source=cluster matched=36 mad=38.0ms"


def _locked(h, m, s, off):
    return f"{_ts(h, m, s)}: {_PFX} LOCKED offset={off}ms"


# ---------------------------------------------------------------- av_offset_dock_live_age_from_log
def test_dock_live_age_fresh_and_absent():
    # freshest diag line 20s behind the log head -> "20"
    lines = [_diag(19, 59, 40), _sugg(19, 59, 50, 932, 47.0), _diag(20, 0, 0), "20:00:00.500: other"]
    # newest ts is 20:00:00.500; freshest diag is 20:00:00.000 -> ~0-1s (round)
    age = bsg.av_offset_dock_live_age_from_log("\n".join(lines) + "\n")
    assert age == "0" or age == "1", age
    # a genuinely stale diag (freshest diag long before the head)
    lines2 = [_diag(19, 40, 0)] + [f"{_ts(19, 55, s)}: {_PFX} something else" for s in range(0, 20)]
    assert int(bsg.av_offset_dock_live_age_from_log("\n".join(lines2) + "\n")) > 300
    # no diag line at all -> ""
    assert bsg.av_offset_dock_live_age_from_log(_sugg(19, 59, 0, 932, 47.0) + "\n") == ""
    assert bsg.av_offset_dock_live_age_from_log("") == ""


# ---------------------------------------------------------------- series byte-identical + folding
def test_suggested_only_series_is_unchanged():
    # A SUGGESTED-only log must keep the EXACT #1267 7-tuple (no UPDATED/LOCKED lines present).
    lines = [_sugg(19, 59, s, 932, 21.0) for s in range(0, 40, 2)]
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert pin == "932" and ps == "1"
    assert recent == "21.0" and int(nr) == 20 and int(nb) == 0


def test_raw_updated_locked_lines_are_deliberately_not_in_the_series():
    # DECISION (issue 1319): the raw per-tick UPDATED/LOCKED offset lines are NOT folded into the
    # series. Folding was contradictory (the #1267 test_locked_updated_line_is_not_the_step_signal
    # asserts they are excluded, and the byte-identical mandate holds) AND wrong (their transient
    # lock-acquisition values would push a quiet dead-band window OUT_OF_BAND). The dead-band case is
    # handled instead by the dock-live-age facet -> IN_BAND_QUIET. Here: SUGGESTED sets the series,
    # a following UPDATED/LOCKED burst adds NOTHING to n_recent.
    lines = [_sugg(19, 59, 0, 932, 10.0), _sugg(19, 59, 2, 932, 10.0)]
    lines += [_upd(19, 59, 10 + i, 12.0) for i in range(6)] + [_locked(19, 59, 20, 12.0)]
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log("\n".join(lines) + "\n")
    assert int(nr) == 2, nr           # only the 2 SUGGESTED samples, the raw burst is excluded
    assert pin == "932" and ps == "1"


# ---------------------------------------------------------------- classify_av_band
def _b(recent, ps, nr, dla, reachable=1, ref=0.0, band=30, mn=6, stale=300):
    return asd.classify_av_band(recent, ps, nr, dla, reachable, band_reference_ms=ref,
                                band_ms=band, min_samples=mn, stale_threshold_s=stale)


def test_band_out_of_band_and_in_band():
    assert _b(47.0, "1", 12, 0, ref=-17.0, band=30) == "OUT_OF_BAND"   # |47-(-17)|=64 > 30
    assert _b(-40.0, "1", 12, 0, ref=0.0, band=30) == "OUT_OF_BAND"    # sign-agnostic
    assert _b(21.0, "1", 12, 0, ref=0.0, band=30) == "IN_BAND"         # |21| <= 30
    assert _b(30.0, "1", 12, 0, ref=0.0, band=30) == "IN_BAND"         # strict >
    assert _b(30.1, "1", 12, 0, ref=0.0, band=30) == "OUT_OF_BAND"


def test_band_repin_never_pages_on_a_moved_pin():
    assert _b(90.0, "0", 12, 0, ref=0.0, band=30) == "REPIN"
    assert _b(90.0, None, 12, 0, ref=0.0, band=30) == "REPIN"   # a missing flag is not "1"


def test_band_quiet_vs_stale_vs_unknown():
    # thin/no recent samples but the dock diag line is FRESH -> IN_BAND_QUIET (healthy, log-only)
    assert _b(None, "1", 0, 4, ref=0.0) == "IN_BAND_QUIET"
    assert _b(None, "1", 0, 250, ref=0.0, stale=300) == "IN_BAND_QUIET"
    # the dock's live line itself is stale -> STALE (never a page)
    assert _b(None, "1", 0, 301, ref=0.0, stale=300) == "STALE"
    # no recent samples AND no diag line at all -> UNKNOWN
    assert _b(None, "1", 0, None, ref=0.0) == "UNKNOWN"
    # fresh band samples are judged even if the dock-live facet is absent
    assert _b(47.0, "1", 12, None, ref=-17.0, band=30) == "OUT_OF_BAND"


def test_band_skip_before_anything():
    assert _b(47.0, "1", 12, 0, reachable=0, ref=-17.0) == "SKIP"


# ---------------------------------------------------------------- analyze_band dict + CLI
_OOB_JSON = json.dumps({
    "av_offset_recent_med_ms": "47.0", "av_offset_pin": "932", "av_offset_pin_stable": "1",
    "av_offset_n_recent": "12", "av_offset_dock_live_age_s": "3",
})


def test_analyze_band_dict():
    d = asd.analyze_band(_OOB_JSON, 1, band_reference_ms=-17.0, band_ms=30)
    assert d["verdict"] == "OUT_OF_BAND"
    assert d["recent_med_ms"] == 47.0 and d["band_reference_ms"] == -17.0
    assert d["band_delta_ms"] == 64.0 and d["pin"] == 932
    assert d["dock_live_age_s"] == 3 and d["pin_stable"] == "1"


def test_analyze_band_skip_does_not_parse_body():
    d = asd.analyze_band("garbage", 0, band_reference_ms=-17.0)
    assert d["verdict"] == "SKIP" and d["recent_med_ms"] is None and d["band_delta_ms"] is None


def _run_cli(args, stdin_text=""):
    p = subprocess.run([sys.executable, str(_SCRIPTS / "av_step_decision.py")] + args,
                       input=stdin_text.encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert p.returncode == 0, p.stderr.decode()
    return dict(l.split("=", 1) for l in p.stdout.decode().splitlines() if "=" in l)


def test_cli_analyze_band():
    out = _run_cli(["analyze-band", "--box-reachable", "1", "--band-reference-ms", "-17",
                    "--band-ms", "30"], _OOB_JSON)
    assert out["verdict"] == "OUT_OF_BAND" and out["band_delta_ms"] == "64.0"
    assert out["recent_med_ms"] == "47.0" and out["pin"] == "932"


# ---------------------------------------------------------------- the realistic 19:40-19:59 shape
def _shape_lines(head_h, head_m, head_s):
    """The owner's 15.9. 19:33-20:00 log shape, truncated at (head_h:head_m:head_s):
    diag locked=yes every 10s throughout; SUGGESTED 2/min 19:33-19:40 (offset climbing 13->30),
    then 19:42/19:43 one each, NOTHING 19:44-19:55, then 19:56-19:59 resuming near +47; an
    UPDATED/LOCKED burst at 19:40:54-19:41:09."""
    head = head_h * 3600 + head_m * 60 + head_s
    rows = []  # (tsec, line) — a real OBS log is in time order, so we SORT before joining.

    def emit(h, m, s, line):
        tsec = h * 3600 + m * 60 + s
        if tsec <= head:
            rows.append((tsec, line))

    for tsec in range(19 * 3600 + 33 * 60, 20 * 3600 + 1, 10):
        h, rem = divmod(tsec, 3600)
        m, s = divmod(rem, 60)
        emit(h, m, s, _diag(h, m, s))
    off = 13.0
    for mm in range(33, 40):
        for ss in (5, 35):
            emit(19, mm, ss, _sugg(19, mm, ss, 932, round(off, 1)))
            off += 1.2
    for ss, o in ((54, 26.6), (58, 15.8)):
        emit(19, 40, ss, _upd(19, 40, ss, o))
    emit(19, 41, 0, _locked(19, 41, 0, 11.8))
    for ss, o in ((5, 5.1), (9, -2.1)):
        emit(19, 41, ss, _upd(19, 41, ss, o))
    emit(19, 42, 20, _sugg(19, 42, 20, 932, 40.0))
    emit(19, 43, 20, _sugg(19, 43, 20, 932, 42.0))
    for mm, cnt in ((56, 1), (57, 2), (58, 1), (59, 2)):
        for i in range(cnt):
            emit(19, mm, 10 + i * 20, _sugg(19, mm, 10 + i * 20, 932, 47.0))
    rows.sort(key=lambda r: r[0])
    return "\n".join(line for _t, line in rows) + "\n"


def test_realistic_out_of_band_at_head_2000():
    txt = _shape_lines(20, 0, 0)
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log(txt)
    dla = bsg.av_offset_dock_live_age_from_log(txt)
    v = asd.classify_av_band(float(recent), ps, int(nr), int(dla), 1, band_reference_ms=-17.0,
                             band_ms=30)
    assert v == "OUT_OF_BAND", (recent, nr, dla, v)   # recent ~47 vs ref -17 -> out of band
    assert int(dla) <= 10                              # dock is LIVE, not stale


def test_realistic_deadband_is_quiet_not_stale_at_head_1950():
    # The 19:44-19:55 dead-band gap: no fresh SUGGESTED in the recent window, but diag is LIVE.
    txt = _shape_lines(19, 50, 0)
    recent, base, pin, ps, age, nr, nb = bsg.av_offset_series_from_log(txt)
    dla = bsg.av_offset_dock_live_age_from_log(txt)
    rec = None if recent == "" else float(recent)
    v = asd.classify_av_band(rec, ps, int(nr), int(dla), 1, band_reference_ms=-17.0, band_ms=30,
                             min_samples=6)
    assert v == "IN_BAND_QUIET", (recent, nr, dla, v)   # NOT STALE (the 19:51 false read)
    assert int(dla) <= 10


# ---------------------------------------------------------------- F2: non-finite reference guard
def test_reference_resolution_rejects_non_finite(tmp_path):
    # #1319 review F2: json.load accepts a bare NaN; a non-finite reference would blind the band arm
    # (abs(recent - nan) > band is always False -> never pages). resolve_band_reference must reject
    # it and fall to the 0 ms fallback; a finite value is returned.
    import os
    wd = _SCRIPTS / "av-step-alert-watchdog.sh"
    f = tmp_path / "resid.json"
    env = dict(os.environ, AV_BAND_REFERENCE_FILE=str(f))
    env.pop("AV_BAND_REFERENCE_MS", None)

    def _resolve():
        p = subprocess.run(["bash", "-c", f'source "{wd}"; resolve_band_reference'],
                           env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return p.stdout.decode().strip()

    f.write_text('{"residual_median_ms": NaN}')
    assert _resolve().startswith("0 fallback"), "NaN must fall to the 0 ms fallback"
    f.write_text('{"residual_median_ms": Infinity}')
    assert _resolve().startswith("0 fallback"), "Infinity must fall to the 0 ms fallback"
    f.write_text('{"residual_median_ms": -16.7}')
    assert _resolve().split()[0] == "-16.7", "a finite reference is returned"
