---
paths:
  - "src/imag_leg_gate.rs"
  - "scripts/lib/imag-leg-marker.sh"
  - "src/bin/recording-verdict.rs"
---

# imag leg recording verdict — report-only seam + its TWO gating paths (issue 798)

## #1142 (2026-08-19) — the imag leg is now SPLIT: presence BLOCKS, per-frame content REPORT-ONLY

The single `gates_overall_pass()` "flip ONE fn" model below is SUPERSEDED. #1142 (owner mandate)
proved via issue 1130 (comment 5347311707) that the imag ~19.5% `imag_optical_stuck` is an x264
record-load OBSERVER EFFECT (the E2E recording starves the imag iGPU past its 16.7ms budget → OBS
repeats whole renders during the record window only; "churn, not loss"). So the imag leg was SPLIT:

- **`imag_leg_gate::gates_overall_pass()` -> `true` (PRESENCE/VERIFICATION, BLOCKING):** folds
  `imag_leg_verified` (a silently-skipped or schema-degraded leg REDs — the ONE sanctioned skip is an
  operator-offline-acked imag, #1013, exempted via `--offline-ack-cams`), the analyzed-span floor,
  the cam2 undecodable moiré floor, and colour. Two fold sites in the WHOLE-recording node block
  (`imag_presence_ok`) + the `imag_leg_verified` fold (outside the `if let Some(imag_frames)` block).
- **`imag_leg_gate::content_gates_overall_pass()` -> `false` (PER-FRAME CONTENT, REPORT-ONLY):**
  folds the digital-burn contiguity + optical-beat (`imag_content_ok`) AND the per-segment
  `all_cambox_continuity.imag` sweep. Confounded by the observer effect → pending the issue 1143 imag
  encoder fix (`TODO(#1143)` in `src/imag_leg_gate.rs`), then flip `content_gates_overall_pass` true.
- **`partial_schema_gate::box_is_report_only` was RENAMED `box_degrades_on_schema_mismatch`** and
  DECOUPLED from the gate flip (unconditionally `true` for imag): a schema-mismatched imag partial
  still DEGRADES (drop the leg + write a verdict from strih+stream) instead of hard-dying, but the
  dropped leg now REDs via `imag_leg_verified` (owner mandate: "degrade smie ostať degrade, ale musí
  RED-ovať"). Do NOT re-couple it to `gates_overall_pass` (that would make a stale-emitter imag
  partial a fatal no-verdict crash again).
- Verdict JSON now carries: `full_chain.loss.imag.{imag_presence_pass,imag_content_pass,
  gates_overall_pass,content_gates_overall_pass}` + `full_chain.{imag_leg_verified_offline_acked,
  imag_leg_verified_gates_overall_pass}`; the per-segment block's `gates_overall_pass` reads the
  CONTENT seam. `e2e_discord_report.py`: imag PRESENCE → `_blocking_failures`, imag CONTENT →
  `_report_only_tripped`.

The rest of this file (below) is the pre-#1142 issue-798 history — still accurate for the CONTENT
seam's report-only rationale, but the "flip ONE fn to make BOTH blocking" instruction is retired.

---

The imag leg's frame-by-frame recording verdict is REPORT-ONLY today via
`camera_box::imag_leg_gate::gates_overall_pass()` (returns `false`), mirroring
`optical_floor` / `e2e_latency_gate` / `burn_hold`. Two non-obvious facts a future change MUST know:

## The imag leg gates `all_pass` in TWO independent places — flip BOTH via the ONE seam

In `recording-verdict.rs::build_and_print_verdict` the imag leg folds into `all_pass` TWICE:

1. **Whole-recording node fold** — `all_pass &= imag_leg_gate::folds_into_overall_pass(nv.is_zero() && span_ok)` inside `if let Some(imag_frames) = &imag_frames_opt` (`node_verdict_for_imag`: optical tick contiguity + `imag_burn_ok` digital-burn contiguity + optical-beat freeze/copy). Surfaced at `full_chain.loss.imag`.
2. **All-cambox per-segment sweep** — `all_pass &= imag_leg_gate::folds_into_overall_pass(imag_overall_pass)` in the `--switch-schedule` sweep. Surfaced at `all_cambox_continuity.imag`.

Both route through the SAME `imag_leg_gate::folds_into_overall_pass()`, so the issue-798 follow-up
flips ONE function (`gates_overall_pass()` → `true`) to make the imag leg blocking again — do NOT
hunt for two separate toggles. The per-cambox (stream) sweep's own `all_pass &= seg.overall_pass`
fold is a DIFFERENT term and stays blocking — never touch it when changing imag gating.

## The imag verdict only FLOWS when recording-e2e.sh `[8/8c]` succeeds — check `imag_leg_verified`

Both imag blocks are `if let Some(imag_frames)`-guarded: they run ONLY when the merge got
`--merge-partials imag=<json>`. Historically that happened in 0 of 76+ runs — `[8/8c]` degrades
gracefully on any imag-side StopRecord / reachability / decode failure and `[8/8d]` silently omits
the flag. Signals that tell you whether the imag leg was actually verified this run:

- `full_chain.imag_leg_verified` (bool, verdict JSON) — the durable, mineable answer to "did the
  imag leg actually run?". Mine it before assuming a green run proved imag.
- The `IMAG-LEG-VERIFIED` / `IMAG-LEG-NOT-VERIFIED` run-log marker (`scripts/lib/imag-leg-marker.sh`,
  emitted at `[8/8c]`) — names the skip REASON (no-recording-path vs extract-failed).

## Before flipping it blocking (the follow-up)

Per `verdict-gate-seam-calibration.md`: you need a GREEN imag-verdict distribution to calibrate
against, and there were ZERO imag runs at report-only-land time. The follow-up must (1) confirm the
rig-side extract is healthy so imag partials flow (a live E2E — supervisor/rig-ops scope), (2)
accumulate green imag runs, (3) flip `gates_overall_pass()` to `true`, and (4) fold in the issue-887
produced-vs-presented ~7% deficit as a blocking term. Do NOT flip it blind.

### 2026-09-01 data-first status (issue 1094 STEP-0) — observer effect GONE, but the CONTENT terms are still red for a DIFFERENT reason; do NOT read `imag_burn_ok=False` as real loss

Mined the 3 green 7-cam verdicts of the first stable series (674135238 / 1363366080 / 1168855508).
The issue-1143 VAAPI encoder fix is confirmed live IN the E2E runs: `record_render_lagged_pct` is
now 0.0–0.3% (was ~18.4% under x264) and `imag_optical_stuck_density` fell to 0.004–0.045 (was
~0.195). So the #1130 observer effect is genuinely gone. BUT `content_gates_overall_pass` still
`false` and `imag_content_pass` still `False` in all 3 — because **`imag_burn_ok` is `False` in
every run (~50–53% of burn IDs "missing"), and that is a burn-CADENCE / metric-interpretation
mismatch on the swapped imag box, NOT frame loss and NOT the observer effect.** On the new box the
imag burn counter steps ~3 IDs/frame (e.g. span 843742→901990 = 58248 over 19513 expected ≈
2.985/frame) and the "missing" IDs follow a clean stride-6-dominant pattern (`imag_burn_present_ok`
true, `burn_unreadable` 0) — random loss would not. The optical-beat term is also not stably green
(`imag_optical_beat_pass` True in only 1/3; `imag_optical_beat_net_zero` False in all 3).

Consequence for whoever picks up the flip: the healthy CONTENT baseline for the new box has never
been characterized, so the raw `imag_burn_ok`/beat booleans are not yet meaningful as a gate.
Flipping `content_gates_overall_pass()` (or folding the #887 `hdmi1_repeated_frames` deficit)
blocking today would RED every currently-green run. The remaining preconditions live in issue 1144
(OPEN) — (2) characterize the healthy burn/beat baseline from a clean post-fix run, and (3) a
deliberately-misconfigured-camera (1/60-shutter) discrimination run, which does not exist yet — and
both need LIVE rig E2E runs (supervisor/rig-ops scope), never a blind flip off today's red content
terms. Recalibrating the burn-contiguity metric to the new-box ~3/frame cadence is itself part of
(2), not a threshold to widen so the red simply passes.

## The 0/76 root cause was the decode CPU-PIN, not StopRecord/reachability/decode (issue 1094, FIXED)

The `[8/8c]` extract ran `recording-verdict-on-imag.sh`, whose `build_onimag_command` hardcoded
`nice -n 19 taskset -c 12-15 …`. That range was the E-cores of the RETIRED 16-thread imag notebook.
The box was swapped to an **i5-13420H (12 threads, online cpus 0-11, E-cores 8-11)** — cores 12-15
do not exist, so `taskset -c 12-15` exits **rc=1 "Invalid argument" BEFORE it can exec
recording-verdict**. No partial was ever produced → `[8/8c]` "extract failed" → `[8/8d]` omitted
`--merge-partials imag=…` → `imag_leg_verified=false` on EVERY run. The setup-imag.sh CPU-isolation
plan was already made topology-agnostic for this same swap (`imag_cpu_isolation_plan`, #833/#841),
but the DECODE pin in recording-verdict-on-imag.sh was a SEPARATE hardcoded core reference the swap
sweep missed.

Fixed (1094): `onimag_decode_core_range <ncpus>` (pure, Tier-0-tested) → top min(4,ncpus) cores
(8-11 here, 12-15 on the old box); `main()` resolves it from the LIVE box (nproc over ssh + a
`taskset` probe) and FAILS OPEN (empty range → decode runs unpinned, nice 19 only), so a pin error
can never again silently zero the imag leg. The stale on-box binary is NOT a factor — it supports
`--extract-partial imag` and emits `schema_version=3`, exactly what the current merge expects.

Diagnostic tells: an imag extract that fails with **no decode-progress log line at all** (no
`recording decode progress frames_read=…`) points at the PREFIX (`taskset`/`nice`), not the binary
or the recording — a failing `taskset` aborts the whole command before recording-verdict starts.
And when a box is swapped, sweep EVERY hardcoded core reference (`grep -rn 'taskset -c'`), not just
the isolation plan.

## The stale-on-imag-binary landmine FIRED on the v3->v4 bump — FIXED both sides (issue 1118)

The latent landmine below fired on its first possible run (E2E 32178766136): the dup-cadence work
bumped `PARTIAL_SCHEMA_VERSION` 3->4, and because `recording-verdict-on-imag.sh` STEP 1 re-uploaded
the on-imag binary only when it was absent/non-executable (`[ -x ]`, no version compare), imag kept
its stale v3-emitting binary. The fresh dev1 merge then HARD-DIED — `RecordingPartial::load(path)?`
propagated `recording partial schema_version 3 is not supported` before any verdict JSON was written,
so the fail-closed E2E guard reds a run a strih+stream-only merge scores `overall_pass=true`. Two
fixes landed (both crate-root-pure so they Tier-0-test despite the probe gate on recording-verdict):

1. **STEP 1 version-gate** (`onimag_upload_decision`, pure/sourced): STEP 1 now sha256-compares the
   on-imag binary against the local one and re-uploads on a mismatch (identical sha keeps the fast
   skip; `--force-upload` still wins; any unreadable sha re-uploads fail-safe). A schema bump can
   never again leave imag on a stale emitter.
2. **Merge-side degrade** (`src/partial_schema_gate.rs` seam, called by `run_merge`): a schema-
   mismatched partial for the REPORT-ONLY imag box now DEGRADES — drop it, loud stderr WARNING,
   `full_chain.imag_leg_verified=false` + `full_chain.imag_leg_skip_reason`, verdict computed from
   the remaining strih+stream partials. `classify_load_failure(box, found_schema, expected_schema)`
   degrades ONLY a clean schema mismatch on a report-only box; **strih/stream and every non-schema
   failure (unreadable/corrupt JSON, a same-schema error) STAY FATAL** — their binaries come fresh
   from CI each run, so a mismatch there is a genuine hard-gate defect. The skip reason threads
   through `build_and_print_verdict_with_stream_diffs` as a trailing `Option<String>` (the 8-arg
   back-compat wrapper passes `None`), mirroring the issue-1112/#1166 `stream_frame_prev_diffs` carry
   (renamed from `..._with_stream_hashes` / `stream_content_hashes` when #1166 replaced the byte-exact
   content hash with the codec-tolerant MAD-to-predecessor near-duplicate signal).

Note the earlier issue-1094 section's aside ("emits `schema_version=3`, exactly what the current
merge expects") was true at issue-1094 time; the schema is 4 now, which is exactly why the landmine
fired. If you bump `PARTIAL_SCHEMA_VERSION` again, the STEP-1 sha gate handles the redeploy and the
degrade path keeps a still-stale report-only imag leg from ever zeroing the whole verdict.
