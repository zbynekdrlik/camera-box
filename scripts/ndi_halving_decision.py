#!/usr/bin/env python3
"""#1203 -- PURE decision core for the NDI per-connection rate-halving auto-heal watchdog.

WHY: the vendored DistroAV receiver loop can degrade a per-connection pull to ~HALF the sender's
cadence (2ME PGM: 15,0/s at a 30,0/s sender; recv_capture_v3 cap_avg ~65,9 ms vs a healthy ~16 ms),
starving the genlock FIFO. A `systemctl restart camera-box` does NOT clear it; a receiver REATTACH
(obs_phase2.py idle-receiver -> --restore, overlay keeps the latency pin) does -- live-confirmed on
the 2ME PGM leg 2026-08-25 (restored 30,0/s / 12,6 ms instantly). This is the pure kernel of the
dev1-side watchdog that DETECTS the halving from the stream OBS log and (when armed) drives that
cure. No I/O, no ssh, no OBS -- exhaustively unit-testable (pytest), the strih-nic-selfheal #1199
python-mirror precedent, so the decision RED->GREENs LOCALLY under Tier-0 #557.

THE TAP: the stream (receiver) OBS log prints, per input, every >=5.0 s:
  `HH:MM:SS.mmm: [distroav] recv-timing #797 '<obs input name>': n=<N> cap_avg=<X>ms cap_max=... out_avg=... out_max=...`
(vendor/distroav/src/ndi-source.cpp:1477; emit gated on `elapsed >= 5.0 && t797_n > 0`, then
`t797_n = 0` + `t797_last_log = now`).

THE LOAD-BEARING DECISION -- `n=` is PER-INTERVAL (reset-on-read, like the asio #1023 starved_blocks
tap; UNLIKE the cumulative `received=` counter #794/#1052 persist across passes). So each line's `n`
counts video frames in exactly `(prev_emit, this_emit]`, and the rate is measured WITHIN ONE PASS
from the last TWO lines: `fps = n_curr / (ts_curr - ts_prev)`, both timestamps from the lines' OWN
log prefixes (the #794 phantom-50 avoidance principle: numerator and denominator from the same two
real lines, never a wall-clock divisor). A pair spanning > max_window_s (a freeze/gap straddle) or
< min_window_s is UNMEASURABLE -> reseed, never a false HALVED.
"""
import argparse
import re
import sys

_TS_RE = re.compile(r"^\s*(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?$")
_N_RE = re.compile(r"\bn=(\d+)\b")
_CAP_RE = re.compile(r"\bcap_avg=([0-9]+(?:\.[0-9]+)?)ms")


def ts_to_seconds(ts):
    """`HH:MM:SS[.mmm][:]` (the OBS log-line prefix) -> seconds-of-day float, or None when the
    input does not strictly parse as a real clock time (an unparseable prefix -> UNMEASURABLE,
    never a guessed value)."""
    if ts is None:
        return None
    ts = ts.strip().removesuffix(":")  # strip the OBS-log-prefix trailing colon
    m = _TS_RE.match(ts)
    if not m:
        return None
    h, mm, s = int(m.group(1)), int(m.group(2)), int(m.group(3))
    if h > 23 or mm > 59 or s >= 60:
        return None
    frac = float("0." + m.group(4)) if m.group(4) else 0.0
    return h * 3600 + mm * 60 + s + frac


def parse_recv_timing(text, source):
    """Return `(ts_seconds_or_None, n, cap_avg)` for every recv-timing #797 line whose input name
    EXACTLY equals *source* (anchored on the trailing `':` so 'NDI 2ME PGM' never matches
    'NDI 2ME PGM (mv)'), in log order. Lines missing n= or cap_avg= are skipped (unusable)."""
    if not text:
        return []
    marker = f"recv-timing #797 '{source}':"
    rows = []
    for ln in text.splitlines():
        if marker not in ln:
            continue
        mn = _N_RE.search(ln)
        mc = _CAP_RE.search(ln)
        if not mn or not mc:
            continue
        # leading `HH:MM:SS[.mmm]` prefix (before the first ': ')
        mts = re.match(r"\s*(\d{2}:\d{2}:\d{2}(?:\.\d+)?)", ln)
        ts = ts_to_seconds(mts.group(1)) if mts else None
        rows.append((ts, int(mn.group(1)), float(mc.group(1))))
    return rows


def measure(text, source, max_window_s=15.0):
    """WITHIN-PASS rate from the last TWO usable recv-timing lines for *source*.

    Returns a dict: samples (count of usable lines found), and fps/window_s/cap_avg/n -- the latter
    four are None when a rate could NOT be measured (fewer than 2 lines, either timestamp missing,
    n_curr's cumulative count is nonsensical, or the window is <=0 / > max_window_s -- a pair
    straddling a freeze/gap). `samples` still reports tap-liveness independent of measurability."""
    rows = parse_recv_timing(text, source)
    out = {"samples": len(rows), "fps": None, "window_s": None, "cap_avg": None, "n": None}
    if len(rows) < 2:
        return out
    (prev_ts, _prev_n, _prev_cap) = rows[-2]
    (curr_ts, curr_n, curr_cap) = rows[-1]
    if prev_ts is None or curr_ts is None:
        return out
    w = curr_ts - prev_ts
    if w < 0:
        w += 86400.0  # date-less OBS log: a window straddling midnight
    if w <= 0 or w > max_window_s:
        return out
    out["fps"] = curr_n / w
    out["window_s"] = w
    out["cap_avg"] = curr_cap
    out["n"] = curr_n
    return out


def classify(fps, cap_avg, expected_fps, window_s, box_reachable, expected_live,
             halving_ratio=0.6, cap_mult=2.0, healthy_ratio=0.85, healthy_cap_mult=1.5,
             min_window_s=3.0, max_window_s=15.0):
    """One input's verdict from a measured (fps, cap_avg, window_s).

      SKIP       -- out of scope this pass: not expected live, OR the box is not reachable
                    (issue-1001 already owns that page). Checked FIRST. Any flag != 1 is out-of-scope.
      UNKNOWN    -- nothing trustable to judge: unmeasurable fps/cap, or a window outside
                    [min_window_s, max_window_s]. Reseed; NEVER page on a shaky reading.
      HALVED     -- fps <= halving_ratio*expected  OR  cap_avg >= cap_mult*(1000/expected).
      HEALTHY    -- fps >= healthy_ratio*expected  AND cap_avg < healthy_cap_mult*(1000/expected).
      BORDERLINE -- between the bands (report-only; the caller holds the confirm counter, no page).
    The thresholds are PER-INPUT: the frame interval derives from that input's own expected_fps."""
    if box_reachable != 1 or expected_live != 1:
        return "SKIP"
    if fps is None or cap_avg is None or window_s is None:
        return "UNKNOWN"
    if window_s < min_window_s or window_s > max_window_s:
        return "UNKNOWN"
    if expected_fps <= 0:
        return "UNKNOWN"
    interval = 1000.0 / expected_fps
    halved = (fps <= halving_ratio * expected_fps) or (cap_avg >= cap_mult * interval)
    if halved:
        return "HALVED"
    healthy = (fps >= healthy_ratio * expected_fps) and (cap_avg < healthy_cap_mult * interval)
    if healthy:
        return "HEALTHY"
    return "BORDERLINE"


def analyze(text, source, expected_fps, box_reachable, expected_live, **kw):
    """parse + measure + classify from a raw log tail. When out of scope (box unreachable / not
    expected live) returns SKIP WITHOUT reading the log (samples=0), mirroring the caller's
    no-double-page guard where the raw log is empty anyway."""
    max_window_s = kw.get("max_window_s", 15.0)
    if box_reachable != 1 or expected_live != 1:
        return {"verdict": "SKIP", "fps": None, "cap_avg": None,
                "window_s": None, "n": None, "samples": 0}
    m = measure(text, source, max_window_s=max_window_s)
    verdict = classify(m["fps"], m["cap_avg"], expected_fps, m["window_s"],
                       box_reachable, expected_live, **kw)
    return {"verdict": verdict, "fps": m["fps"], "cap_avg": m["cap_avg"],
            "window_s": m["window_s"], "n": m["n"], "samples": m["samples"]}


def cooldown_elapsed(last_cure_ts, now, cooldown_s):
    """True when a fresh cure is allowed: never cured before (empty/None/unparseable last ts), or
    at least cooldown_s have elapsed since the last cure. Gates re-attach so a persistent halving is
    cured ONCE per cooldown window, then paged (never reattach-spam)."""
    if last_cure_ts is None or str(last_cure_ts).strip() == "":
        return True
    try:
        last = float(last_cure_ts)
        return (float(now) - last) >= float(cooldown_s)
    except (TypeError, ValueError):
        return True  # unparseable prior -> treat as never-cured, never block a needed cure


def cure_decision(cure_enabled, cooldown_ok):
    """A CONFIRMED-halved input either gets a reattach or a page:
      cure_enabled False -> "page" (report-only phase: alert, never touch the live receiver).
      cure_enabled True  + cooldown_ok True  -> "cure" (one reattach attempt).
      cure_enabled True  + cooldown_ok False -> "page" (already cured this episode, still halved
                                                 -> alert, no reattach-spam)."""
    if bool(cure_enabled) and bool(cooldown_ok):
        return "cure"
    return "page"


def _fmt(v):
    return "" if v is None else (f"{v:.3f}" if isinstance(v, float) else str(v))


def _main(argv):
    ap = argparse.ArgumentParser(description="pure NDI rate-halving decisions (#1203)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze", help="read raw OBS log on stdin -> verdict + measured values")
    a.add_argument("--source", required=True)
    a.add_argument("--expected-fps", type=float, required=True)
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--expected-live", type=int, required=True)
    a.add_argument("--halving-ratio", type=float, default=0.6)
    a.add_argument("--cap-mult", type=float, default=2.0)
    a.add_argument("--healthy-ratio", type=float, default=0.85)
    a.add_argument("--healthy-cap-mult", type=float, default=1.5)
    a.add_argument("--min-window-s", type=float, default=3.0)
    a.add_argument("--max-window-s", type=float, default=15.0)

    c = sub.add_parser("cure-decision", help="cure vs page for a CONFIRMED-halved input")
    c.add_argument("--cure-enabled", type=int, required=True)
    c.add_argument("--last-cure-ts", default="")
    c.add_argument("--now", required=True)
    c.add_argument("--cooldown-s", required=True)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        text = sys.stdin.read()
        res = analyze(text, ns.source, ns.expected_fps, ns.box_reachable, ns.expected_live,
                      halving_ratio=ns.halving_ratio, cap_mult=ns.cap_mult,
                      healthy_ratio=ns.healthy_ratio, healthy_cap_mult=ns.healthy_cap_mult,
                      min_window_s=ns.min_window_s, max_window_s=ns.max_window_s)
        for k in ("verdict", "fps", "cap_avg", "window_s", "n", "samples"):
            print(f"{k}={_fmt(res[k])}")
        return 0

    if ns.cmd == "cure-decision":
        ok = cooldown_elapsed(ns.last_cure_ts, ns.now, ns.cooldown_s)
        print(f"cooldown_ok={1 if ok else 0}")
        print(f"action={cure_decision(bool(ns.cure_enabled), ok)}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
