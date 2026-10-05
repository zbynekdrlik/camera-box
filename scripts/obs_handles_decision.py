#!/usr/bin/env python3
"""issue 1406 -- PURE decision core of the dev1 OBS handle-leak alert watchdog.

WHY: on 5.10.2026 the stream obs64 held 4,066,772 handles after ~22 h. The third-party Audio
Monitor plugin's output listed an absent Focusrite endpoint, and every audio tick it opened that
endpoint's property-store registry key and never closed it: +46.875 handles/s (48000/1024),
168,750/h, kernel paged pool at 1.7 GB, ~100 h from the 16,777,216 per-process cap -- which would
have frozen OBS during the next production. Nothing read a handle count; the leak was found by
accident. bundle_state_gather now serves the OBS process's count on each box's :8899
(`obs_handles`, `obs_handles_pid`, `obs_handles_start`, Linux `obs_handles_run` +
`obs_handles_limit`); this module grades one pass's reading against the reference sample the
watchdog kept from its last pass, and scripts/obs-handles-alert-watchdog.sh drives the confirm,
throttle and time-bucketed notify.

The process identity is `<pid>@<run token>` when the box serves one (Linux: boot id + start ticks,
which a wall-clock step never moves -- strih-lx steps its clock as the dantesync date master),
else `<pid>@<start epoch>` (Windows: the create time is recorded once, so it never moves either).

Verdicts (one per pass), decided in this order:
  SKIP      box not fetched (:8899 / box down) -- deferred to the bundle-state / reach watchdogs.
  UNKNOWN   facet absent (an older server, or no readable OBS process) or garbled. Never a page.
  BASELINE  the first reading of this process (no reference, or a new identity = a restart): the
            reference resets. A restart clears the alarm.
  REBASE    the same process with an interval <= 0 (the dev1 clock stepped back, two runs in one
            second): only the reference resets; the alarm and its confirm are left alone.
  HOLD      the same process under MIN_INTERVAL_S since the reference (a manual run between timer
            passes): the older reference is kept, so a short interval never inflates the rate, and
            neither arm's confirm moves -- also over the ceiling.
  CEILING   handles >= the ceiling: DEFAULT_CEILING (500,000), or 80% of the box's own soft
            open-files limit when that is lower (Linux strih-lx: its RLIMIT_NOFILE is the cap; a
            Windows box reports no limit). The reference still advances.
  GROWING   the count grew at >= the growth bound over this interval: GROWTH_PER_H, or half a
            reported soft limit per hour when that is lower.
  HEALTHY   otherwise.

Calibration (the 5.10 readings on the ticket):
  * healthy stream OBS after the fix: ~5,790 handles, flat (+-16 over 2.5 min, ~384/h as a rate);
  * the leak: 168,750 handles/h; 4,066,772 after ~22 h;
  * GROWTH_PER_H = 5,000/h: 34x below the leak rate, ~13x above that healthy wobble's rate. ONE
    interval over the bound is not a page: the watchdog confirms GROWING over 3 consecutive passes
    (~15 min), so a one-off step (a scene loading its sources, an NDI reconnect) reads GROWING once
    and HEALTHY next. The leak gains only ~42k in 15 min;
  * on a Linux box the bound is at most half its soft open-files limit per hour (512/h under the
    usual 1024, ~43 fds per pass): a fixed 5,000/h would be ~5x that whole limit per hour, and the
    leak would hit EMFILE long before paging. A larger limit keeps the 5,000/h;
  * DEFAULT_CEILING = 500,000: 86x the healthy count, 3% of the Windows cap; the 5.10 leak
    crossed it ~3 h after OBS started. Confirmed over 2 passes.
Both inputs are quality-gated per .claude/rules/watchdog-notify-dedup.md: the growth is a rate over
one pass interval (never a since-start quantity) and the ceiling is the current level.

No I/O -- pytest Tier-0 (the render_freeze_decision.py / audio_mixer_decision.py precedent).
"""
import argparse
import json
import sys

WINDOWS_HANDLE_CAP = 16_777_216     # 2**24, the per-process handle cap on Windows
DEFAULT_CEILING = 500_000
DEFAULT_LIMIT_FRACTION = 0.8        # of a reported (Linux) soft open-files limit
DEFAULT_GROWTH_PER_H = 5_000.0
DEFAULT_LIMIT_GROWTH_FRACTION = 0.5  # of a reported soft limit, per hour
DEFAULT_MIN_INTERVAL_S = 240        # the timer runs every 300 s; a manual run between passes HOLDs
DEFAULT_GROWTH_CONFIRM = 3          # consecutive GROWING passes before a page (~15 min)
DEFAULT_CEILING_CONFIRM = 2


def _loads_obj(text):
    try:
        obj = json.loads(text or "")
    except (TypeError, ValueError):
        return None
    return obj if isinstance(obj, dict) else None


def _nonneg_int(obj, key):
    """A facet value -> int >= 0, or None for absent / empty / garbled / negative."""
    raw = obj.get(key) if isinstance(obj, dict) else None
    try:
        value = int(str(raw).strip())
    except (TypeError, ValueError):
        return None
    return value if value >= 0 else None


def reading(obj):
    """The facet in a fetched body -> (handles, pid, start, run, limit) or None when unusable. A
    count without a pid is unusable: the restart detection needs the process identity."""
    handles = _nonneg_int(obj, "obs_handles")
    pid = _nonneg_int(obj, "obs_handles_pid")
    if handles is None or pid is None:
        return None
    limit = _nonneg_int(obj, "obs_handles_limit")
    run = str(obj.get("obs_handles_run") or "").strip() or None
    return handles, pid, _nonneg_int(obj, "obs_handles_start"), run, (limit or None)


def identity(pid, start, run=None):
    """The process identity a reference belongs to: `<pid>@<run token>` when the box serves one,
    else `<pid>@<start epoch>`, else the pid alone."""
    if run:
        return f"{pid}@{run}"
    return f"{pid}@{start}" if start is not None else str(pid)


def effective_ceiling(ceiling, limit, limit_fraction):
    """The absolute ceiling, lowered to `limit_fraction` of a reported soft limit."""
    if limit:
        return min(ceiling, int(limit * limit_fraction))
    return ceiling


def effective_growth_bound(growth_per_h, limit, limit_growth_fraction):
    """The absolute growth bound (handles/h), lowered to `limit_growth_fraction` of a reported soft
    limit per hour: a small Linux fd limit is exhausted long before a Windows-scale rate."""
    if limit:
        return min(growth_per_h, limit * limit_growth_fraction)
    return growth_per_h


def _growth(handles, now_epoch, ref_handles, ref_epoch):
    """-> (interval_s, rate_per_h); rate None when there is no usable reference interval."""
    if ref_handles is None or ref_epoch is None or now_epoch is None:
        return None, None
    interval = now_epoch - ref_epoch
    if interval <= 0:
        return interval, None
    return interval, (handles - ref_handles) * 3600.0 / interval


def _same_process_verdict(handles, now_epoch, ref, rate_bound, min_interval_s):
    """The reading belongs to the reference's process -> (verdict, interval_s, rate_per_h, next
    reference). REBASE (interval <= 0) and HOLD (interval under the minimum) are no observation:
    neither arm's confirm moves."""
    ident, ref_handles, ref_epoch = ref
    interval, rate = _growth(handles, now_epoch, ref_handles, ref_epoch)
    if rate is None:
        return "REBASE", interval, None, (ident, handles, now_epoch)
    if interval < min_interval_s:
        return "HOLD", interval, None, ref
    return ("GROWING" if rate >= rate_bound else "HEALTHY"), interval, rate, (ident, handles,
                                                                             now_epoch)


def classify(read, box_reachable, now_epoch, ref, ceiling=DEFAULT_CEILING,
             limit_fraction=DEFAULT_LIMIT_FRACTION, growth_per_h=DEFAULT_GROWTH_PER_H,
             min_interval_s=DEFAULT_MIN_INTERVAL_S,
             limit_growth_fraction=DEFAULT_LIMIT_GROWTH_FRACTION):
    """One pass -> dict(verdict, interval_s, rate_per_h, ceiling, growth_bound_per_h, cap,
    hours_to_cap, next_ref). `ref` is (ident, handles, epoch) of the watchdog's reference sample
    (each may be None)."""
    ref_ident, ref_handles, ref_epoch = ref
    out = {"verdict": "SKIP", "interval_s": None, "rate_per_h": None, "ceiling": None,
           "growth_bound_per_h": None, "cap": None, "hours_to_cap": None, "next_ref": ref}
    if box_reachable != 1:
        return out
    if read is None:
        out["verdict"] = "UNKNOWN"
        return out
    handles, pid, start, run, limit = read
    ident = identity(pid, start, run)
    out["ceiling"] = effective_ceiling(ceiling, limit, limit_fraction)
    out["growth_bound_per_h"] = effective_growth_bound(growth_per_h, limit, limit_growth_fraction)
    out["cap"] = limit or WINDOWS_HANDLE_CAP
    if ref_ident != ident or ref_handles is None or ref_epoch is None or now_epoch is None:
        verdict, out["next_ref"] = "BASELINE", (ident, handles, now_epoch)
    else:
        verdict, out["interval_s"], out["rate_per_h"], out["next_ref"] = _same_process_verdict(
            handles, now_epoch, (ident, ref_handles, ref_epoch), out["growth_bound_per_h"],
            min_interval_s)
    if out["rate_per_h"] is not None and out["rate_per_h"] > 0:
        out["hours_to_cap"] = max(0.0, (out["cap"] - handles) / out["rate_per_h"])
    if verdict not in ("REBASE", "HOLD") and handles >= out["ceiling"]:
        verdict = "CEILING"
    out["verdict"] = verdict
    return out


def analyze(bundle_json_text, box_reachable, now_epoch, ref_ident, ref_handles, ref_epoch,
            ceiling=DEFAULT_CEILING, limit_fraction=DEFAULT_LIMIT_FRACTION,
            growth_per_h=DEFAULT_GROWTH_PER_H, min_interval_s=DEFAULT_MIN_INTERVAL_S,
            limit_growth_fraction=DEFAULT_LIMIT_GROWTH_FRACTION):
    """The fetched body + the stored reference -> the verdict, the reading and the next reference
    (`next_ident` / `next_handles` / `next_epoch`) the watchdog stores for its next pass."""
    obj = _loads_obj(bundle_json_text) if box_reachable == 1 else None
    read = reading(obj) if obj is not None else None
    res = classify(read, box_reachable, now_epoch, (ref_ident, ref_handles, ref_epoch), ceiling,
                   limit_fraction, growth_per_h, min_interval_s, limit_growth_fraction)
    handles, pid, start, run, limit = read if read is not None else (None,) * 5
    next_ident, next_handles, next_epoch = res["next_ref"]
    return {
        "verdict": res["verdict"], "handles": handles, "pid": pid, "start": start, "run": run,
        "ident": identity(pid, start, run) if pid is not None else None, "limit": limit,
        "cap": res["cap"], "ceiling": res["ceiling"],
        "growth_bound_per_h": res["growth_bound_per_h"], "rate_per_h": res["rate_per_h"],
        "interval_s": res["interval_s"], "hours_to_cap": res["hours_to_cap"],
        "next_ident": next_ident, "next_handles": next_handles, "next_epoch": next_epoch,
    }


_KEYS = ("verdict", "handles", "pid", "start", "run", "ident", "limit", "cap", "ceiling",
         "growth_bound_per_h", "rate_per_h", "interval_s", "hours_to_cap", "next_ident",
         "next_handles", "next_epoch")


def _fmt(key, value):
    if value is None:
        return ""
    if key in ("rate_per_h", "growth_bound_per_h"):
        return str(int(round(value)))
    if key == "hours_to_cap":
        return f"{value:.1f}"
    return str(value)


def _opt_int(text):
    text = (text or "").strip()
    return int(text) if text.lstrip("-").isdigit() else None


def _main(argv):
    ap = argparse.ArgumentParser(description="pure OBS handle-leak decisions (issue 1406)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze", help="read /bundle-state.json on stdin -> one pass's verdict")
    a.add_argument("--box-reachable", type=int, required=True)
    a.add_argument("--now-epoch", type=int, required=True)
    a.add_argument("--ref-ident", default="")
    a.add_argument("--ref-handles", default="")
    a.add_argument("--ref-epoch", default="")
    a.add_argument("--ceiling", type=int, default=DEFAULT_CEILING)
    a.add_argument("--limit-fraction", type=float, default=DEFAULT_LIMIT_FRACTION)
    a.add_argument("--growth-per-h", type=float, default=DEFAULT_GROWTH_PER_H)
    a.add_argument("--min-interval-s", type=int, default=DEFAULT_MIN_INTERVAL_S)
    a.add_argument("--limit-growth-fraction", type=float, default=DEFAULT_LIMIT_GROWTH_FRACTION)
    ns = ap.parse_args(argv)
    # Tolerant read (the ndi_halving precedent): a strict read that raised would be swallowed by
    # the caller's 2>/dev/null and read as SKIP forever. box_reachable=0 needs no stdin.
    text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
    res = analyze(text, ns.box_reachable, ns.now_epoch, ns.ref_ident.strip() or None,
                  _opt_int(ns.ref_handles), _opt_int(ns.ref_epoch), ns.ceiling,
                  ns.limit_fraction, ns.growth_per_h, ns.min_interval_s,
                  ns.limit_growth_fraction)
    for k in _KEYS:
        print(f"{k}={_fmt(k, res[k])}")
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
