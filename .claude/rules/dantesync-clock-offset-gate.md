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

## Journal path grades MEDIAN + SPREAD too (#837, the twin of the #836 HTTP check)
`dantesync_offset_verdict JOURNAL FRESHNESS_S BOUND_US [STABILITY_US]` — the OPTIONAL 4th arg is the
journal-path mirror of `sampled_offset_verdict`'s spread/stability check. Omitted/empty = median-only,
byte-for-byte the pre-#837 contract (every 3-arg caller — `verify-device.sh` (d), painter gate — that
does NOT want the spread check stays unchanged). Present = grade the SPREAD (`_fresh_offset_spread_us`
→ `spread_of_ints`) of the SAME K=11 fresh set the median grades, adding `unstable`/`drift_unstable`
(same words the HTTP path uses). The raw fresh samples come from `_fresh_offset_samples_us` (ONE parse;
`_fresh_offset_median_us`/`_fresh_offset_spread_us` are thin wrappers — the #595 single-source rule).
Stability bound is `DANTESYNC_STABILITY_US` (default 2000), plumbed as `GATE_STABILITY_US` /
`DEVICE_CLOCK_STABILITY_US` / `IMAG_CLOCK_STABILITY_US` / the CLI `--stability-us`.

**The #1055 slew rescue is deliberately NOT on the journal path.** `slew_transient_exclusion_verdict`
rescues a would-be median-`drift`, so wiring it here would turn a node that `drift`s TODAY into `ok` =
FEWER failures than today = violates #837's "strictly more failures, never fewer" invariant. The HTTP
path landed the strict #836 check first and added the 1022/1041/1055 rescues LATER after live
false-fails; the journal path mirrors that sequencing — a journal slew rescue is a future ticket IF a
live false-`unstable` is ever observed on it, not a speculative add now.

## `run_sourced` test bodies run under `set -e` — guard any rc-returning call
`run_sourced` starts with `set -uo pipefail` and then `. clock-offset-guard.sh`, whose top-level
`set -euo pipefail` (line ~38) RE-ENABLES `-e` in the harness. So a test body that calls a function
returning non-zero (e.g. `offset_verdict_check`, which returns 2 on drift/unstable) aborts the whole
harness → `run_sourced`'s `out.status.success()` assert fails, masking the real assertion. Guard it the
way the existing `offset_check` tests do: `rc=0; the_fn … || rc=$?; echo "rc=$rc"`, then assert on the
captured `rc`. The pure verdict functions (`dantesync_offset_verdict`, `_fresh_offset_*`) all `printf`
a word and `return 0`, so `$(…)` captures of THEM need no guard — only the rc-signalling `*_check`
wrappers do.

## Grandmaster IDENTITY check is REPORT-FIRST and HTTP-path only (#834)
`grade_http_node` now parses `gm_source_ip` from the freshest `/status` payload and calls
`gm_check` (clock-offset-guard.sh) against `GATE_GRANDMASTER_IP` (`RIG_GRANDMASTER_IP`, default
10.77.9.184) — a node PTP-locked to a FOREIGN grandmaster reads `is_locked=true` while ~15 ms out,
which offset+PTP-lock alone cannot catch (the stream box on 10.77.7.109, live 2026-08-15). Two hard
constraints baked in:
- **REPORT-FIRST by default.** The `GM OK/FOREIGN/UNKNOWN` line ALWAYS prints, but only feeds the
  node verdict when `DANTESYNC_GATE_GM_ENFORCE=1` (default 0). In report-only mode `gm_gate_rc`
  stays 0, so `node_verdict` is byte-for-byte the pre-#834 offset+PTP grade. This is deliberate:
  enforcing while the stream box is genuinely on a foreign GM would fail `[0/8]` and brick every E2E
  run. Flipping to enforce is a SEPARATE ticket gated on the rig-side election fix (#1073), NOT a
  code cleanup — never enable enforce until every fleet node holds the rig GM. `node_verdict` gained
  an OPTIONAL 3rd rc arg (default 0) so every 2-arg caller is unchanged.
- **HTTP path only.** The check lives after the offset/PTP grade in `grade_http_node`, using the
  same `$status`. The linux **journal FALLBACK** path (empty `samples_raw`) returns BEFORE it and is
  never GM-checked — journald carries no `gm_source_ip` (verify-imag.sh:~977 documents the same),
  and strih/stream are always on the HTTP path anyway.
- **Offline test seam:** the existing `DANTESYNC_GATE_WIN_HTTP_<NAME>`/`..._LINUX_HTTP_<NAME>`
  fixtures already carry `gm_source_ip`; a foreign-GM test just sets it to a non-rig IP, and
  `DANTESYNC_GATE_GM_ENFORCE=1` (env) exercises the blocking path. A stream-only invocation still
  needs `--ntp-master ""` to pass the master-name guard (unrelated to GM).
