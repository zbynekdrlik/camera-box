---
paths:
  - "src/lipsync_cross_check.rs"
  - "scripts/lipsync-cross-check.sh"
  - "scripts/lipsync-test-mode.sh"
  - "scripts/lipsync-asset.sh"
---

# Lipsync cross-check fold — the evidence lives in a MANUAL paired-run campaign, not the E2E verdicts (issue 1032)

`src/lipsync_cross_check.rs` cross-validates the QR/QPSK A/V-sync calibration against a NEURAL
SyncNet lipsync offset. It follows the standard `gates_overall_pass()` seam
(`verdict-gate-seam-calibration.md`) but has ONE trap that makes DATA-FIRST calibration different
from every other gate in this repo.

## The cross-check is NOT part of the standard E2E verdict — mining verdict JSONs finds ZERO

`optical_floor` / `e2e_latency_gate` / `presentation_cadence` etc. are all computed inside
`recording-verdict`'s main verdict pass, so `/tmp/recording-e2e-*/verdict-*.json` carry their
distributions and you calibrate from those green runs. **The lipsync cross-check does NOT.** It runs
ONLY in `recording-verdict --av-sync --syncnet-offset-ms <X>` mode — a SEPARATE CLI mode invoked
solely by the manual `scripts/lipsync-cross-check.sh` orchestration, on a PAIRED (lipsync-test-mode
recording vs QR/QPSK TEST-mode recording of the SAME rig state). `scripts/recording-e2e.sh` never
passes `--syncnet-offset-ms`. Consequence, confirmed 2026-08-17: **0 of 144 verdict-*.json carry any
`lipsync`/`syncnet`/`cross_check` key**, and the harness writes its `lipsync-syncnet-agg.json` /
`lipsync-calibration.jsonl` into a `mktemp -d` workdir it then `rm -rf`s — so NO paired-run evidence
persists anywhere on the box. Do NOT conclude "no evidence ⇒ the check is broken"; it just means the
evidence source is the manual paired-run campaign, which is SUPERVISOR/rig-ops scope
(`lipsync-cross-check.sh` header, "exercised end-to-end only by the supervisor on the real rig").

## The fold is COUNTER-derived — flip ONE constant, not a bare bool (issue 1032)

Unlike the plain-bool seams, `gates_overall_pass()` here is derived:
`fold_is_earned(RECORDED_CLEAN_PAIRED_RUNS)` = `RECORDED_CLEAN_PAIRED_RUNS >= REQUIRED_CLEAN_PAIRED_RUNS`
(REQUIRED = 5, the ticket's "N (>=5)"). Report-only today because `RECORDED_CLEAN_PAIRED_RUNS = 0`
(no evidence). To make the fold LIVE: bump `RECORDED_CLEAN_PAIRED_RUNS` to 5 (the ONE constant) as
you record the real N-run evidence set, linking each clean paired-run verdict on issue 1032 — NEVER
on a guess (`verdict-gate-seam-calibration.md` gates-green-first). No consumer edit is needed: the
consumer (`run_av_sync`) already calls `folds_to_failure(verdict, RECORDED_CLEAN_PAIRED_RUNS)` and
bails on a Disagree once earned.

## Pass semantics (the fold's decision)

`lipsync_cross_check_gate_pass`: only `Disagree` FAILS. `Agree` passes; `Unknown` also PASSES — a
paired run where one side couldn't be measured proves nothing about disagreement (no double-jeopardy),
and the real consumer path always supplies both offsets so `Unknown` never arises there anyway. The
fold happens in `run_av_sync` (an `anyhow::bail!` after the JSON is printed), NOT in
`build_and_print_verdict`'s `all_pass` accumulator — because the cross-check is a per-`--av-sync`-run
measurement, not a per-recording verdict term.

## A SyncNet conf ~1.0-on-every-chunk regression with a tracked face + clean audio = emit-CADENCE warp, not a lag (issue 1174)

When SyncNet reads conf ~1.0 on EVERY chunk while the face is fully tracked (S3FD ~419/419) and the
audio is present + clean (direct cam2-monitor-out → Dante cable, no acoustics), and an offset sweep
±2.2 s never lifts it — that is NOT an A/V lag (no single offset restores a NON-uniform time warp).
It is the VIDEO timeline being warped by the camera-box emit path. The prime suspect is
`src/dupe_decimation/gate.rs`: its `dupe_shed_summary` line (logged ~every 5 s to cam2's journal)
carries 7 counters that split by MOTION effect —
- **PRESERVING** (smooth motion; the Aug-5 baseline had only these): `dupe-victim shed`,
  `blind-pacing shed` (uniform decimation / true-dup drop).
- **WARPING** (freeze/jump; ALL added AFTER Aug-5 — #1111/#1145/#1167): `late-dupe copies emitted`,
  `boundaries retired`, `depth-drained`, `fast-drained`, `starvation last-frame repeats`.
Aug-5 predates the WHOLE dupe-decimation era, so ANY nonzero WARPING count during the lipsync window
is cadence damage that decorrelates lips from the continuous audio → conf collapses.

**Confirm it in ONE command** (`lipsync-cross-check.sh` gained a code-side diagnostic, issue 1174):
capture cam2's window (`journalctl -u camera-box --since … --until … | grep 'dupe-preferring
decimation' > cadence.log`) and pass `--lipsync-cadence-log cadence.log` to the SAME paired-run
cross-check command. It prints `lipsync_cadence: verdict=CADENCE-WARP|CADENCE-CLEAN|CADENCE-UNKNOWN
…`: WARP (warp_events>0) = emit path confirmed contributing; CLEAN (=0, ≥1 line) = emit exonerated,
look at moiré/exposure on the video branch; UNKNOWN (no lines) = re-capture, NEVER read as "no warp".
The pure parser `lipsync_cadence_attribution` (awk-only, always exit 0 — report-only #1133 discipline)
is unit-tested in `tests/harness_lipsync_cross_check.rs`. The classifier is grounded in code HISTORY
(those event types didn't exist on Aug-5), not a tuned threshold. The actual FIX once WARP is
confirmed is a `needs-user-decision` tradeoff (the valves keep the burn-id grid locked for the burn
gate) — do NOT change the production emit path blind.
