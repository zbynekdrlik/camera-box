#!/usr/bin/env python3
"""#1299 -- PURE decision core for the dev1 genlock-lock alert watchdog.

WHY: the in-OBS genlock LOCK indicator (#1298) is visible ONLY to an operator standing at each
box's statusbar. The fleet (dev1) had no way to SEE whether strih/stream/imag/resolume are
LIVE-LOCKED to the fleet clock, and no page fired when a box silently left LOCKED -- the exact
silent-degradation class the dev1 alert-watchdog family (#732/#1001/#1226) exists to close.

The producer (the #1298 statusbar widget) already scalarises the THREE genlock producers
(per-source FIFO counters, the NDI output's wall-stamping flag, the dantesync :8898 clock facet)
into ONE verdict at the one place they meet, and emits it as a versioned `genlock-lock-json:` line;
bundle_state_gather exposes that as the nested `genlock_lock` facet on `:8899/bundle-state.json`.
This module is the pure kernel of the dev1 watchdog that reads that facet from each box and decides
when to page. No I/O, no ssh, no OBS, no MCP -- exhaustively unit-testable (pytest), the
strih-nic-selfheal #1199 / ndi-halving #1203 python-mirror precedent, so the decision RED->GREENs
LOCALLY under Tier-0 (#557 kills cargo). The orchestrator scripts/genlock-lock-alert-watchdog.sh
curls the JSON, calls `analyze` here, and drives obs-watchdog-decision.sh's confirm/throttle +
airuleset notify (--dedup-key genlock-lock-$box, #1206).

`decide()` is a byte-faithful Python MIRROR of src/genlock_lock_state.rs's `decide` (the Rust
authority, itself C-vs-Rust parity-gated against GenlockLockState.hpp). The watchdog trusts the
`state` string the widget already decided and carries in the facet; `decide()` exists so the
test suite can feed the SAME counters the Rust/C parity gate uses and assert the three-state
precedence is understood identically here -- the facet can never disagree with the statusbar.

Verdicts (classify):
  SKIP     -- box could not be fetched (:8899 down / box down). That page is #732 (bundle-state) /
              #1001 (network-reach) territory, never this watchdog's -- so paging requires a
              successfully fetched facet, and a dev1-side outage can only produce SKIP.
  UNKNOWN  -- box fetched OK but the genlock_lock facet is absent (a stock OBS, or no
              genlock-lock-json: line in the tail yet) -- NEVER a false UNLOCKED.
  HEALTHY  -- state == LOCKED.
  DEGRADED -- state == DEGRADED. The watchdog pages after a 2-pass confirm.
  UNLOCKED -- state == UNLOCKED. The watchdog pages after a 2-pass confirm.
"""
import argparse
import json
import sys

# State strings -- match src/genlock_lock_state.rs LockState + the C genlock_state_name().
ST_LOCKED = "LOCKED"
ST_DEGRADED = "DEGRADED"
ST_UNLOCKED = "UNLOCKED"

# Reason tokens -- match the C genlock_reason_key() + src/genlock_lock_state.rs LockReason.
R_NONE = "none"
R_NO_GENLOCK = "no_genlock"
R_CLOCK = "clock"
R_OUTPUT = "output"
R_NO_INPUT_LOCKED = "no_input_locked"
R_INPUT_UNLOCKED = "input_unlocked"
R_RECENT_EVENT = "recent_event"
R_NTP_FAILED = "ntp_failed"
R_QPC_DRIFT = "qpc_drift"
R_AUDIO_PAIRING = "audio_pairing"      # #1303: audio-enabled source unpaired with its video FIFO hold
R_AUDIO_UNEXPECTED = "audio_unexpected"  # #1303: silent-by-contract source found audible (double-audio hazard)
R_MEDIA_CLOCK = "media_clock"          # issue 1372 part D: the audio (media) clock does not follow the wall

# Issue 1372 part D -- the media-clock (audio clock) verdict tokens + bounds (mirror
# src/genlock_lock_state.rs + GenlockLockState.hpp / OBSBasicStatusBar.cpp).
MC_OK = "ok"
MC_DRIFT = "drift"
MC_UNDISCIPLINED = "undisciplined"
GENLOCK_MEDIA_CLOCK_WINDOW_S = 600          # the window the drift growth is measured over
GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US = 2000   # offset growth beyond this (us per window) is DRIFT
GENLOCK_MEDIA_CLOCK_MAX_GAP_MS = 5000       # a pair further apart (a stalled UI) is not a sample
GENLOCK_MEDIA_CLOCK_BAND_PPB = 25_000       # the trimmed mean keeps pairs within this of the median
# The Windows os_gettime_discipline() outcomes that mean "fell back to raw QPC".
MEDIA_DISCIPLINE_RAW_FALLBACK = ("disabled", "read_failed", "api_missing")

# #1299 Part 4 + #1357 scope C -- the wall-vs-QPC drift bounds (mirror src/genlock_lock_state.rs +
# GenlockLockState.hpp / OBSBasicStatusBar.cpp). The verdict is the wall STEP only -- not the unbounded
# cumulative offset (grows ~50 ms/h on a disciplined Windows box, false-paged the fleet) and not a rate
# (0 by construction on Linux's disciplined CLOCK_MONOTONIC, the free crystal on Windows: a rate check
# meant a different thing per box). The windowed rate stays report-only telemetry.
GENLOCK_QPC_STEP_BOUND_MS = 33       # a single-sample wall STEP beyond this (one 30 fps frame) DEGRADES
GENLOCK_QPC_WINDOW_S = 300           # rolling window (s) of the report-only drift-rate telemetry


def qpc_window_rate_ppm(drift_delta_ms, elapsed_ms):
    """#1299 Part 4 -- the windowed drift RATE in ppm from an integer-ms cumulative-drift delta over an
    integer-ms elapsed span. `delta/elapsed` is dimensionless; x 1e6 is ppm. 0.0 for a non-positive
    span (not-ready / degenerate). Byte-faithful mirror of camera_box::genlock_lock_state::
    qpc_window_rate_ppm and the arithmetic inside the C genlock_qpc_drift_beyond_bound."""
    if elapsed_ms <= 0:
        return 0.0
    return float(drift_delta_ms) / float(elapsed_ms) * 1_000_000.0


def qpc_drift_beyond_bound(rate_ready, drift_delta_ms, elapsed_ms, max_step_ms, step_bound_ms):
    """#1299 Part 4 + #1357 scope C -- decide whether the wall clock STEPPED against the monotonic
    timebase, and report the measured windowed rate as telemetry. DEGRADED only when a single-sample
    STEP exceeds `step_bound_ms` (judged as soon as two samples exist, rate_ready or not) -- the one
    clock hazard for genlock, the same on every box. The rate never feeds the verdict. Returns
    (beyond_bound: bool, measured_ppm: float) -- byte-faithful mirror of
    camera_box::genlock_lock_state::qpc_drift_beyond_bound (C-vs-Rust parity-gated), so the test suite
    can pin the same fixture the Rust/C gate uses."""
    measured_ppm = qpc_window_rate_ppm(drift_delta_ms, elapsed_ms) if rate_ready else 0.0
    return (abs(max_step_ms) > step_bound_ms, measured_ppm)


def _trunc_div(a, b):
    """Integer division truncated toward zero (C / Rust `/`), never python's floor."""
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b > 0) else -q


def media_clock_window(samples, window_s, max_gap_ms, band_ppb):
    """Issue 1372 part D -- the wall-vs-media rate across `(t_ms, offset_us)` samples (oldest first):
    each consecutive pair with 0 < dt <= max_gap_ms yields one rate in ppb (change_us * 1e6 / dt,
    truncated toward zero). Their MEDIAN (the mean of the two middle rates for an even count,
    a + (b - a) / 2) is the centre; the result is the TRIMMED MEAN of the rates within band_ppb of it
    (the centre itself when none is), truncated toward zero, scaled to window_s: mean_ppb * window_s /
    1000 us. A wall step lands far outside the band; a drift in only part of the pairs is averaged in.
    Returns (drift_us, counted_ms); no pair or a non-positive window_s gives drift 0. Mirror of
    camera_box::genlock_lock_state::media_clock_window, cross-checked vector by vector against it by
    tests/genlock_lock_state_parity.rs (python ints never overflow, so the Rust/C saturation only
    matters at the i64 extremes no real clock reaches; that gate keeps to the real range)."""
    rates = []
    counted_ms = 0
    for (ta, oa), (tb, ob) in zip(samples, samples[1:]):
        dt = tb - ta
        if dt <= 0 or dt > max_gap_ms:
            continue
        rates.append(_trunc_div((ob - oa) * 1_000_000, dt))
        counted_ms += dt
    if not rates or window_s <= 0:
        return (0, counted_ms)
    rates.sort()
    m = len(rates)
    if m % 2 == 1:
        centre = rates[m // 2]
    else:
        a, b = rates[m // 2 - 1], rates[m // 2]
        centre = a + _trunc_div(b - a, 2)
    kept = [r for r in rates if abs(r - centre) <= band_ppb]
    mean = _trunc_div(sum(kept), len(kept)) if kept else centre
    return (_trunc_div(mean * window_s, 1000), counted_ms)


def media_clock_window_ready(counted_ms, window_s):
    """Issue 1372 part D -- True once the counted pairs cover >= 90 % of the window; a non-positive
    window is never ready. Mirror of camera_box::genlock_lock_state::media_clock_window_ready."""
    return window_s > 0 and counted_ms >= (window_s * 1000) // 10 * 9


def media_clock_verdict(window_ready, drift_us, drift_bound_us, discipline, clock_present):
    """Issue 1372 part D -- UNDISCIPLINED when the Windows clock fell back to raw QPC (`discipline` one
    of MEDIA_DISCIPLINE_RAW_FALLBACK) while dantesync answers; else DRIFT when the window is ready and
    |drift_us| > drift_bound_us; else OK. Mirror of camera_box::genlock_lock_state::media_clock_verdict
    (the discipline is the widget's token: active/disabled/read_failed/api_missing/unknown/n/a)."""
    if clock_present and discipline in MEDIA_DISCIPLINE_RAW_FALLBACK:
        return MC_UNDISCIPLINED
    if window_ready and abs(drift_us) > drift_bound_us:
        return MC_DRIFT
    return MC_OK


def decide(n_inputs, n_locked, recent_event, qpc_drift_beyond_bound, clock_present,
           clock_locked, clock_ntp_failed, output_present, output_stamping, n_absent=0,
           audio_unpaired=False, audio_unexpected=False, n_idle=0, media_clock=MC_OK):
    """Pure three-state decision -- a byte-faithful mirror of src/genlock_lock_state.rs `decide`.

    UNLOCKED precedence: clock (absent/unlocked) > output (present but not stamping) >
    no-input-locked. DEGRADED precedence (only when no UNLOCKED condition holds): some-input-
    unlocked > recent-event > ntp-failed > qpc-drift > media-clock (issue 1372 part D) >
    audio-pairing > audio-unexpected (#1303). Otherwise LOCKED. Returns (state, reason).
    `media_clock` is the verdict token (ok/drift/undisciplined); anything but ok DEGRADES, it never
    UNLOCKS. Default ok so an earlier caller reproduces the old verdict exactly.

    #1299/#1341: `n_absent` = senderless inputs (no live NDI connection); `n_idle` = CONNECTED-but-
    IDLE inputs (keep-alive-only, received-frame rate below the idle floor). The input decisions judge
    only CONNECTED-non-idle inputs (`n_connected = n_inputs - n_absent - n_idle`), so neither a
    senderless nor a keep-alive-only input ever DEGRADES; inputs-present-but-ALL-absent/idle is
    HEALTHY-idle (LOCKED), not UNLOCKED. Both default 0 so a pre-#1299/#1341 caller reproduces the old
    verdict exactly.
    """
    # #1299/#1341 -- connected-non-idle inputs only (max(0, ...) keeps the decision total under a
    # transient n_absent + n_idle > n_inputs, mirroring the Rust saturating_sub / the C clamp).
    n_connected = max(0, n_inputs - n_absent - n_idle)

    # --- UNLOCKED (red): clock > output > no-input-locked ---------------------------
    if not clock_present or not clock_locked:
        return (ST_UNLOCKED, R_CLOCK)
    if output_present and not output_stamping:
        return (ST_UNLOCKED, R_OUTPUT)
    if n_locked == 0:
        if n_inputs == 0:
            return (ST_UNLOCKED, R_NO_GENLOCK)          # no genlock configured at all
        if n_connected == 0:
            return (ST_LOCKED, R_NONE)                  # #1299: all senderless -> HEALTHY-idle
        return (ST_UNLOCKED, R_NO_INPUT_LOCKED)         # live senders, none locking -> fault

    # --- DEGRADED (amber): some-unlocked > recent-event > ntp > qpc ------------------
    if n_locked < n_connected:
        return (ST_DEGRADED, R_INPUT_UNLOCKED)
    if recent_event:
        return (ST_DEGRADED, R_RECENT_EVENT)
    if clock_ntp_failed:
        return (ST_DEGRADED, R_NTP_FAILED)
    if qpc_drift_beyond_bound:
        return (ST_DEGRADED, R_QPC_DRIFT)
    # Issue 1372 part D -- the audio (media) clock does not follow the disciplined wall clock: a
    # clock-class cause, above the audio-pairing symptoms.
    if media_clock != MC_OK:
        return (ST_DEGRADED, R_MEDIA_CLOCK)
    # #1303 -- the two lowest-precedence DEGRADED audio axes (below the video reasons): an
    # audio-enabled source unpaired with its video hold, then a silent-by-contract source found
    # audible. Both default False so a pre-#1303 caller reproduces the old verdict exactly.
    if audio_unpaired:
        return (ST_DEGRADED, R_AUDIO_PAIRING)
    if audio_unexpected:
        return (ST_DEGRADED, R_AUDIO_UNEXPECTED)

    # --- LOCKED (green) -------------------------------------------------------------
    return (ST_LOCKED, R_NONE)


def facet_from_obj(obj):
    """Return the nested `genlock_lock` facet dict from a parsed bundle-state object, or None when
    it is absent / not a dict (a stock OBS, or a box whose log has no genlock-lock-json: line yet).
    None means UNKNOWN downstream -- NEVER a fabricated UNLOCKED."""
    if not isinstance(obj, dict):
        return None
    facet = obj.get("genlock_lock")
    return facet if isinstance(facet, dict) else None


def _loads_obj(text):
    """Parse *text* as a JSON object; None on any failure (the ndi_halving #1203 precedent: a
    strict parse that raised got swallowed by the caller's 2>/dev/null and read as SKIP forever)."""
    if not (text or "").strip():
        return None
    try:
        obj = json.loads(text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def classify(state, box_reachable):
    """One box's verdict from the facet's carried `state` string.

      box_reachable != 1   -> SKIP     (defer to #732/#1001; never our page)
      state is None        -> UNKNOWN  (facet absent; no reading to judge -- never a false UNLOCKED)
      state == LOCKED      -> HEALTHY
      state == DEGRADED    -> DEGRADED
      state == UNLOCKED    -> UNLOCKED
      any other string     -> UNKNOWN  (fail-safe: an unrecognised state is never paged)
    """
    if box_reachable != 1:
        return "SKIP"
    if state is None:
        return "UNKNOWN"
    if state == ST_LOCKED:
        return "HEALTHY"
    if state == ST_DEGRADED:
        return "DEGRADED"
    if state == ST_UNLOCKED:
        return "UNLOCKED"
    return "UNKNOWN"


def analyze(bundle_json_text, box_reachable):
    """Fetch-result -> `{verdict, state, reason, n_inputs, n_locked}`. SKIP without parsing when the
    box was not reachable this pass; UNKNOWN (state None) when the facet is absent."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "state": None, "reason": None,
                "n_inputs": None, "n_locked": None, "n_absent": None, "n_idle": None,
                "qpc_drift_ppm": None, "qpc_expected_ppm": None,
                "media_clock": None, "media_clock_drift_us": None, "media_clock_discipline": None}
    facet = facet_from_obj(_loads_obj(bundle_json_text))
    if facet is None:
        return {"verdict": "UNKNOWN", "state": None, "reason": None,
                "n_inputs": None, "n_locked": None, "n_absent": None, "n_idle": None,
                "qpc_drift_ppm": None, "qpc_expected_ppm": None,
                "media_clock": None, "media_clock_drift_us": None, "media_clock_discipline": None}
    state = facet.get("state")
    reason = facet.get("reason")
    n_inputs = facet.get("n_inputs")
    n_locked = facet.get("n_locked")
    n_absent = facet.get("n_absent")  # #1299: senderless inputs (observability; the widget already
                                      # decided `state`, so this never changes the verdict here).
    n_idle = facet.get("n_idle")      # #1341: connected-but-idle inputs (observability / card text).
    reason = _enrich_recent_event_reason(reason, facet)
    reason = _enrich_audio_unexpected_reason(reason, facet)
    reason = _enrich_media_clock_reason(reason, facet)
    mc = facet.get("media_clock") if isinstance(facet.get("media_clock"), dict) else {}
    return {"verdict": classify(state, box_reachable), "state": state, "reason": reason,
            "n_inputs": n_inputs, "n_locked": n_locked, "n_absent": n_absent, "n_idle": n_idle,
            # #1299 Part 4: windowed drift telemetry (report-only; since #1357 the widget's qpc_drift
            # verdict is the wall STEP only, so these never change `state` — logged so a rate anomaly,
            # e.g. a second clock writer slewing the wall, is visible in-band).
            "qpc_drift_ppm": facet.get("qpc_drift_ppm"),
            "qpc_expected_ppm": facet.get("qpc_expected_ppm"),
            # issue 1372 part D: the audio (media) clock facet (None for a pre-v7 line).
            "media_clock": mc.get("state"),
            "media_clock_drift_us": mc.get("drift_us"),
            "media_clock_discipline": mc.get("discipline")}


def _enrich_recent_event_reason(reason, facet):
    """#1299 Part 3: for a `recent_event` reason, append the top offending input's name so the
    watchdog log line + Discord body read `recent_event:<name>` (an actionable page). The widget
    carries the offender in the v3 `recent_event_inputs` list; when it is absent/empty (a v1/v2 line,
    or no offender) the bare `recent_event` token is returned unchanged — never `recent_event:` with
    an empty name. Any other reason is returned verbatim (a stray offender list never corrupts it)."""
    if reason != R_RECENT_EVENT:
        return reason
    rei = facet.get("recent_event_inputs")
    if not isinstance(rei, list) or not rei:
        return reason
    top = rei[0]
    if not isinstance(top, dict):
        return reason
    name = top.get("name")
    if not isinstance(name, str) or not name:
        return reason
    return f"{R_RECENT_EVENT}:{name}"


def _enrich_audio_unexpected_reason(reason, facet):
    """#1303: for an `audio_unexpected` reason, append the offending input's name so the watchdog
    log line + Discord body read `audio_unexpected:<name>` (an actionable page — WHICH source is
    bleeding audio into a Dante-fed mix). The widget carries the offender in the v4
    `audio_unexpected_inputs` list; when it is absent/empty (a v1/v2/v3 line, or no offender) the
    bare `audio_unexpected` token is returned unchanged — never `audio_unexpected:` with an empty
    name. Any other reason is returned verbatim (a stray offender list never corrupts it)."""
    if reason != R_AUDIO_UNEXPECTED:
        return reason
    aui = facet.get("audio_unexpected_inputs")
    if not isinstance(aui, list) or not aui:
        return reason
    top = aui[0]
    if not isinstance(top, dict):
        return reason
    name = top.get("name")
    if not isinstance(name, str) or not name:
        return reason
    return f"{R_AUDIO_UNEXPECTED}:{name}"


def _enrich_media_clock_reason(reason, facet):
    """Issue 1372 part D: for a `media_clock` reason, append the sub-kind so the watchdog log line +
    Discord body read `media_clock:drift` or `media_clock:undisciplined` (the fix differs: a drifting
    mixer vs a Windows clock that fell back to raw QPC). A pre-v7 line / a malformed facet returns the
    bare token; any other reason is returned verbatim."""
    if reason != R_MEDIA_CLOCK:
        return reason
    mc = facet.get("media_clock")
    if not isinstance(mc, dict):
        return reason
    kind = mc.get("state")
    if not isinstance(kind, str) or not kind:
        return reason
    return f"{R_MEDIA_CLOCK}:{kind}"


def _fmt(v):
    """key=value rendering for the shell: None -> empty string (the UNKNOWN/absent contract)."""
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure genlock-lock watchdog decisions (#1299)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze",
                       help="read /bundle-state.json on stdin -> verdict + state + reason + inputs")
    a.add_argument("--box-reachable", type=int, required=True)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # Well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway (the ndi_halving #1203
        # hotfix precedent: a strict read that raised got swallowed by 2>/dev/null -> SKIP forever).
        # box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable)
        for k, key in (("verdict", "verdict"), ("state", "state"), ("reason", "reason"),
                       ("n_inputs", "n_inputs"), ("n_locked", "n_locked"), ("n_absent", "n_absent"),
                       ("n_idle", "n_idle"),
                       ("qpc_drift_ppm", "qpc_drift_ppm"), ("qpc_expected_ppm", "qpc_expected_ppm"),
                       ("media_clock", "media_clock"), ("media_clock_drift_us", "media_clock_drift_us"),
                       ("media_clock_discipline", "media_clock_discipline")):
            print(f"{k}={_fmt(res[key])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
