---
paths:
  - "scripts/arrival_floor_decompose.py"
  - "tests/python/test_arrival_floor_decompose_1168.py"
---

# Per-box arrival-floor stage decomposition (`scripts/arrival_floor_decompose.py`, issue 1168 task 1)

A dev1-side SUPERVISOR mining tool: run it over a FINISHED E2E run's collected logs to get one
per-camera table decomposing each camera's arrival floor into grabber / NDI-transport / strih stages,
so "which box/stage owns the cross-camera presented-age offset" is answered from data. Wired into NO
gate, drives NO rig. Tasks 2 (reduce the highest floor) and 3 (re-tighten `[4i/8align]`) are
downstream consumers of this tool's output.

## How to run

```
python3 scripts/arrival_floor_decompose.py --run-dir /tmp/recording-e2e-<RUN>        # text table
python3 scripts/arrival_floor_decompose.py --run-dir /tmp/recording-e2e-<RUN> --json # machine output
```

It auto-discovers the three stage artefacts by their standard names inside the run dir:
`qr-align-jitter-<RUN>.json`, `qr-align-strih-<RUN>.log`, `cam*-cbox-burn-<RUN>.log`. Override with
`--jitter-json` / `--strih-log`; restrict cameras with `--cameras "1,2,3"`; the strih source template
is `--source-template "NDI cam{n}"` (the current 1:1 rig NDI mapping).

## The model — algebraic, NOT fitted (do not "improve" it into a curve fit)

Each camera's arrival floor is EXACTLY `floor = latency_ms + mean_head_skew_ms` — the definition
`qr_align_pins.arrival_floors_from_jitter` already uses. So the per-camera EXCESS over the FASTEST
camera decomposes exactly:

```
excess = Δlatency_ms  (strih-config pin difference)  +  Δmean_head_skew_ms  (everything UPSTREAM of the pin)
```

`recv-timing #797` `cap_avg` is the NDI transport arrival CADENCE. When it is UNIFORM across cameras
(spread ≤ `TRANSPORT_UNIFORM_SPREAD_MS` = 3 ms) transport is NOT the per-camera differentiator, so the
upstream (Δskew) excess is attributed to the CAMBOX GRABBER — corroborated (never replaced) by the
cambox burn-log `Streaming:` emit-vs-capture parity + `#707 DEQUEUE STALL` health. A DQBUF stall is a
grabber-side JITTER signal (anti-correlated with real loss, issue 1198), so it corroborates a per-box
floor, it never proves loss.

## REUSE, never re-parse (the load-bearing constraint)

- Total floor + phantom-floor guard → `qr_align_pins.arrival_floors_from_jitter` (drops a samples<3
  floor, issue 1253). NEVER hand-derive `latency + skew` with a fresh regex.
- Transport cadence → `ndi_halving_decision.parse_recv_timing` (each line's OWN timestamp, the issue-797
  phantom-rate discipline — never a wall-clock divisor).
- Skew / FIFO pin → the harness's `qr-align-jitter-<RUN>.json` artefact (= `genlock-jitter-report
  --json`, i.e. `src/jitter_audit.rs`). The tool CONSUMES that JSON — it does not re-parse the
  `genlock-fifo audit` `ts_head_skew_ms` line with a new regex.
- Only the cambox `Streaming:`/`#707` grabber parsers are genuinely new (that signal isn't parsed
  elsewhere for this).

## Tier-0 (issue 557) — pure Python, full local RED→GREEN

Pure decision core (`decompose`, `parse_streaming`, `parse_dqbuf_stalls`, `cap_avg_by_source`,
`floors_and_fields`) with no I/O below the CLI, so `python3 -m pytest
tests/python/test_arrival_floor_decompose_1168.py` gives a complete local RED→GREEN with zero cargo —
the issue-1199/1203/1226 python-mirror precedent. The two real-data smoke tests run over a COMMITTED fixture
(`tests/fixtures/arrival_floor_1168/`), so they always run (test-strictness: no skips).

## Finding on the green series (as of 2026-09-01, verdicts 1363366080 / 1168855508)

cam1 consistently owns the highest per-box floor (~15–18 ms above the pack), attributed to the CAMBOX
GRABBER (transport cap_avg uniform ~0.1 ms; latency pin same 3 ms as the anchor). cam2's ~+3 ms is
PURELY its strih `latency_ms=6` pin — its grabber skew is actually the LOWEST, so cam2 must NOT be
blamed on its grabber. This mechanizes the hand-mined design finding; it is the data task 2 (reduce
cam1's grabber-side floor) starts from.
