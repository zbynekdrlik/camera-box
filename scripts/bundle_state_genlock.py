#!/usr/bin/env python3
"""The OBS / genlock facet parsers of the bundle-state gather: the startup banner (OBS +
DistroAV version, the first reset block's fps), the genlock wall-clock + capability markers,
the fleet genlock LOCK facet (#1299), the PROGRAM-render freeze facet and the relock-burst
facet (#1320).

Part of the bundle-state gather split (issue 1386): a PURE facet family re-exported by
`bundle_state_gather` -- import it through that module, never directly (it resolves its flat
siblings). Ships in the :8899 server tree declared in `scripts/lib/bundle-state-files.txt`.
"""
from __future__ import annotations

import json
import re

from bundle_state_log import LOG_BOUNDED_READ_SEPARATOR, _log_line_seconds, _recency_gap_s


def obs_version_from_log(text):
    """"OBS 32.1.2 (64-bit, windows)" -> "32.1.2". "" if the log never printed it (UNKNOWN, per
    drift-guard's never-a-false-clean contract — the caller simply omits the key)."""
    m = re.search(r"OBS (\d+\.\d+\.\d+)", text or "")
    return m.group(1) if m else ""


def distroav_version_from_log(text):
    """"DistroAV (Version 6.2.1)" -> "6.2.1". "" if absent."""
    m = re.search(r"DistroAV \(Version (\d+\.\d+\.\d+)\)", text or "")
    return m.group(1) if m else ""


def output_fps_from_log(text):
    """The `fps:` line INSIDE the first "video settings reset:" block -> "30". "" if either the
    reset block or its fps line is absent. Mirrors `.claude/commands/drift-guard.md` step 1's
    PowerShell block-scoped scan line-for-line (first reset block only, first fps line inside it)."""
    lines = (text or "").splitlines()
    for i, line in enumerate(lines):
        if "video settings reset:" in line:
            for later in lines[i:]:
                m = re.search(r"fps:\s+(\d+)/", later)
                if m:
                    return m.group(1)
            break  # found the reset block but no fps line followed it — UNKNOWN, don't scan past it
    return ""


def genlock_wall_clock_from_log(text):
    """"1" (render tick ENABLED), "0" (DISABLED), "" if the build never logged the marker at all
    (a stock OBS, or OBS never launched — UNKNOWN, never guessed)."""
    t = text or ""
    if re.search(r"genlock:.*render tick ENABLED", t):
        return "1"
    if re.search(r"genlock:.*render tick DISABLED", t):
        return "0"
    return ""


def genlock_capability_from_log(text):
    """Every `genlock:` capability-marker line (render tick ENABLED / sub-frame jitter reserve /
    timestamp-aligned release) joined with '\\n' — the #122 build-unique tell. "" if the build
    emits none (a stock OBS). Gathered for forward-compat with the opt-in manifest/capability
    facet in drift-guard.sh; harmless when no manifest= is supplied (the current CI invocation)."""
    pattern = re.compile(r"genlock:.*(render tick ENABLED|sub-frame jitter reserve|timestamp-aligned release)")
    matches = [line for line in (text or "").splitlines() if pattern.search(line)]
    return "\n".join(matches)


# #1299 — the fleet-visible genlock LOCK facet. The #1298 statusbar widget is the ONE place the
# three genlock producers (per-source FIFO counters, the NDI output's wall-stamping flag, the
# dantesync :8898 clock facet) are joined into one decided verdict; it emits that verdict as a
# versioned `genlock-lock-json: {…} (#1299)` line (a heartbeat + on-change, so the #1222 bounded
# TAIL always holds a fresh one). This parser reads the NEWEST such line and reshapes the widget's
# payload into the nested `genlock_lock` facet the dev1 watchdog + rig-status read. Reusing the
# SAME already-bounded log_text as every other facet (no second read, no subprocess -> no new
# #1222 cache needed). "" / a stock OBS with no such line -> None (facet OMITTED downstream, never a
# false UNLOCKED).
_GENLOCK_LOCK_JSON_MARKER = "genlock-lock-json:"


def _genlock_lock_payload(text):
    """The widget payload (a dict) of the NEWEST `genlock-lock-json:` line in *text*, or None when
    there is no such line or it does not carry a JSON object."""
    t = text or ""
    if _GENLOCK_LOCK_JSON_MARKER not in t:
        return None
    # Newest line wins (the widget heartbeats this, so the last occurrence is the current state).
    newest = None
    for line in t.splitlines():
        if _GENLOCK_LOCK_JSON_MARKER in line:
            newest = line
    if newest is None:
        return None
    # The payload is a JSON object; slice from its first "{" to its last "}" so a leading log-time
    # prefix and the trailing " (#1299)" tag are both ignored. A malformed line -> None.
    start = newest.find("{")
    end = newest.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        payload = json.loads(newest[start:end + 1])
    except (ValueError, TypeError):
        return None
    if not isinstance(payload, dict):
        return None
    return payload


def _genlock_lock_inputs(raw_inputs):
    """The facet's per-input map `{name: {...}}` from the widget's `inputs` array; a row that is not
    an object or has no non-empty string name is skipped, a duplicate name keeps the last."""
    inputs_map = {}
    if isinstance(raw_inputs, list):
        for row in raw_inputs:
            if not isinstance(row, dict):
                continue
            name = row.get("name")
            if not isinstance(name, str) or not name:
                continue
            inputs_map[name] = {
                "locked": bool(row.get("locked")),
                # #1299 (schema v2): whether the DistroAV receiver has a live NDI connection. Default
                # True for a v1 line from an older build (no `connected` key) so a senderless-but-
                # unreported input reads connected, exactly as the pre-#1299 behaviour.
                "connected": bool(row.get("connected", True)),
                # #1341 (schema v6): whether this CONNECTED input is IDLE (keep-alive-only). Default
                # False for a pre-v6 line (no `idle` key) so an older line reads exactly as pre-#1341.
                "idle": bool(row.get("idle", False)),
                "latency_ms": row.get("latency_ms"),
                "underruns": row.get("underruns"),
                "relocks": row.get("relocks"),
                "late_holds": row.get("late_holds"),
                "depth": row.get("depth"),
            }
    return inputs_map


def _genlock_lock_named_rows(raw_rows, with_events):
    """The offender list `[{name[, events]}]` of a v3 `recent_event_inputs` (with_events) or a v4
    `audio_unexpected_inputs` (names only). Each entry is tolerant: a row that is not an object or
    has no non-empty string name is skipped. `[]` when *raw_rows* is not a list."""
    rows = []
    if isinstance(raw_rows, list):
        for row in raw_rows:
            if not isinstance(row, dict):
                continue
            name = row.get("name")
            if not isinstance(name, str) or not name:
                continue
            if with_events:
                rows.append({"name": name, "events": row.get("events")})
            else:
                rows.append({"name": name})
    return rows


def _genlock_lock_media_clock(raw_mc):
    """The v7 `media_clock` sub-facet, or None for an absent / malformed object (no string
    `state`)."""
    if isinstance(raw_mc, dict) and isinstance(raw_mc.get("state"), str):
        return {
            "state": raw_mc.get("state"),
            "drift_us": raw_mc.get("drift_us"),
            "window_s": raw_mc.get("window_s"),
            "ready": bool(raw_mc.get("ready")),
            "discipline": raw_mc.get("discipline"),
        }
    return None


def genlock_lock_facet_from_log(text):
    """The nested `genlock_lock` facet dict from the NEWEST `genlock-lock-json:` line in *text*, or
    None when the line is absent / unparseable (a stock OBS, or no such line in the bounded window
    yet — UNKNOWN downstream, NEVER a fabricated UNLOCKED).

    Shape:
      {state, reason, n_inputs, n_locked, n_absent, n_idle, latency_ms, recent_event, qpc_drift_ms,
       qpc_drift_ppm, qpc_expected_ppm, qpc_step,
       clock:{state}, output:{present, stamping_wallclock},
       inputs:{<name>:{locked, connected, idle, latency_ms, underruns, relocks, late_holds, depth}},
       [recent_event_inputs:[{name, events}]], [audio_unexpected_inputs:[{name}]],
       [media_clock:{state, drift_us, window_s, ready, discipline}], source:"log"}

    Issue 1372 part D (schema v7): `media_clock` is the audio (media) clock facet the widget decided
    with -- `state` ok|drift|undisciplined, the wall-vs-media drift `drift_us` per `window_s` (the
    time-weighted rate of the non-step pairs, scaled to the window),
    whether that window has filled (`ready`), and the Windows discipline outcome (`discipline`:
    active|disabled|read_failed|api_missing|unknown, or n/a on Linux). Omitted for a pre-v7 line or a
    malformed object, never fabricated.

    #1341 (schema v6): `n_idle` (connected-but-keep-alive-only input count) and per-input `idle`
    distinguish an idle SongPlayer playlist input from a live one so the fleet indicator never
    false-flaps DEGRADED/recent_event on it. Both degrade gracefully for a pre-v6 line
    (`n_idle`->None, per-input `idle`->False).

    #1299 (schema v5, Part 4): `qpc_drift_ppm` (measured windowed drift rate), `qpc_expected_ppm` (the
    dantesync-reported slew) and `qpc_step` (a single-sample wall STEP tripped) are report-only
    telemetry — the qpc_drift VERDICT (since #1357 the wall STEP only) is folded into `state` by the widget.
    All three default to None for a v1-v4 line from an older build (the cumulative `qpc_drift_ms` stays).

    #1299 (schema v3, Part 3): `recent_event_inputs` is the top recent-event offender (name+count),
    present ONLY when the v3 line carries a non-empty list (a DEGRADED/recent_event page names it).
    Omitted for a v1/v2 line from an older build, or an empty list, so an older line never fabricates
    an attribution.

    #1303 (schema v4): `audio_unexpected_inputs` is a silent-by-contract source found AUDIBLE (the
    double-audio hazard), present ONLY when the v4 line carries a non-empty list. Omitted for a
    v1/v2/v3 line, or an empty list.

    #1299 (schema v2): `n_absent` (senderless input count) and per-input `connected` distinguish an
    idle NDI input (no sender) from a connected-but-unlocked one so the fleet watchdog never
    false-pages a legitimately-idle input. Both degrade gracefully for a v1 line from an older
    build (`n_absent`->None, `connected`->True).

    `state`/`reason` are the verdict the widget ALREADY decided (so the facet can never disagree
    with the statusbar). The per-input array is keyed by name; a duplicate name keeps the last."""
    payload = _genlock_lock_payload(text)
    if payload is None:
        return None

    # Reshape the widget payload into the facet. Every field is tolerant of absence (a future
    # widget that drops a field must degrade, never crash this gather).
    clock_str = payload.get("clock")
    output_str = payload.get("output")
    facet = {
        "state": payload.get("state"),
        "reason": payload.get("reason"),
        "n_inputs": payload.get("n_inputs"),
        "n_locked": payload.get("n_locked"),
        # #1299 (schema v2): senderless (no-NDI-connection) input count. None for a v1 line from an
        # older build -> the decision treats absent as 0, i.e. the pre-#1299 all-connected reading.
        "n_absent": payload.get("n_absent"),
        # #1341 (schema v6): CONNECTED-but-IDLE (keep-alive-only) input count. None for a pre-v6 line
        # -> the decision treats idle as 0 (the pre-#1341 reading); the widget already decided `state`.
        "n_idle": payload.get("n_idle"),
        "latency_ms": payload.get("latency_ms"),
        "recent_event": bool(payload.get("recent_event")),
        "qpc_drift_ms": payload.get("qpc_drift_ms"),
        # #1299 Part 4 (schema v5): windowed wall-vs-QPC drift telemetry (report-only). Since #1357 the
        # qpc_drift VERDICT is the wall STEP only (`qpc_step`) — neither the rate (`qpc_drift_ppm`) nor
        # the dantesync slew (`qpc_expected_ppm`) nor the cumulative `qpc_drift_ms` above gates.
        # All three default to None for a v1-v4 line from an older build.
        "qpc_drift_ppm": payload.get("qpc_drift_ppm"),
        "qpc_expected_ppm": payload.get("qpc_expected_ppm"),
        "qpc_step": payload.get("qpc_step"),
        "clock": {"state": clock_str} if isinstance(clock_str, str) else {},
        "output": {
            "present": output_str != "absent",
            "stamping_wallclock": output_str == "stamping",
        } if isinstance(output_str, str) else {},
        "inputs": _genlock_lock_inputs(payload.get("inputs")),
        "source": "log",
    }

    # #1299 (schema v3, Part 3): the top recent-event offender(s) — [{name, events}] — so a
    # DEGRADED/recent_event page can NAME the offending input (reason=recent_event:<name>). Omit the
    # key entirely when absent (a v1/v2 line from an older build) or empty (no offender), so an older
    # line never fabricates an attribution.
    rei = _genlock_lock_named_rows(payload.get("recent_event_inputs"), with_events=True)
    if rei:
        facet["recent_event_inputs"] = rei

    # #1303 (schema v4): the audio-unexpected offender(s) — [{name}] — a silent-by-contract source
    # found AUDIBLE, so a DEGRADED/audio_unexpected page can NAME it (reason=audio_unexpected:<name>).
    # Omit the key entirely when absent (a v1/v2/v3 line from an older build) or empty (no offender),
    # so an older line never fabricates an attribution. Names-only (no events count — unlike
    # recent_event, an unexpected-audio input is a binary condition, not a cumulative counter).
    aui = _genlock_lock_named_rows(payload.get("audio_unexpected_inputs"), with_events=False)
    if aui:
        facet["audio_unexpected_inputs"] = aui

    # Issue 1372 part D (schema v7): the media (audio) clock facet. Omit the key entirely for a pre-v7
    # line or a malformed object (no string `state`), so an older build never reads as a verdict.
    media_clock = _genlock_lock_media_clock(payload.get("media_clock"))
    if media_clock is not None:
        facet["media_clock"] = media_clock

    return facet


# #1320 — the PROGRAM-render freeze signal. `program-render-audit:` (obs-video.c
# obs_graphics_thread_loop, ~5 s) carries the PROGRAM output's render cadence; `lagged` ==
# renderSkipped in that window, so a `lagged>0` window is a render-thread freeze.
_PROGRAM_RENDER_LAGGED_RE = re.compile(r"program-render-audit:.*?\blagged=(\d+)\b")


def program_render_lagged_from_log(text):
    """#1320 — the strih PROGRAM-render freeze signal `(max_lagged_str, age_s_str)`, `("", "")` when
    no `program-render-audit:` line exists (absent -> UNKNOWN downstream, never a fabricated 0).

    `program-render-audit:` (obs-video.c obs_graphics_thread_loop, ~5 s) reports the PROGRAM output's
    render cadence; `lagged` == renderSkipped in that window, so a `lagged>0` window is a render-
    thread freeze. Issue 1320: a scene-switch-coincident DistroAV reattach whose blocking
    NDIlib_recv_destroy ran on the graphics thread froze the PROGRAM render ~7.5 s (lagged=228
    avg_frame_ms=782) -> 2ME PGM starved -> stream FIFO underrun -> relock storm -> presented video
    +2/+3 frames late ~40 min. This facet exposes the MAX `lagged` across the tail's
    program-render-audit lines + the in-log age (whole seconds) of the MOST RECENT window achieving
    that max, so the dev1 watchdog can page on a RECENT freeze (not one that scrolled out of the
    tail). `"0"` (a healthy tail: render telemetry live, no freeze) is a truthy string and is KEPT;
    `""` (no telemetry at all) is dropped by the omit-when-empty filter.

    Reads ONLY the TAIL slice of the #1222 bounded head+separator+tail read (a freeze surviving only
    in the head is never reported; a small whole-file log is scanned entirely), in ONE pass (no
    second log read). File order is time order (append-only log), so `_recency_gap_s` corrects a
    single midnight wrap on the date-less OBS timestamps."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    log_newest_ts = None   # ts of the LAST parseable line in file order (the log write head)
    max_lagged = None
    max_ts = None          # ts of the most-recent line achieving max_lagged
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        m = _PROGRAM_RENDER_LAGGED_RE.search(line)
        if m:
            lagged = int(m.group(1))
            if max_lagged is None or lagged > max_lagged:
                max_lagged = lagged
                max_ts = ts
            elif lagged == max_lagged and ts is not None:
                max_ts = ts   # a LATER window at the same max -> report the fresher age
    if max_lagged is None:
        return ("", "")
    gap = _recency_gap_s(log_newest_ts, max_ts)
    age_s = "0" if gap is None else str(round(gap))
    return (str(max_lagged), age_s)


# #1320 — the RELOCK-BURST facet: a receiver FIFO overshoot STORM (the downstream consequence of a
# sender PROGRAM render freeze). PORTS issue 1318's summarize_relock_bursts (src/jitter_audit.rs) to
# Python so the dev1 render-freeze watchdog can page on it off :8899; the summarizer is NOT
# re-implemented across languages beyond this one mirror (the ndi_halving_decision / #1199
# python-mirror precedent) and a parity test pins it to the Rust test fixtures BYTE-for-byte.
RELOCK_BURSTS_MIN_DEFAULT = 8  # N: >= this many relocks within 1 s on ONE input == a burst (issue 1318)
_RELOCK_MARK = "genlock-relock '"


def _parse_hhmmss_ms(tok):
    """A `HH:MM:SS[.mmm]` (optional trailing `:`) token -> milliseconds-of-day, or None. Byte-faithful
    mirror of src/jitter_audit.rs `parse_hhmmss_ms`: strips one trailing `:`, requires exactly 3
    colon-parts, pads the fractional to 3 digits (`.205` -> 205 ms)."""
    if tok.endswith(":"):
        tok = tok[:-1]
    parts = tok.split(":")
    if len(parts) != 3:
        return None
    try:
        hh = int(parts[0])
        mm = int(parts[1])
    except ValueError:
        return None
    sec_frac = parts[2]
    if "." in sec_frac:
        s, f = sec_frac.split(".", 1)
        try:
            ss = int(s)
        except ValueError:
            return None
        fms = int((f + "000")[:3]) if f else 0
        sub_ms = ss * 1000 + fms
    else:
        try:
            sub_ms = int(sec_frac) * 1000
        except ValueError:
            return None
    return hh * 3_600_000 + mm * 60_000 + sub_ms


def _parse_relock_event(line):
    """`(source, at_ms_int)` from a `genlock-relock '<src>':` line, or None. Byte-faithful mirror of
    src/jitter_audit.rs parse_relock_line: needs the `genlock-relock '` marker (mutually non-substring
    vs `genlock-fifo audit '`/`genlock-ndi-*`) AND a parseable clock time in the LAST whitespace token
    before the marker (so a journald/SSH-wrapper-prefixed line still clusters, exactly like the Rust).
    An event with no timeline position cannot be clustered -> None."""
    i = line.find(_RELOCK_MARK)
    if i < 0:
        return None
    after = line[i + len(_RELOCK_MARK):]
    q = after.find("'")
    if q < 0:
        return None
    source = after[:q]
    before = line[:i].split()          # the token right before the marker carries the OBS timestamp
    if not before:
        return None
    at_ms = _parse_hhmmss_ms(before[-1])
    if at_ms is None:
        return None
    return (source, at_ms)


def _peak_in_window(times, window_ms):
    """Peak count of events within any `window_ms`-wide window over an ASCENDING slice (two-pointer,
    inclusive `t[j] - t[i] <= window_ms`). Mirror of src/jitter_audit.rs peak_in_window."""
    left = 0
    peak = 0
    for right in range(len(times)):
        while times[right] - times[left] > window_ms:
            left += 1
        peak = max(peak, right - left + 1)
    return peak


def _summarize_one_source_bursts(source, times, min_burst_relocks, window_ms):
    """Cluster ONE source's relock timestamps into burst episodes. A new cluster starts on a gap
    greater than `window_ms` OR a backward time step (midnight wrap / a new log concatenated); a
    cluster is a BURST when its densest `window_ms` window holds >= `min_burst_relocks` events.
    Mirror of src/jitter_audit.rs summarize_one_source_bursts."""
    summary = {
        "source": source,
        "total_relocks": len(times),
        "bursts": 0,
        "max_per_second": 0,
        "first_at_ms": times[0] if times else 0,
        "last_at_ms": times[-1] if times else 0,
    }
    cluster_start = 0
    for i in range(len(times)):
        boundary = i > cluster_start and (times[i] < times[i - 1] or times[i] - times[i - 1] > window_ms)
        if boundary:
            peak = _peak_in_window(times[cluster_start:i], window_ms)
            summary["max_per_second"] = max(summary["max_per_second"], peak)
            if peak >= min_burst_relocks:
                summary["bursts"] += 1
            cluster_start = i
    if cluster_start < len(times):
        peak = _peak_in_window(times[cluster_start:], window_ms)
        summary["max_per_second"] = max(summary["max_per_second"], peak)
        if peak >= min_burst_relocks:
            summary["bursts"] += 1
    return summary


def _summarize_relock_bursts(events, min_burst_relocks, window_ms):
    """Per-source relock-burst summaries (grouped in first-seen order, kept in log order). `events`
    is a list of `(source, at_ms)` tuples. Mirror of src/jitter_audit.rs summarize_relock_bursts."""
    order = []
    groups = {}
    for source, at_ms in events:
        if source not in groups:
            order.append(source)
            groups[source] = []
        groups[source].append(at_ms)
    return [_summarize_one_source_bursts(name, groups[name], min_burst_relocks, window_ms)
            for name in order]


def relock_bursts_from_log(text):
    """#1320 — the RELOCK-BURST facet `(max_bursts_str, age_s_str)`, `("", "")` when there is NO
    `genlock-relock` line at all (steady state; absent -> UNKNOWN downstream, never a fabricated 0).

    `max_bursts_str` is the MAX per-input burst count over the tail (>=8 relocks within 1 s ==
    a FIFO overshoot storm, issue 1318); `age_s_str` is the whole-second in-log age of the NEWEST
    relock event behind the tail's newest line of any kind, so the dev1 watchdog pages on a RECENT
    storm (not one that scrolled into the tail). `"0"` (relock telemetry live, no storm) is a truthy
    string and is KEPT; `""` is dropped by the omit-when-empty filter.

    Reads ONLY the TAIL slice of the #1222 bounded head+separator+tail read, in ONE pass (no second
    log read). File order is time order (append-only log), so `_recency_gap_s` corrects a single
    midnight wrap on the date-less OBS timestamps."""
    t = text or ""
    if LOG_BOUNDED_READ_SEPARATOR in t:
        t = t.rsplit(LOG_BOUNDED_READ_SEPARATOR, 1)[-1]
    log_newest_ts = None    # ts of the LAST parseable line in file order (the log write head)
    newest_relock_ts = None  # seconds-of-day of the newest relock line (file order == time order)
    events = []
    for line in t.splitlines():
        ts = _log_line_seconds(line)
        if ts is not None:
            log_newest_ts = ts
        ev = _parse_relock_event(line)
        if ev is not None:
            events.append(ev)
            if ts is not None:
                newest_relock_ts = ts
    if not events:
        return ("", "")
    summaries = _summarize_relock_bursts(events, RELOCK_BURSTS_MIN_DEFAULT, 1000)
    max_bursts = max(s["bursts"] for s in summaries)
    gap = _recency_gap_s(log_newest_ts, newest_relock_ts)
    age_s = "0" if gap is None else str(round(gap))
    return (str(max_bursts), age_s)
