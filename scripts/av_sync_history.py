#!/usr/bin/env python3
"""#1265 -- the append-only per-run history log for the #856 rig-wide A/V controller.

The loop-gain fix (Prístup 1, this lane) stabilizes the controller NOW; the adaptive-slope
estimator (Prístup 2) is deferred until there are >= 5 data points. This module STARTS collecting
that corpus: one JSON object per run appended to `~/.camera-box/av-sync-history.jsonl`:

    run_id, ts, pin_at_measure, residual_median_ms, residual_spread_ms,
    proposed_offset_ms (the DAMPED correction, when one was computed),
    loop_gain, combined_offset_ms_raw,
    held (bool) and EITHER applied_pin (a proceed) OR hold_reason (a HOLD).

It reads the two dev1 state files the #1265 guard lib already writes -- `av-sync-residual-last.json`
(this run's measured state, written EVERY run by `av_sync_persist_residual` BEFORE the apply) for
run_id / pin_at_measure / residual / spread, and `av-sync-last.json` (the last-APPLIED pin, written
AFTER the apply) for `applied_pin` -- and appends a complete record.

Runs inside `recording-e2e.sh` cleanup()'s EXIT trap, so it must NEVER truncate the log, must
tolerate a missing dir/file, and must ALWAYS exit 0. It is a no-op when this run produced no
measurement (residual-last.json missing or a DIFFERENT run_id -- so a stale prior line is never
re-recorded). Pure logic + plain file I/O, fully Tier-0 testable with tmp files.

Usage (recording-e2e.sh cleanup, via scripts/lib/av-sync-apply-guard.sh):
    av_sync_history.py append --run-id R --proposed-offset-ms P --hold-reason H \
        --loop-gain G --combined-offset-ms-raw C \
        --residual-last <path> --last-applied <path> --dest <path>
"""
import argparse
import json
import os
import sys
import time


def _read_json(path):
    """Parse `path` as a JSON object, or None (missing / unreadable / non-object). Never raises."""
    if not path or not os.path.isfile(path):
        return None
    try:
        with open(path) as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def _num(x):
    """str/float/int/None -> float, or None for None/empty/unparseable."""
    if x is None:
        return None
    s = str(x).strip()
    if s == "":
        return None
    try:
        return float(s)
    except (ValueError, TypeError):
        return None


def build_record(residual_last, last_applied, run_id, proposed_offset_ms, hold_reason,
                 loop_gain, combined_offset_ms_raw, now_ts=None):
    """Build one history record dict, or None when there is nothing to record for THIS run.

    Returns None if `residual_last` is missing OR its run_id does not match `run_id` (this run
    produced no measurement -- `av_sync_persist_residual` no-op'd, so residual-last.json is a prior
    run's; never record a stale line). Otherwise builds the record from residual_last (measured
    state) + the args, adding `applied_pin` (from last_applied) on a proceed, or `hold_reason` on a
    HOLD. Optional fields absent in the inputs are omitted (never written as null)."""
    if not isinstance(residual_last, dict):
        return None
    if str(residual_last.get("run_id")) != str(run_id):
        return None

    rec = {"run_id": str(run_id)}
    ts = residual_last.get("ts")
    rec["ts"] = ts if isinstance(ts, (int, float)) else (
        now_ts if now_ts is not None else time.time()
    )
    for key in ("pin_at_measure", "residual_median_ms", "residual_spread_ms"):
        v = residual_last.get(key)
        if isinstance(v, (int, float)):
            rec[key] = v

    gain = _num(loop_gain)
    if gain is not None:
        rec["loop_gain"] = gain
    raw = _num(combined_offset_ms_raw)
    if raw is not None:
        rec["combined_offset_ms_raw"] = raw
    proposed = _num(proposed_offset_ms)
    if proposed is not None:
        rec["proposed_offset_ms"] = proposed

    held = bool((hold_reason or "").strip())
    rec["held"] = held
    if held:
        rec["hold_reason"] = hold_reason.strip()
    else:
        # a proceed run records the pin that actually landed (last-applied, written post-apply).
        applied = _num((last_applied or {}).get("applied_latency_ms"))
        if applied is not None:
            rec["applied_pin"] = int(applied) if float(applied).is_integer() else applied
    return rec


def append_history(dest_path, record):
    """Append ONE JSON object as a line to `dest_path` (append-only, never truncates). Creates the
    parent dir and the file if missing. A None record is a no-op. Never raises."""
    if record is None:
        return
    try:
        parent = os.path.dirname(dest_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(dest_path, "a") as f:
            f.write(json.dumps(record) + "\n")
    except OSError as e:
        sys.stderr.write(f"[av-sync] WARNING: could not append av-sync history to {dest_path}: {e}\n")


def _default_residual_last():
    return os.path.join(os.path.expanduser("~"), ".camera-box", "av-sync-residual-last.json")


def _default_last_applied():
    return os.path.join(os.path.expanduser("~"), ".camera-box", "av-sync-last.json")


def _default_dest():
    return os.path.join(os.path.expanduser("~"), ".camera-box", "av-sync-history.jsonl")


def _main(argv):
    ap = argparse.ArgumentParser(description="#1265 #856 A/V-controller history log")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("append", help="append one per-run record; always exits 0")
    a.add_argument("--run-id", required=True)
    a.add_argument("--proposed-offset-ms", default="")
    a.add_argument("--hold-reason", default="")
    a.add_argument("--loop-gain", default="")
    a.add_argument("--combined-offset-ms-raw", default="")
    a.add_argument("--residual-last", default=None)
    a.add_argument("--last-applied", default=None)
    a.add_argument("--dest", default=None)
    ns = ap.parse_args(argv)

    if ns.cmd == "append":
        try:
            residual_last = _read_json(ns.residual_last or _default_residual_last())
            last_applied = _read_json(ns.last_applied or _default_last_applied())
            record = build_record(
                residual_last, last_applied, ns.run_id,
                ns.proposed_offset_ms, ns.hold_reason,
                ns.loop_gain, ns.combined_offset_ms_raw,
            )
            append_history(ns.dest or _default_dest(), record)
        except Exception as e:  # noqa: BLE001 - runs in the cleanup EXIT trap; never abort the run
            sys.stderr.write(f"[av-sync] WARNING: av-sync history append failed: {e}\n")
        return 0
    return 2


def main(argv=None):
    return _main(sys.argv[1:] if argv is None else argv)


if __name__ == "__main__":
    sys.exit(main())
