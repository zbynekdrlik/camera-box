---
paths:
  - "src/multi_source_window.rs"
  - "src/probe/recording_segments.rs"
  - "tests/multi_source_window_1367.rs"
  - "tests/fixtures/multi-source-1367/**"
  - "scripts/e2e_discord_report.py"
  - "scripts/window_gate_walkdown.py"
  - "tests/python/fixtures/e2e_discord_report/regen_multi_source_1367.py"
  - "tests/python/test_e2e_discord_report_multi_source_1367.py"
---

# A multi-source cambox window is judged by its node burn (issue 1367)

## What it is

The all-cambox sweep's per-window test-pattern checks read the cam2 dual-QR Vernier tick. That
tick is the max optical id in each recorded frame (`RecordingFrame::tick`). The checks assume the
window films ONE painted feed.

When a camera films an OBS monitor instead (cam2 filming the strih-lx vk-direct multiview since
25.9.2026), one frame holds several generations of the pattern. On release PR 1373's runs the tiles
were ~248 / 413 / 546 ms old, including cam2's own recursion, and the multiview renders ~30 fps off
the program grid. When the newest tile misses, the tick falls back to an older one. The result is
balanced copies/gaps, cadence failures and a `frozen_leg`, while the leg itself delivered every
frame once (its node burn: zero loss, max hold 2).

## The decision (ROZHODNUTÉ 5843424054, design 5843426470 on issue 1367)

- **The signal already exists.** A window is MULTI-SOURCE when the tear detector's per-window
  `multi_path_suspect_fraction` (frames carrying more optical QRs than one tile can produce) is
  STRICTLY above the single-sourced `tear_detect::MULTI_PATH_SUSPECT_CEILING` (0.10). This is the
  same ceiling that already makes the tear gate call such a window unscoreable. It is NOT
  `TEAR_FRACTION_CEILING` (0.005), which is the scanout-tear rate ceiling.
- **These fold REPORT-ONLY** for a multi-source window: copies/gaps, the three cadence gates
  (judder, uniformity, duplication-masked) and its `frozen_leg` entry. They are still computed,
  printed and in the JSON.
- **These still gate:**
  - the node-burn contiguity + max-hold folds (per node, whole recording, never scoped);
  - the window's presence (`frames > 0`) and its optical undecodable floor;
  - the tear gate;
  - self-heal events.
- **Fail-closed:** NaN, or a window with no fraction/scope, reads single-source. A single-source
  window behaves exactly as before.
- **Accepted limit:** a repeat introduced on the HDMI/capture side that still gets a fresh cambox
  burn is invisible while the content is multi-source. The planned re-tightening is a run-scoped
  single-camera HDMI view (option 2 on the ticket). Plain Program view is rejected: with Cam 2 on
  air it is a feedback loop.

## Where it lives

- `src/multi_source_window.rs` (pure crate-root, Tier-0) holds the logic:
  - `window_is_multi_source`, `window_check_scope`, `scope_at` (fail-closed);
  - `multi_source_tag` (`None` on single-source, so no new JSON key there);
  - `tag_line`;
  - `scoped_continuity_term`: the SAME `window_gate::decide_with_tolerance(..).overall_pass_term`,
    with copies/gaps zeroed for a multi-source window;
  - `gating_items`;
  - `partition_frozen_legs`: a `FrozenLeg.since_ns` IS its window's `start_ns`; unmatched entries
    stay gating.
- `probe::recording_segments::segment_continuity_scoped(.., multi_path_fractions)` tags each
  segment and uses the scoped term. `segment_continuity` delegates with an empty slice, so every old
  caller is unchanged.
- `recording-verdict.rs` computes `tear_by_window`/`tear_stats` BEFORE the continuity fold (the tear
  JSON + LIVE tear fold further down read the same `tear_stats`). It then scopes:
  - the cadence worsts (judder + the three uniformity readings);
  - the dup-cadence `masked_windows` + worst-masked (the gating values). The raw worst stays a
    whole-run diagnostic, and `windows[]` still lists every window;
  - the frozen partition.
- **The stdout line is honest.** The continuity headline appends
  `(N multi-source window(s) judged by their node burn, #1367)`, so `CONTINUITY-CLEAN` never reads as
  zero copies. Every copies/gaps WARN that would claim the window FAILS is suppressed for it.
- **The walk-down tool skips it too.** `scripts/window_gate_walkdown.py` leaves a multi-source window
  out of its per-camera table. Its copies/gaps are multiview generations, not the genlock-FIFO
  residual that tool measures.
- **JSON keys:**
  - `all_cambox_continuity.segments[].multi_source{tag, multi_path_suspect_fraction, ceiling,
    report_only_checks, blocking_checks}`;
  - `all_cambox_continuity.windows_multi_source`;
  - `multi_source_report_only` on `cadence_judder_gate`, `cadence_uniformity_gate`,
    `duplication_masked_cadence` and `frozen_leg`.
  - `windows_over_copies_gaps_tolerance` / `windows_singleton_allowance_consumed` skip a
    multi-source window, because neither gates it.
- **Discord report** (`scripts/e2e_discord_report.py`):
  - `_multi_source_windows` / `_multi_source_summary`;
  - item 4 of `_blocking_failures` never names a multi-source window;
  - a PASS keeps its 3 lines and appends the tag to the camera line;
  - a FAIL puts it on the `ℹ️` line;
  - `compose_report`'s overall section lists each window with its fraction;
  - its cadence section appends `— <tag>, <fraction>` to a multi-source window's line;
  - its residual-events line appends `, z toho N v multi-source oknách (<tag>)` (events matched by
    cambox + `start_ns <= gen_ts_ns < end_ns`).

## Verifying under Tier-0

- **Pure module:** the real per-window numbers of both PR-1373 runs are in
  `tests/fixtures/multi-source-1367/cam_windows.tsv`. The whole integration test + the module's own
  tests run with no cargo via a standalone replica:
  - wrap `tear_detect`, `window_gate`, `optical_floor`, `frozen_leg`, `presentation_cadence`
    (test-stripped, serde derives stripped) and `multi_source_window` as `pub mod` in one file;
  - append the integration test with `camera_box::` -> `crate::`, rewriting only lines WITHOUT a `"`
    so the source-anchor strings stay intact;
  - `rustc --test`, then `clippy-driver --test -D warnings` on the same file.
- **Probe wiring:** the end-to-end probe-gated differential is
  `multi_source_window_is_judged_by_its_node_burn_1367` in `recording-verdict.rs`:
  - single-source copies FAIL;
  - multi-source passes and is tagged;
  - a multi-source window with a delivered frame missing its cam2 burn FAILS.
  It compiles first on CI.
- **Python report fixture:** the REAL run-2059624745 verdict re-folded exactly as the Rust now
  folds it. The committed generator `tests/python/fixtures/e2e_discord_report/regen_multi_source_1367.py
  <real verdict.json> <out.json>` mirrors every key above and reproduces the fixture byte-identically.
  When a key changes, update the generator next to the Rust `json!` bodies and rerun it. Never
  hand-invent a shape.
