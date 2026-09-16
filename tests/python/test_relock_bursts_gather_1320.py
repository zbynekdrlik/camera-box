"""#1320 — unit tests for the `relock_bursts` bundle-state facet in scripts/bundle_state_gather.py.

`relock_bursts_from_log(text)` PORTS issue 1318's `summarize_relock_bursts` (src/jitter_audit.rs) to
Python so the dev1 render-freeze watchdog can page on a receiver FIFO overshoot storm off :8899. A
`genlock-relock '<src>':` line == one relock event; a BURST is a gap-separated cluster whose densest
1 s sub-window holds >= N (=8) relocks. The facet exposes `(max_bursts_str, age_s_str)` — the MAX
bursts across sources + the in-log age (whole seconds) of the newest relock event — `("", "")` when
there is NO relock line at all (steady state; absent -> UNKNOWN downstream, never a fabricated 0).

The PARITY block below feeds the SAME synthetic event sequences the Rust
`src/jitter_audit.rs::tests` use and asserts byte-for-byte identical bursts/max_per_second so the
Python mirror can never silently drift from the authoritative Rust summarizer.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402


def _ms_to_hms(ms):
    """Milliseconds-of-day -> an OBS-log `HH:MM:SS.mmm` prefix."""
    s, mm = divmod(ms, 1000)
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return f"{h:02d}:{m:02d}:{s:02d}.{mm:03d}"


def _relock_line(at_ms, src="NDI 2ME PGM"):
    return (f"{_ms_to_hms(at_ms)}: genlock-relock '{src}': depth=41 steady_depth_frames=28 "
            "due=1 erased=0 head_skew_ms=938 latency_ms=925")


# ── the parser (mirror of parse_relock_line) ────────────────────────────────────────────────────
def test_parses_a_real_relock_line():
    # The exact issue-1318 storm line: 18:27:28.205 = ((18*60+27)*60+28)*1000 + 205 = 66_448_205.
    line = ("18:27:28.205: genlock-relock 'NDI 2ME PGM': depth=41 steady_depth_frames=28 due=1 "
            "erased=0 head_skew_ms=938 tick_phase_ns=19461 latency_ms=925")
    assert bsg._parse_relock_event(line) == ("NDI 2ME PGM", 66_448_205)


def test_parser_rejects_non_relock_and_timestampless_lines():
    assert bsg._parse_relock_event("14:00:00.001: genlock-fifo audit 'NDI cam1': received=1000") is None
    assert bsg._parse_relock_event("genlock-relock 'NDI 2ME PGM': depth=41 erased=0") is None  # no ts
    assert bsg._parse_relock_event("18:00:00.000: [obs] unrelated") is None


def test_parser_reads_the_timestamp_token_before_the_marker_wrapper_robust():
    # Byte-faithful to the Rust `parse_hhmmss_ms(line[..mark].split_whitespace().last())`: a
    # journald/SSH-wrapper prefix before the OBS timestamp still clusters (the LAST token before the
    # marker carries the clock time), whereas a line-start regex would drop it. (review finding F1)
    line = "<7>host journald: 18:27:28.205: genlock-relock 'NDI 2ME PGM': depth=41 erased=0"
    assert bsg._parse_relock_event(line) == ("NDI 2ME PGM", 66_448_205)
    # and the bare-seconds form (no fractional) the Rust also accepts:
    assert bsg._parse_relock_event("00:00:01: genlock-relock 'X': z") == ("X", 1000)


# ── the summarizer (byte-for-byte parity with src/jitter_audit.rs::tests) ────────────────────────
def _ev(source, at_ms):
    return (source, at_ms)


def test_clusters_a_storm_into_one_burst_parity():
    evs = [_ev("NDI 2ME PGM", 1_000_000 + i * 33) for i in range(30)]
    out = bsg._summarize_relock_bursts(evs, 8, 1000)
    assert len(out) == 1
    s = out[0]
    assert s["source"] == "NDI 2ME PGM"
    assert s["total_relocks"] == 30
    assert s["bursts"] == 1
    assert s["max_per_second"] == 30
    assert s["first_at_ms"] == 1_000_000
    assert s["last_at_ms"] == 1_000_000 + 29 * 33


def test_ignores_isolated_single_relocks_parity():
    evs = [_ev("NDI cam1", 0), _ev("NDI cam1", 2000), _ev("NDI cam1", 4000), _ev("NDI cam1", 6000)]
    out = bsg._summarize_relock_bursts(evs, 8, 1000)
    assert len(out) == 1
    assert out[0]["total_relocks"] == 4
    assert out[0]["bursts"] == 0
    assert out[0]["max_per_second"] == 1


def test_separates_two_storms_by_a_gap_parity():
    evs = [_ev("NDI 2ME PGM", 100 + i * 33) for i in range(10)]
    evs += [_ev("NDI 2ME PGM", 100 + 5000 + i * 33) for i in range(10)]
    out = bsg._summarize_relock_bursts(evs, 8, 1000)
    assert len(out) == 1
    assert out[0]["bursts"] == 2
    assert out[0]["max_per_second"] == 10
    assert out[0]["total_relocks"] == 20


def test_groups_bursts_by_source_in_first_seen_order_parity():
    evs = [_ev("NDI 2ME PGM", 0)] + [_ev("NDI 2ME PGM", i * 33) for i in range(1, 12)]
    evs += [_ev("NDI cam1", 500)]
    out = bsg._summarize_relock_bursts(evs, 8, 1000)
    assert len(out) == 2
    assert out[0]["source"] == "NDI 2ME PGM" and out[0]["bursts"] == 1
    assert out[1]["source"] == "NDI cam1" and out[1]["bursts"] == 0


def test_backward_time_step_starts_a_new_cluster_parity():
    evs = [_ev("NDI 2ME PGM", 86_399_900), _ev("NDI 2ME PGM", 86_399_933),
           _ev("NDI 2ME PGM", 33), _ev("NDI 2ME PGM", 66)]
    out = bsg._summarize_relock_bursts(evs, 2, 1000)
    assert len(out) == 1
    assert out[0]["bursts"] == 2  # two 2-event clusters, not one 86 400 s gap
    assert out[0]["max_per_second"] == 2


# ── the facet (max bursts + recency) ────────────────────────────────────────────────────────────
def test_facet_reports_a_storm_with_recency():
    base = 66_448_205  # 18:27:28.205
    lines = [_relock_line(base + i * 33) for i in range(12)]   # 12 in ~0.36 s -> one burst
    lines.append("18:27:55.000: program-render-audit: render_fps=30.0 lagged=0 total=151")  # head, later
    lagged, age = bsg.relock_bursts_from_log("\n".join(lines) + "\n")
    assert lagged == "1", lagged           # max bursts = 1
    # head 18:27:55.000 (66475.0 s) - last relock 18:27:28.568 (66448.568 s) = 26.4 s -> round 26
    assert age == "26", age


def test_facet_absent_when_no_relock_line():
    # Steady state: NO genlock-relock line at all -> absent -> UNKNOWN downstream, never "0".
    assert bsg.relock_bursts_from_log("18:00:00.000: [obs] some line\n") == ("", "")
    assert bsg.relock_bursts_from_log("") == ("", "")


def test_facet_reports_zero_bursts_when_relocks_present_but_sparse():
    # A handful of isolated relocks (below the 8-in-1s bar): relock telemetry live, no STORM -> "0"
    # (a truthy string KEPT), distinct from "" (absent).
    lines = [_relock_line(1000 + i * 2000) for i in range(3)]  # 3 relocks 2 s apart
    lagged, _age = bsg.relock_bursts_from_log("\n".join(lines) + "\n")
    assert lagged == "0", lagged


def test_facet_reads_only_the_tail_after_the_separator():
    base = 66_448_205
    head_storm = "\n".join(_relock_line(base + i * 33) for i in range(12))
    tail = "18:30:00.000: [obs] quiet tail line\n"
    log = head_storm + "\n" + bsg.LOG_BOUNDED_READ_SEPARATOR + tail
    # The storm survives only in the HEAD (before the #1222 separator) -> not reported.
    assert bsg.relock_bursts_from_log(log) == ("", "")


def test_build_bundle_state_omits_empty_but_keeps_zero_relock():
    keep = bsg.build_bundle_state(relock_bursts="1", relock_bursts_age_s="8")
    assert keep["relock_bursts"] == "1"
    assert keep["relock_bursts_age_s"] == "8"

    zero = bsg.build_bundle_state(relock_bursts="0", relock_bursts_age_s="12")
    assert zero["relock_bursts"] == "0"  # "0" is truthy -> KEPT (relocks live, no storm)

    absent = bsg.build_bundle_state()
    assert "relock_bursts" not in absent
    assert "relock_bursts_age_s" not in absent
