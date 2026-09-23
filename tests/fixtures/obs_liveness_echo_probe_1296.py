#!/usr/bin/env python3
"""#1296 test stub for obs-liveness-watchdog.sh's python probe: echo one
`HEALTHY <box>: stub box=<name>=<host>:<fps>` verdict line per --box arg, so the
sourced watchdog's VERDICT_LINES names exactly the boxes measure_boxes chose to
poll AND (issue 1317) the host/fps it chose for each, so a per-box arm's knobs
(STRIH_HOST / STRIH_TARGET_FPS on strih-lx) are observable."""
import sys

args = sys.argv[1:]
i = 0
while i < len(args):
    if args[i] == "--box":
        i += 1
        name = args[i].split("=", 1)[0]
        print(f"HEALTHY {name}: stub box={args[i]}")
    i += 1
