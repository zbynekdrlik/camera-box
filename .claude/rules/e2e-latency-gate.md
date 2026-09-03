---
paths:
  - "src/e2e_latency_gate.rs"
  - "src/bin/recording-verdict.rs"
  - "src/probe/differ.rs"
  - "src/probe/analyzer.rs"
  - "src/bin/frame-probe.rs"
---

# Absolute latency / freeze BOUNDS live in TWO separate E2E subsystems — don't wire the wrong one

camera-box has **two independent E2E latency-gate mechanisms**. A ticket that cites one but asks
for the other's behavior (issue 1035 did exactly this) will send you to wire a gate that never runs
in the target harness.

- **MAIN E2E = `scripts/recording-e2e.sh` → `src/bin/recording-verdict.rs`** (recorded OBS-program
  files). Its per-hop latency lives in `probe::recording_latency` (`hop_latency`/`HopLatency`),
  emitted as `report["latency"]["cam_strih"]` / `full_chain.latency.*` / `all_cambox_latency` /
  `all_cambox_delivery_latency`. Its absolute cam→strih p99 BOUND is
  `src/e2e_latency_gate.rs` (issue 1035), folded into `overall_pass`.
- **LOOPBACK E2E = `scripts/loopback-e2e.sh` → `src/bin/frame-probe.rs`** (Phase-1/2 NDI-tap
  differencing). Its bounds are `differ::absolute_latency_gate_pass` / `analyzer::
  latency_freeze_gate_pass` (`--max-p99-latency-ms 350 --max-freeze-periods 6`).

**`differ::absolute_latency_gate_pass` / `overall_verdict` / `latency_freeze_gate_pass` are NEVER
called from `recording-verdict`.** In `recording-e2e.sh`, `frame-probe` runs ONLY as the cam2
`--paint-only` painter (no differ, no analyzer gate). So "wire a latency bound into the main E2E"
means adding a bound to the RECORDING verdict, not touching differ/frame-probe.

## The two latency traps in the recording path

1. **cam→stream / strih→stream ≈ 1000-1150 ms is BY DESIGN** — the intentional genlock hold that
   aligns program video to the ~1s-late mastered audio (the operator A/V-align domain). NEVER bound
   it tightly / never propose reducing it. The honest gate-able "absolute latency" is
   `latency.cam_strih` (cam2 paint gen_ts → strih program, BEFORE the hold): ~210-241 ms p99.
2. **Freeze in the recording path = `frozen_leg`, BLOCKING again as of issue 905 item 2**
   (`gates_overall_pass=true`; issue 914 had made it report-only while cam1's grabber, issue 909,
   tripped it — restored once a green E2E series proved it clean). Don't add a SECOND freeze gate
   here — the freeze bound is that existing seam.

## Calibrate any new recording-path bound from the green verdict JSONs (never guess)

Recent green runs' verdicts sit at `/tmp/recording-e2e-*/verdict-*.json` on dev1. Mine the real
distribution before setting a bound:
```bash
for V in $(ls /tmp/recording-e2e-*/verdict-*.json); do
  [ "$(jq -r '.overall_pass' "$V")" = "true" ] || continue
  jq -r '.latency.cam_strih.p99_ms' "$V"   # or any metric block
done | awk '{n++; if($1>mx)mx=$1} END{print "n="n" worst="mx}'
```
Set the bound with honest margin above the WORST green value (issue 1035: worst p99 240.7 → 400 ms
= 1.66x). A bound that would have failed a recent green run is a bug, not a gate.

## Mirror the crate-root `gates_overall_pass()` seam (Tier-0 testable, one-line-restorable)

`src/bin/recording-verdict.rs` is `required-features=["probe"]` — ZERO local compile path. Put the
PURE gate logic + calibrated constant + `gates_overall_pass()->bool` seam in a **crate-root** module
(`src/e2e_latency_gate.rs`, mirroring `src/optical_floor.rs` / `src/self_heal_attribution.rs`) so it
unit-tests Tier-0 (RED→GREEN locally). The verdict body only CALLS it: `all_pass &= pass ||
!gates_overall_pass()`. Relax/re-tighten is then a one-line flip. Make a necessary gate DEFAULT-ON
(CLI default = the constant, hard-locked) — not a forgettable flag a script must remember to pass.
