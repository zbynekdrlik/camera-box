---
paths:
  - "scripts/lib/frozen-cam-received.sh"
---

# `[4c/8]` frozen-camera gate leg-liveness = `received=` counter DELTA, NOT pixel hash (#1233)

The `[4c/8]` gate in `scripts/recording-e2e.sh` decides "is every strih camera input alive" BEFORE
committing a ~40-min run. Since #1233 the ABORT signal is strih's `genlock-fifo audit '<input>':
received=N` counter DELTA per input (`scripts/lib/frozen-cam-received.sh`), NOT a pixel hash of
preview screenshots.

## Why pixel-hash was wrong (the root cause, don't reintroduce it)

`frozen-camera-gate.py` sampled `GetSourceScreenshot` → SHA1; N identical hashes = FROZEN → abort.
That is CONTENT-dependent and reads FROZEN on a LIVE leg whenever the strih DistroAV receiver holds
the last frame — during the `[2b/8]` cambox deploy wave (a re-attaching receiver), or on a genuinely
static scene (one camera → splitter → every box repeats identical pixels). With 7 cameras the deploy
wave is long enough that the gate lands inside it and false-aborts (live run 33311702636 attempt 3:
FROZEN on 6/7 cams while all captured 60fps colour + the QR sweep decoded the live painter). `received=`
counts FIFO frame arrivals, so it advances whenever frames flow, immune to static content.

The pixel-hash check is KEPT as a report-only diagnostic line (`pixel-hash REPORT-ONLY: PASS/TIMEOUT/
FROZEN`) — it still warms each input onto PREVIEW (#747 side-effect) and its pixel-vs-received
disagreement is useful evidence — but it NEVER aborts. It is bounded by `FROZEN_CAM_PIXEL_REPORT_
TIMEOUT_S` (default 180s; a 124 timeout is labelled TIMEOUT, not FROZEN, so it never pollutes the
evidence).

## The lib REUSES the shared received= building blocks — never re-implement them

`scripts/lib/frozen-cam-received.sh` is the THIRD consumer of the same `received=` tap (after the
#1052 frozen-input watchdog and the #1093 mv-reverify escalation). It guard-sources and reuses:
- `mv_reverify_probe_raw` / `mv_reverify_extract_received` (`scripts/lib/mv-reverify-escalate.sh`) —
  the flat-ssh strih OBS-log tail read + newest per-source `received=` extract.
- `frozen_input_classify` (`scripts/lib/frozen-input-health.sh`) — the pure
  (prev,curr,expected_live,sender_reachable) → FROZEN|ADVANCING|UNKNOWN|SKIP decision.
It adds ONLY orchestration: `frozen_cam_received_classify_raw` (agg → ALIVE / FROZEN:<srcs> /
INCONCLUSIVE:<srcs> / READ_FAIL, precedence READ_FAIL>FROZEN>INCONCLUSIVE>ALIVE),
`frozen_cam_gate_should_abort` (PASS/ABORT/WARN_PASS), and the I/O wrapper
`frozen_cam_received_read_and_verdict`. Any further leg-liveness check should reuse these, not a
fourth copy.

## Byte-safety of the received= tap (#1258 layer 2)

Since this lib reuses `mv_reverify_extract_received` unchanged, it inherits the layer-2 byte-safety
fix for free — a PowerShell-side ANSI re-encode of a non-ASCII glyph in the fetched OBS-log tail can
otherwise make GNU grep flag stdin BINARY (empty extraction → every source reads "none" →
INCONCLUSIVE, exactly the 4/4-INCONCLUSIVE failure this gate exists to avoid producing false PASSes
for). Full root cause + fix: `.claude/rules/mv-reverify-escalate.md` “Layer 2” section. Never
re-add a plain (non-`LC_ALL=C`) grep/sed on the raw tail anywhere in this lib.

## The two load-bearing invariants

- **#797 — never divide an audit-counter delta by a wall-clock sleep.** Compare the raw counter
  VALUE across two reads (delta>0 = advancing, ==0 = frozen). The read GAP (`FROZEN_CAM_RECEIVED_
  GAP_S`, default 12s) MUST exceed the ~5.017s audit emit cadence or a live source reads the same
  newest audit line twice = false FROZEN. Mirrors `MV_REVERIFY_WEDGE_SAMPLE_GAP_S`.
- **Abort keys on a PROVEN freeze, and NEVER false-aborts on absence of evidence.** The `[4c/8]`
  loop tracks `frozen_proven` (set on any `FROZEN:*` attempt) and feeds `${frozen_proven:-$frozen_
  recv_verdict}` to `should_abort` — a proven freeze aborts even if the FINAL read glitches, but an
  all-INCONCLUSIVE/READ_FAIL run (no audit line / unreadable log — never a proven freeze) is a loud
  WARN, not an abort (the leg is re-proven downstream by the QR sweep; #365 abort intent preserved
  because a genuinely stuck leg never reaches ALIVE and stays FROZEN). Trade-off (accepted, ticket-
  mandated "abort keys on received= only"): a fully-DROPPED input with no audit line at all reads
  WARN not abort — but a wedged receiver still prints the line with a stuck counter → FROZEN, the
  #1158 self-heal restores a drifted mapping between attempts, and a larger tail
  (`FROZEN_CAM_RECEIVED_TAIL` default 800, set in a subshell so it never leaks into a later
  mv-reverify call) shrinks the stale-line scroll-out window.

## Tier-0 testing (no cargo — #557)

Pure fns are testable by direct sourcing over fixture raw-log text (`tests/harness_frozen_cam_
received_1233.rs` mirrors the pattern; a bash driver replicates each Rust body). Verify the I/O
wrapper under FULL `set -euo pipefail` (the caller invokes it inside a `$(...)` assignment in
recording-e2e.sh's strict-mode body) via a stateful fake reader on `FROZEN_CAM_RECEIVED_CMD` +
`FROZEN_CAM_RECEIVED_GAP_S=0` — grep-no-match extracts are `|| true`-guarded so no expected empty
read aborts the run.
