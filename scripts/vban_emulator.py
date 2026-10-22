#!/usr/bin/env python3
"""Issue 1460 slice 1 -- the lv1 VBAN EMULATOR: sends a planned VBAN stream to one host:port.

The schedule (clock offset, jitter, stalls, losses, the pilot) is the pure planner's
(scripts/vban_emulator_plan.py, its module doc says what each knob means). This file only resolves
the target, paces the sends on ABSOLUTE deadlines of `time.monotonic_ns()` from one start instant
(a late wake never shifts the later packets, so the average rate is exactly the planned one), and
records what really happened (late sends, the counts).

The pilot must only ever reach an OBS TEST input on a dedicated recording track
(scripts/vban-receiver-bench.sh): the ports a real rig receiver listens on are refused
(`vban_emulator_plan.port_refusal`).

Usage:
  vban_emulator.py --target HOST [--port P] [--stream-name NAME] [--profile clean|lv1|stress]
                   [--ppm X] [--duration-s S] [--seed N] [--losses T:N,...|none] [--pilot-hz F]
                   [--level-dbfs D] [--start-frame N] [--schedule-out FILE] [--stats-out FILE]
                   [--dry-run [--print-packets N]]

--dry-run prints the schedule (its summary, every stall and every loss, the first N packets) and
sends nothing; --schedule-out is written in both modes BEFORE the first send, so a bench that kills
the sender still has the plan the recording is graded against.

Exit: 0 sent the whole run (or a dry run), 1 a send failed, 3 usage error, 5 stopped by a signal
(the stats are still written).
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import vban_emulator_plan as vp  # noqa: E402

LATE_REPORT_MS = 2.0


def _args(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--target", help="receiver host (required unless --dry-run)")
    ap.add_argument("--port", type=int, default=6995)
    ap.add_argument("--stream-name", default="bench1460")
    ap.add_argument("--profile", choices=sorted(vp.PROFILES), default="lv1")
    ap.add_argument("--ppm", type=float, default=None, help="sender clock offset (default: the profile's)")
    ap.add_argument("--duration-s", type=float, default=60.0)
    ap.add_argument("--seed", type=int, default=1460)
    ap.add_argument("--losses", default=None, help="T:N,... seconds:packets, or 'none' (default: the profile's)")
    ap.add_argument("--pilot-hz", type=float, default=vp.DEFAULT_PILOT_HZ)
    ap.add_argument("--level-dbfs", type=float, default=vp.DEFAULT_LEVEL_DBFS)
    ap.add_argument("--start-frame", type=int, default=0)
    ap.add_argument("--schedule-out")
    ap.add_argument("--stats-out")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--print-packets", type=int, default=0)
    return ap.parse_args(argv)


def _validated(a):
    """-> (profile, ppm, losses, amp) or raise ValueError naming the bad argument."""
    profile = vp.PROFILES[a.profile]
    ppm = profile.ppm if a.ppm is None else a.ppm
    ppm_u = vp.ppm_units(ppm)
    refusal = vp.port_refusal(a.port)
    if refusal:
        raise ValueError(refusal)
    if not vp.stream_name_ok(a.stream_name):
        raise ValueError("--stream-name must be 1..16 printable ASCII characters")
    if not a.dry_run and not a.target:
        raise ValueError("--target is required unless --dry-run")
    if not 2 * vp.LOSS_EDGE_S < a.duration_s <= 3600:
        raise ValueError("--duration-s must be in (%.0f, 3600]" % (2 * vp.LOSS_EDGE_S))
    times = vp.default_losses(profile, a.duration_s) if a.losses is None else vp.parse_losses(a.losses)
    losses = vp.plan_losses(times, a.duration_s, ppm_u)
    amp = vp.amplitude(a.level_dbfs)
    vp.pilot_block(0, 1, a.pilot_hz, amp)  # validates the frequency
    if a.print_packets < 0:
        raise ValueError("--print-packets must be >= 0")
    return profile, ppm, losses, amp


def _write_json(path, doc):
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(doc, fh, indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, path)


def _dry_run(a, profile, ppm, losses, summary):
    print("vban-emulator DRY RUN -- nothing is sent")
    print("profile=%s ppm=%+.3f duration_s=%.3f seed=%d stream=%s port=%d pilot_hz=%.3f level_dbfs=%.1f"
          % (profile.name, ppm, a.duration_s, a.seed, a.stream_name, a.port, a.pilot_hz, a.level_dbfs))
    print("summary: " + " ".join("%s=%s" % (k, _fmt(v)) for k, v in sorted(summary.items())))
    for lo in losses:
        print("loss: packet %d x%d (sample %d)" % (lo.packet, lo.count, lo.sample))
    shown = 0
    in_stall = False
    for p in vp.plan(profile, a.duration_s, a.seed, ppm, losses, a.start_frame):
        if p.stalled and not in_stall:
            print("stall: from packet %d (slot %.3f ms) released at %.3f ms"
                  % (p.index, p.deadline_ns / 1e6, p.release_ns / 1e6))
        in_stall = p.stalled
        if shown < a.print_packets:
            print("packet %d nuFrame=%d slot_ms=%.3f release_ms=%.3f%s%s"
                  % (p.index, p.nu_frame, p.deadline_ns / 1e6, p.release_ns / 1e6,
                     " LOST" if p.lost else "", " stalled" if p.stalled else ""))
            shown += 1


def _fmt(v):
    return "%.3f" % v if isinstance(v, float) else str(v)


class _Stop(Exception):
    pass


def _on_signal(signum, _frame):
    raise _Stop(signal.Signals(signum).name)


def _send(a, profile, ppm, losses, amp):
    """Send the run. Returns (stats, exit code)."""
    addr = socket.getaddrinfo(a.target, a.port, socket.AF_INET, socket.SOCK_DGRAM)[0][4]
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    stats = {"target": "%s:%d" % addr, "sent": 0, "lost": 0, "max_late_ms": 0.0,
             "late_over_%gms" % LATE_REPORT_MS: 0, "interrupted": None, "error": None,
             "started_epoch": time.time(), "ended_epoch": None}
    late_key = "late_over_%gms" % LATE_REPORT_MS
    rc = 0
    t0 = time.monotonic_ns()
    try:
        for p in vp.plan(profile, a.duration_s, a.seed, ppm, losses, a.start_frame):
            due = t0 + p.release_ns
            wait = due - time.monotonic_ns()
            if wait > 0:
                time.sleep(wait / 1e9)
            if p.lost:
                stats["lost"] += 1
                continue
            sock.sendto(vp.packet_bytes(a.stream_name, p, a.pilot_hz, amp), addr)
            stats["sent"] += 1
            late_ms = (time.monotonic_ns() - due) / 1e6
            stats["max_late_ms"] = max(stats["max_late_ms"], late_ms)
            if late_ms > LATE_REPORT_MS:
                stats[late_key] += 1
    except _Stop as e:
        stats["interrupted"] = str(e)
        rc = 5
    except OSError as e:
        stats["error"] = str(e)
        rc = 1
    finally:
        sock.close()
        stats["ended_epoch"] = time.time()
    return stats, rc


def main(argv=None):
    a = _args(argv)
    try:
        profile, ppm, losses, amp = _validated(a)
    except ValueError as e:
        print("vban-emulator: %s" % e, file=sys.stderr)
        return 3
    summary = vp.summarize(vp.plan(profile, a.duration_s, a.seed, ppm, losses, a.start_frame))
    if a.schedule_out:
        _write_json(a.schedule_out, vp.schedule_document(
            profile, ppm, a.duration_s, a.seed, losses, a.start_frame, a.stream_name, a.pilot_hz,
            a.level_dbfs, summary))
    if a.dry_run:
        _dry_run(a, profile, ppm, losses, summary)
        return 0
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, _on_signal)
    print("vban-emulator: sending %s to %s:%d (%d packets, %+.3f ppm, %d lost)"
          % (profile.name, a.target, a.port, summary["packets"], ppm, summary["lost"]), file=sys.stderr)
    stats, rc = _send(a, profile, ppm, losses, amp)
    if a.stats_out:
        _write_json(a.stats_out, stats)
    print("vban-emulator: " + " ".join("%s=%s" % (k, _fmt(v)) for k, v in sorted(stats.items())),
          file=sys.stderr)
    return rc


if __name__ == "__main__":
    sys.exit(main())
