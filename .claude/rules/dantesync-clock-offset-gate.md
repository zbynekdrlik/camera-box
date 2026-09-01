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

## A WINDOWS client's step envelope comes from /status, not a journal (#1129)
The #1022/#1041 median widening and the #1123 STABILITY (spread) widening both need the client's own
adaptive step threshold (the size of offset ITS OWN daemon tolerates before stepping). A **linux**
client reads it from its journal (`client_step_threshold_us_from_journal` tail-1 for the median,
`client_max_step_threshold_us_from_journal` window-max for the spread). A **windows** client has NO
journald, so pre-#1129 `grade_http_node` fell back to the fixed `GATE_CLIENT_STEP_THRESHOLD_FALLBACK_US`
(700us) → its stability bound stayed the base 2000us and a healthy ~3.4ms step-straddle spread
false-UNSTABLE'd the whole E2E (PR #1125 attempt 4, "client step threshold via fallback(700us)").
dantesync (#1129) now publishes the client's OWN currently-active adaptive step threshold in
`/status` as **`ntp_step_threshold_us`** — the SAME quantity (`calculate_ntp_adaptive_threshold()`)
a linux journal logs as `threshold:NNNus`, populated for a client too (server nodes report their
`server_step_threshold_us`). `grade_http_node`'s win branch reads the WINDOW-MAX across the sampled
payloads (`client_max_step_threshold_us_from_status_lines`) and feeds it as the client step term into
BOTH `client_chase_bound_us` (median) and `client_chase_stability_us` (spread) — the SAME step-aware
treatment cam2 gets from its journal. A box NOT yet serving the field (empty / null) keeps the 700us
fallback, always admitted in the gate note ("via fallback(700us)"); once serving it, the note reads
"via its own /status (Nus)". This REQUIRES the dantesync field deployed fleet-wide (a canary-first
fleet upgrade + a `dantesync-version-gate.sh` PIN bump AFTER the upgrade — release=deploy doctrine);
the gate change is graceful and merges safely before that (unchanged 700us behaviour on the current
fleet). `ntp_step_threshold_us` is `null` on a box in server mode's pre-lock state only where
`calculate_ntp_adaptive_threshold`/`server_step_threshold_us` cannot yet be computed — otherwise
`Some(..)`.

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

## phase_slew ENABLED check + the cam-box provisioning gap it exposed (#1215)

`phase_slew_enabled_from_pipe_json`/`phase_slew_check` were added to `clock-offset-guard.sh`
mirroring `gm_source_ip_from_pipe_json`/`gm_check` byte-for-byte: read a boolean field off the
SAME `:8898/status` blob `grade_http_node`/`verify-imag.sh` check (l) already fetch, map to
0 ENABLED / 2 DISABLED / 3 UNKNOWN. When adding a new field-read off an already-fetched status
blob (this is the third one now, after `is_locked`/`ntp_offset_us` and `gm_source_ip`), copy the
EXISTING sibling pair's shape rather than inventing a new one — same `|| true` no-match survival,
same `case`-based check function, same "unreadable is never OK" contract.

**Finding while investigating this ticket's scope: `scripts/setup-device.sh` (the cam1-4
provisioning script) does NOT write `/etc/dantesync/config.json` at all** —
`grep -rn "phase_slew\|/etc/dantesync" scripts/setup-device.sh` returns nothing. The cam-box
fleet's `phase_slew.enabled=true`/`gm_allowlist` config came from an out-of-band hand/canary
rollout (dantesync issue 97), never from this repo's own provisioning. So there is currently
**zero shared code** between imag-nb's config write (`setup-imag.sh` step 3, #1215) and the cam
boxes for this specific file — a future ticket that wants `setup-device.sh` to provision the same
config (closing the identical gap on the cam-box side) starts from scratch, reusing only the JSON
body shape and the `RIG_GRANDMASTER_IP` env-override name (already shared with `verify-imag.sh`/
`dantesync-gate.sh`) — don't assume a shared helper already exists just because the JSON is
identical on paper.

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

## `DANTESYNC_GATE_GM_ENFORCE` — grandmaster IDENTITY enforcement (LIVE since #1073)

`gm_check` (`clock-offset-guard.sh:658`, rc 0 OK / 2 FOREIGN / 3 UNKNOWN) checks every HTTP-graded
node's `gm_source_ip` against `GATE_GRANDMASTER_IP` (`RIG_GRANDMASTER_IP`, default `10.77.9.184`).
It ALWAYS prints its `GM OK/FOREIGN/UNKNOWN` line, but its rc only feeds `node_verdict` when
`GATE_GM_ENFORCE=1` (`dantesync-gate.sh:600`). Env: `DANTESYNC_GATE_GM_ENFORCE`, default `0`
(report-first, `dantesync-gate.sh:169`); `!=0 && !=1` is a hard config error (`:920`).

- **What it gates: IDENTITY only, never OFFSET.** A node PTP-locked to a FOREIGN grandmaster (the
  stream-on-`10.77.7.109` false-green issue 834) passes the offset+PTP grade while being on a
  different timebase — `gm_check` is the only term that catches it. The ~23ppm GM-frequency
  step-storm is a SEPARATE offset-gate concern (issue 1108), untouched by this flag.
- **Flipped LIVE at BOTH `recording-e2e.sh` invocations** (main `[0/8]` gate ~line 705 grading
  cam1/cam2/strih/stream; `#947` secondary ~line 1071 grading strih) via an env PREFIX
  `DANTESYNC_GATE_GM_ENFORCE=1 "$HERE/dantesync-gate.sh"`. The gate's own DEFAULT stays `0`, so
  standalone/dry-run callers and `verify-imag.sh` (which enforces via its OWN direct
  `gm_check`→`fail`, ~line 1087) are unaffected — do NOT change the gate default to enforce.
- **Only HTTP-graded nodes are gm_checked.** The journal FALLBACK path returns BEFORE `gm_check`
  (journald carries no `gm_source_ip`); the grandmaster device itself is never a graded node. On
  enforce: FOREIGN→exit 20, UNKNOWN (unread `gm_source_ip`)→exit 11.
- **Verify-before-flip (never flip a gate red):** the enforced condition = every HTTP-graded node
  reports `gm_source_ip=RIG_GRANDMASTER_IP`. Prove it live by SOURCING `clock-offset-guard.sh` and
  running `gm_check <node> "$(gm_source_ip_from_pipe_json "$(curl -fsS http://<ip>:8898/status)")"
  10.77.9.184` on each graded node — all must be rc=0. Never enforce while any graded node is on a
  foreign/unreadable GM (that is exactly why #834 shipped report-first).
- **TDD an env-var flip on a subprocess call by asserting the ENV VALUE, not argv:** run the REAL
  script region against a fake `dantesync-gate.sh` that logs `${DANTESYNC_GATE_GM_ENFORCE:-UNSET}`;
  `env_remove` it in the harness for a genuine RED; assert `ENFORCE=1`. A static "text present"
  check cannot prove the prefix is on the right line or well-formed. See
  `tests/harness_recording_e2e_gm_enforce_1073.rs`.
## `ntp_deadband_us` is the NO-STEP threshold, NOT the per-step CAP — master bound floors at the step-cap (#1119)
Two DIFFERENT quantities, easy to conflate: `ntp_deadband_us` (live v1.8.46: **1000us**) is the
threshold below which the master does not step; the **≤2500us bounded PER-STEP cap** (dantesync
v1.8.46 design) is what the master's own `ntp_offset_us` actually sawtooths TOWARD under a slow
grandmaster (~23ppm). The step-cap is **NOT exposed over `/status`**. So the #1021 deadband widening
(`ntp_master_effective_bound_us` = max(2000, deadband+margin) = max(2000, 1000+1000) = 2000) produces
**no widening** on v1.8.46, and a healthy sawtooth median (failed run: 2699us) false-DRIFTs the bare
2000us bound — a per-window coin flip on the NTP master ALONE. Fix (#1119): the master's median bound
also floors at a **gate-side** step-cap constant `GATE_NTP_MASTER_STEP_CAP_US` (default 2500,
`DANTESYNC_NTP_MASTER_STEP_CAP_US`), i.e. `ntp_master_effective_bound_us STATUS BOUND MARGIN
[STEP_CAP_US]` → max(bound, deadband+margin, step_cap+margin) = 3500us. **Gated on a numeric
`ntp_deadband_us` being present** (the dantesync-#84+ bounded-step regime marker) so a pre-#84 master
keeps the bare bound (preserves the #1021 no/null-deadband tests); the step-cap term only bites when
the reported deadband is SMALLER than the step-cap — exactly the v1.8.46 regime. The optional 4th arg
defaults "0" → byte-identical pre-#1119 for every 3-arg caller/unit-test.

Because the master's raw UTC median is thus no longer a health signal up to the step-cap, the master
is ALSO hard-failed on dantesync's OWN `ntp_step_storm:true` (its >120-steps/hour thrashing alarm) via
`ntp_master_step_storm_verdict` — a HARD fail regardless of median, in the median-only branch, only on
a freshly-graded payload (`rc_off != 3`), false/null/absent never fails (report-first). `ntp_step_storm`
and `ntp_steps_last_hour` are **MASTER-only fields** (null on clients, exactly like `ntp_deadband_us`).
The #1055 slew rescue never applied here: it is a LINUX-CLIENT journal rescue; strih is a `--win-http`
node with no journal, so the master's only widening path is `ntp_master_effective_bound_us`. Genlock
FIFO pacing is monotonic-clock-based, so this UTC sawtooth is harmless to the recording path — only the
gate's median term coin-flips. Long-term GM-frequency fix is the owner's Dante-device court + dantesync
issue 95. Local RED→GREEN needs no cargo: run `scripts/dantesync-gate.sh` directly with a fresh
`DANTESYNC_GATE_WIN_HTTP_STRIH` executable fixture (fresh `updated_ts` — the 300s freshness window
ages a stale fixture into NTP STALE).

## The client STABILITY (spread) term needs the SAME step-awareness as the median — via the WINDOW-MAX threshold (#1123)
Sibling of #1119, but on the CLIENT spread side. The #1022/#1041 client MEDIAN widening
(`client_chase_bound_us`) is step-aware, but the STABILITY (spread) bound stayed fixed at
`GATE_STABILITY_US` (2000us). A client chases the master's by-design UTC sawtooth with its OWN
bounded steps; a step landing mid-sample-window makes the 6 samples straddle it, so the SPREAD ≈ the
client's step MAGNITUDE (live cam1 2026-08-19: 2938us, == its own `[NTP] Stepped +2938us`) → false
`UNSTABLE (median 1924us <= 2775us bound; spread 2938us > 2000us stability)` while PTP LOCKED + GM OK.

**Key subtlety: the spread exceeds even the WIDENED median bound** (2775us here) — because the median
widening reads `client_step_threshold_us_from_journal` = the **tail-1** (freshest) adaptive threshold
(775us at grade time), while cam1's adaptive `threshold` jittered up to **6860us** in the same window.
The median is a point estimate → tail-1; the SPREAD is a WINDOW-WIDE range → the **window-MAX**
threshold. Fix: NEW pure `client_max_step_threshold_us_from_journal` (max of every `threshold:Nus`
via `sort -n | tail -1`) + `client_chase_stability_us STABILITY MARGIN JOURNAL [FALLBACK]` =
max(STABILITY, max_threshold+margin); wired in `grade_http_node`'s client branch right after the
median widening, reusing the already-read `$client_journal` (no extra SSH). A spread beyond the
client's own journal envelope (or no readable journal) still FAILS; a gross desync fails on MEDIAN
(drift), never reaching the spread question. No master-deadband term for the spread — the client's own
adaptive threshold already bakes in the master excursion it chases.

**Why NOT a post-step-cluster recency rescue (the #1055 lineage):** cam1's failure is a RISING-EDGE
straddle — the offset is ramping toward a step NOT YET made at grade time, so there are no post-step
survivors in the window to grade. A recency-anchored "grade the tight post-step cluster" rescue
structurally cannot catch a pre-step ramp. Local RED→GREEN with NO cargo: `rustc --edition 2021 --test
tests/<file>.rs` standalone (provide `CARGO_MANIFEST_DIR=<worktree>`; the harness tests only use std +
shell out), then run the binary; OR run the pure bash fn under `bash -c 'set -uo pipefail; . scripts/
clock-offset-guard.sh; ...'` (`cargo test --no-run` is now hook-blocked too, #477 tightening).

## phase_slew is now ENFORCED at the fleet [0/8] gate (#1130 — report-first landed, then flipped)

The #1215 section above added `phase_slew_check`/`phase_slew_enabled_from_pipe_json` but wired
them into `verify-imag.sh` ONLY (imag box). #1130 wired them into `dantesync-gate.sh`'s
`grade_http_node` too, so EVERY HTTP-graded fleet node's phase_slew state is checked on every
recording-E2E `[0/8]` run — because phase_slew (dantesync issue 97) is the fleet-wide CURE for the
chronic NTP step storm, and a box silently reverting to `phase_slew=off` would re-introduce it
uncaught until dantesync's own >120/h `ntp_step_storm` alarm (far above the visible-judder
threshold). Mirrors the #834 `gm_check` sibling byte-for-byte:

- **`GATE_PHASE_SLEW_ENFORCE` (env `DANTESYNC_GATE_PHASE_SLEW_ENFORCE`) is the gate's own default-0
  report-first flag**, unchanged by the flip below: at 0 the `PHASE-SLEW ENABLED/DISABLED/UNKNOWN`
  line ALWAYS prints per node but `ps_gate_rc` stays 0 → verdict byte-identical to pre-#1130. At 1:
  DISABLED → BAD/20, UNKNOWN (field absent/unread) → INCOMPLETE/11. Validated must-be-0/1 (same
  loud-on-typo guard as GM). Standalone/dry-run callers and `verify-imag.sh` (its own direct
  phase_slew check) still see the default-0 report-first behaviour — only `recording-e2e.sh`'s two
  invocations flip it, exactly like the GM precedent.
- **`node_verdict` gained an OPTIONAL 4th `[PS_RC]` arg (default 0)** — every 2-/3-arg caller
  unchanged. HTTP-path only (journal fallback returns before the block; journald has no
  `phase_slew_enabled`).
- **The enforce flip LANDED 2026-09-02** (a one-env-prefix change at BOTH `recording-e2e.sh`
  `dantesync-gate.sh` invocations — the main `[0/8]` gate ~line 819 and the secondary `#947` sanity
  gate ~line 1245, exactly like GM's #1073 flipped at both call sites), after re-verifying LIVE that
  EVERY graded node — all seven active cameras (cam5/6/7 included, extending the ticket's own
  earlier cam1-4+strih+stream-only check), strih, and stream — serves `phase_slew_enabled=true`.
  `DANTESYNC_GATE_PHASE_SLEW_ENFORCE=1` now sits alongside the pre-existing
  `DANTESYNC_GATE_GM_ENFORCE=1` prefix on both invocations: a box that silently reverts to
  `phase_slew=off` now hard-fails the `[0/8]` gate (DISABLED→20, UNKNOWN→11) instead of only being
  reported. Verify-before-any-FUTURE-relax the same way: source `clock-offset-guard.sh` +
  `phase_slew_check <node> "$(phase_slew_enabled_from_pipe_json "$(curl -fsS
  http://<ip>:8898/status)")"` on each graded node.

**Do NOT walk back the #1119/#1123/#1055 widenings just because the step storm is gone (#1130).**
As of 2026-09-01 the fleet is on 1.8.52 with phase_slew ENABLED everywhere, master
`ntp_steps_last_hour=0`, offsets µs-grade — so those widenings/rescues are DORMANT (zero steps) and
harmless (they never gate video). Removing them is a BLIND tightening on single-point evidence
during a FRESH phase_slew deployment: the owner wants a quiet-window fleet step-census first
(issue 1130 comment 2026-08-31), and "never re-relax / recalibrate only from post-fix distribution
data" applies in the tightening direction too. The recording-verdict floor/fold re-tighten is a
SEPARATE lane (issue 1242, OPEN), never this gate.
