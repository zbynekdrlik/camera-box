---
paths:
  - "scripts/clock-offset-guard.sh"
  - "scripts/dantesync-gate.sh"
  - "tests/clock_offset_guard.rs"
  - "tests/dantesync_gate.rs"
---

# DanteSync clock-offset gate (#8 precondition) — grading paths, slew lineage, test seams

`scripts/dantesync-gate.sh` (#7) is the recording-E2E precondition: every DanteSync node must be
NTP-in-bound AND PTP-locked. `scripts/clock-offset-guard.sh` holds its PURE, unit-tested functions
(sourced by `tests/clock_offset_guard.rs` via its `BASH_SOURCE != $0` guard; the gate's own e2e is
`tests/dantesync_gate.rs`).

## Two grading paths — know which one a CLIENT actually hits
`grade_http_node` samples each node via **HTTP `/status`** first (`gather_http_samples` → N reads →
`distinct_offset_samples_us` → `sampled_offset_verdict` grades the MEDIAN + spread). The
**journal** (`dantesync_offset_verdict` → `_fresh_offset_median_us`, `-o short-iso`) is only the
FALLBACK when HTTP is unreachable. On the live rig every cam serves `/status` on :8898, so a cam
CLIENT is graded by the HTTP median, not the journal — but the journal is ALSO fetched for a linux
client to read its adaptive step threshold and (since #1055) the slew step-correlation evidence.

## The master-slew / bimodal false-DRIFT lineage (#1021 → #1022 → #1041 → #1055)
When the NTP master (strih) exits its ~2.5 ms deadband and STEPS, every client observes a transient
+2.7–3.3 ms slew for ~30–60 s. A 30 s HTTP window landing in that plateau reads a majority-elevated
set → the MEDIAN drifts → false-fail of a µs-healthy fleet.
- **#1021** widens the MASTER row's own median bound from its `ntp_deadband_us`.
- **#1022/#1041** `client_chase_bound_us` widens a CLIENT row's median bound (`min(deadband,ceiling)
  + client_step_threshold + margin`) — but it is **derived from the MASTER's `/status`**
  (`master_chase_status`), a curl to the WINDOWS box. That read fails ~50 % during a live E2E, and
  when it comes back empty the client is graded on the bare 2000 µs bound → false-DRIFT.
  `chase_bimodal_exclusion_verdict` only ever rescues a SPREAD-side `unstable` (median-in-bound)
  verdict, NEVER a median-out-of-bound `drift`/`drift_unstable` (its own #1041 finding).
- **#1055** adds the master-INDEPENDENT rescue: `slew_transient_exclusion_verdict` reads the
  client's OWN journal, excludes `[NTP] offset:` samples within `--slew-step-window-s` (5 s) of a
  `[NTP] (Stepped|step candidate)` marker, and passes only when the **post-most-recent-correction**
  survivors (≥1, `--slew-min-surviving` for the window-sanity total) have a median in bound. Wired
  into `grade_http_node` as a linux-client rescue consulted ONLY when the raw verdict is
  drift/drift_unstable and chase_bimodal already said no (journal SSH paid only on a would-be-DRIFT
  node).

## Design lesson — "exclude transient samples then grade the rest" has an ONSET-drift hole
Grading the median of ALL step-excluded survivors over a wide (~5.5 min, K=11) window can be MASKED
at drift ONSET: a genuine desync that just started, stepping every cycle (all its samples excluded),
with pre-onset healthy baseline STILL in the window, reads a baseline survivor median while the
recent HTTP median correctly reads drift. The honest discriminator is RECENCY: grade only the
survivors NEWER than the newest correction marker — proof the clock RETURNED to and held µs-grade
AFTER its last correction. A transient returns to baseline (post-correction survivors µs → pass); an
onset desync leaves zero post-correction survivors, or still-elevated ones → fail. Any future
"exclude the transient, grade the baseline" gate must anchor on post-event recency, not the whole
window (found by adversarial review, #1055).

## Test-fixture seams + gotchas (`tests/dantesync_gate.rs`)
- `DANTESYNC_GATE_LINUX_JOURNAL_<NAME>` and `DANTESYNC_GATE_LINUX_HTTP_<NAME>` (NAME uppercased,
  `-`→`_`) inject offline fixtures. The **journal** var is a **FILE PATH** (`cat`'d by
  `read_linux_node_journal`) — passing the journal TEXT directly silently yields an EMPTY journal
  (cat of a huge string fails), so use `write_dante_journal(name, text)` and pass its path. The HTTP
  var may be a static file OR an **executable** (run every call) so a multi-read fixture returns a
  DIFFERENT payload per call — use `write_multi_read_fixture` (#836).
- `distinct_offset_samples_us` counts a read as a NEW sample only when its `updated_ts` differs from
  the last accepted one (dedup, #836). A multi-sample HTTP fixture MUST give each response a
  **distinct `updated_ts`** or it collapses to 1 distinct sample → `insufficient`, never the median
  you intended.
- `--ntp-master ""` opts OUT of the master concept (no client widening) — the faithful way to
  reproduce the "master `/status` unreadable" false-fail offline without a real curl to strih.
- Pure `slew_*`/`chase_bimodal_*`/`sampled_offset_*` functions are tested by SOURCING the script
  (`run_sourced`); a bash-fn RED test fails as `command not found` — that IS a valid RED.
- Rewriting `clock-offset-guard.sh` wholesale via `awk/sed > new && mv` DROPS the executable bit;
  the 4 CLI-subprocess tests then fail `PermissionDenied`. `chmod +x` after any such rewrite.
- `shellcheck -x dantesync-gate.sh` emits ~490 SC2317 (info) "unreachable" false-positives on the
  top-level assignments (the `BASH_SOURCE != $0` source-guard confuses its reachability analysis) —
  pre-existing, not real; filter them (`grep -v SC2317`) and there must be zero warning/error/style.

## PTP-lock parser is timestamp-grace-aware, not line-position (#864)
`ptp_locked_from_journal` no longer grades DEGRADED purely by LINE POSITION (last `[NTP] offset:`
newer than the last `[PTP] (NANO|LOCK) Drift:` servo line). That was a false-DEGRADED on a healthy
servo: NTP lines emit ~15 s, the servo ~30 s (live cam2 2026-08-14), so a LOCKED steady state
routinely has an NTP line as the window's last line with the next servo tick simply not due yet.
Now, when NTP is positionally newer, it grades by the `-o short-iso` TIMESTAMPS (both callers —
`verify-device.sh` (d) and `dantesync-gate.sh`'s journal fallback — gather them): DEGRADED only when
the NTP line trails the last servo line by MORE than grace = `max(measured_servo_cadence × 2, 75 s)`.
Cadence is self-calibrated (`_servo_cadence_s` = median inter-servo interval, robust to a dropped
tick; `_dante_line_iso_ts` extracts the ISO stamp) because the report cadence has changed once
already (issue 679). A journal with NO ISO timestamps (the older `Jun 22`-format unit fixtures) falls
back to the old position verdict — so every pre-existing test AND genuine servo-stopped detection
(lines cease while NTP continues → gap grows past grace → DEGRADED) are preserved. Env-tunable via
`PTP_LOCK_SERVO_GRACE_FACTOR`/`PTP_LOCK_SERVO_GRACE_FLOOR_S`.

**Local RED→GREEN for a bash change here does NOT need cargo.** `# airuleset:build-ok` is DISABLED
for camera-box (heavy builds are CI-only), so `cargo test` cannot run locally. But the Rust tests
are just `run_sourced` wrappers around bash — SOURCE `clock-offset-guard.sh` in a plain `bash -c`
and call the function on the fixture directly (that is exactly what the test does). This gives a
genuine local RED (against the pre-fix script) → GREEN proof with zero compilation, and is the
verification path to use for any pure bash function in this repo's sourced gates.
