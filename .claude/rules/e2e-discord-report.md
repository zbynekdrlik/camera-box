---
paths:
  - "scripts/e2e_discord_report.py"
  - "scripts/lib/e2e-discord-report.sh"
  - "tests/python/test_e2e_discord_report*.py"
---

# E2E Discord report — two renderings + derived blocking-vs-report-only (#711, #1127)

`scripts/e2e_discord_report.py` composes the per-run full-path E2E notification. Since #1127 it has
**TWO renderings** and one **derived** gate classification. Get these wrong and you either recreate
the multi-page wall the owner hated or hide a real FAIL.

## Two renderings — pick the right one

- **`compose_summary(verdict, meta)`** → the SHORT, phone-readable Discord body. Reached ONLY via
  the CLI `--json-chunks` flag. PASS = exactly 3 lines (verdict-first, "N kamier, 0 stratených
  snímok", CI link). FAIL = verdict + ONLY failing BLOCKING gates (one line each, #1117 ownership)
  + at most ONE collapsed `ℹ️ sledované (negatuje verdikt)` line + link. **Report-only metrics are
  NEVER rendered as `❌`** (owner directive #1127, angry — a `❌` on a PASSING run is the exact bug).
- **`compose_report(verdict, meta)`** → the FULL detailed 1️⃣–6️⃣ wall. This is the plain-text mode
  (no `--json-chunks`) = the CI-log / manual-inspection rendering. UNCHANGED since #711. The Discord
  body no longer carries it; the full detail lives here + in the uploaded verdict JSON artifact.

## The caller captures stdout `2>&1` and jq-parses it

`scripts/lib/e2e-discord-report.sh` runs the composer with `--json-chunks 2>&1` and pipes to `jq`.
So the `--json-chunks` path MUST print **ONLY** the JSON array on stdout — any stray `print()` /
warning to stdout OR stderr corrupts the capture and the report is silently skipped (fail-open).
Keep all diagnostics out of the composer; the shell is fail-open by design.

## Blocking-vs-report-only is DERIVED from the verdict JSON, mirroring recording-verdict.rs

Do NOT hardcode which gates are blocking. `src/bin/recording-verdict.rs` folds `all_pass &= …`;
each LIVE-toggleable seam ships `gates_overall_pass=true` in its JSON node, each report-only seam
ships `gates_overall_pass=false`. `_blocking_failures()` honors that field; `_report_only_tripped()`
lists the report-only ones for the `ℹ️` line.

- **Unconditionally BLOCKING** (no `gates_overall_pass` field — always fold): `full_chain.zero_loss`
  (already EXCLUDES the imag leg), per-cam `full_chain.loss.camN.zero_loss` (digital burn), the
  `full_chain.loss.cam2_*` V4L2 capture nodes (physical fault → "Treba fyzicky skontrolovať"),
  `all_cambox_continuity.overall_pass`, and `all_cambox_latency.spread_gate_pass` (**SOURCE** side).
- **LIVE seams** (blocking only while their node's `gates_overall_pass==true`): `all_cambox_av_sync`
  (`gate_pass`), `latency.cam_strih_gate`, `all_cambox_continuity.cadence_judder_gate`, and — since
  **#1142** — `all_cambox_continuity.cadence_uniformity_gate` (NEW uniformity floor),
  `all_cambox_delivery_latency` (`spread_gate_pass` + `gates_overall_pass` — the DELIVERY spread,
  flipped BLOCKING; the block now surfaces `gates_overall_pass` so the classifier auto-follows), the
  imag PRESENCE/VERIFICATION terms (`full_chain.imag_leg_verified` not-acked +
  `full_chain.loss.imag.imag_presence_pass`, both guarded by their `gates_overall_pass`), and — since
  **#1166** — `all_cambox_continuity.duplication_masked_cadence` (`masked_windows > 0` +
  `gates_overall_pass` — the duplication-masked 50→60 pulldown detector, flipped BLOCKING at the
  promote cycle; see `verdict-gate-seam-calibration.md` §15 for the calibration table), and — since
  **#905** — `frozen_leg` (`frozen` non-empty, NOT `stale_replay`) + `self_heal_reset` (`attributed`
  OR `unattributed_events`), both guarded by their node's `gates_overall_pass` (item 15 in
  `_blocking_failures`; restored from the #914 report-only decoupling).
- **REPORT-ONLY** (`gates_overall_pass=false`, NEVER a `❌`): the imag PER-FRAME CONTENT terms only
  since #1142 (`all_cambox_continuity.imag` continuity + `full_chain.loss.imag.imag_content_pass` —
  the observer-effect-confounded burn/beat; the imag PRESENCE terms are now BLOCKING above),
  `cold_cut_onset`, the optical undecodable floor (`run_wide_undecodable_within_floor`), lipsync,
  and `frozen_leg.stale_replay` (which NEVER gates — the `frozen`/self-heal terms flipped BLOCKING
  by **#905**, see LIVE above; only the milder stale-replay signal stays report-only). **NOTE (#1142
  + #1166):** the delivery spread and
  duplication_masked_cadence stay report-only ONLY on a PRE-flip verdict (no `gates_overall_pass` —
  or `gates_overall_pass=false` — on the respective block); each `_report_only_tripped` branch is
  guarded `gates_overall_pass is not True` so a post-flip verdict
  routes it to blocking instead (no double-count).

**When you add a NEW gate to recording-verdict.rs:** if it folds into `all_pass`, add it to
`_blocking_failures`; if report-only, add it to `_report_only_tripped`. The summary has a safety net
(overall_pass=false with no matched blocking gate → a generic "pozri CI log" line) so a forgotten
blocking gate degrades to a generic FAIL rather than a hidden one — but attribute it properly.

## Fixtures

Two REAL verdict JSONs anchor the classifier (never invent field shapes):
`tests/python/fixtures/e2e_discord_report/verdict_real_pass_reportonly_1104689227.json`
(overall_pass=true WITH report-only crosses — the run that confused the owner) and
`verdict_real_fail_cam1_77008829.json` (CAM1 stream-continuity window over tolerance → FAIL).

## CI-run link

`compose_summary` stays PURE — it renders `meta["run_url"]`. The impure `main()` derives it from
`GITHUB_SERVER_URL`/`GITHUB_REPOSITORY`/`GITHUB_RUN_ID` (or the `--run-url` override); outside CI
the link line falls back to naming the verdict-JSON artifact.
