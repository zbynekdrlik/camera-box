---
paths:
  - "scripts/latency_pins_verify.py"
  - "scripts/latency_pins_snapshot.py"
  - "scripts/apply_latency_pins.py"
  - "scripts/imag_latency_enforce.py"
  - "scripts/latency-pins-baseline.json"
  - "scripts/bundle_state_gather.py"
  - "tests/python/test_latency_pins_verify.py"
  - "tests/python/test_apply_latency_pins_1003.py"
  - "scripts/drift-guard.sh"
  - "vendor/README.md"
---

# Reading + verifying per-source genlock latency pins (#1061 / #866 latency half)

## The authoritative pin key is `genlock_latency_ms_src` over OBS WS — NOT bundle-state

To read a source's per-source genlock latency pin, read `genlock_latency_ms_src` via
`GetInputSettings` over OBS WebSocket (the key `latency_pins_snapshot.read_pin` /
`imag_latency_enforce` / `latency_pins_verify` all use). **Do NOT use bundle-state's
`ndi_input_latency` facet** (`http://<box>:8899/bundle-state.json`): `bundle_state_gather.ndi_input_latency_csv`
reads DistroAV's STOCK `latency` setting, which the certified config pins to **0** everywhere
(vendor/README.md). So bundle-state reports `...=0` for every input on a perfectly healthy box —
it is the wrong source for anything about the genlock per-source hold, and seeding a pins baseline
from it would bake in all-zeros. (Confirmed live 2026-08-15: bundle-state said `NDI cam1=0..cam7=0`
while WS `genlock_latency_ms_src` said `cam1=3, cam2=6, cam3=20, ...`.)

## Latency verify-at-start is REPORT-ONLY, never overwrite

Per-source latency is the operator's A/V-align domain (repo memory
`latency-is-user-av-align-domain`), so the OBS-start verify may only REPORT drift against the
committed baseline (`scripts/latency-pins-baseline.json`), NEVER force-overwrite — the opposite of
the #1057 burn sweep-off (a burn is never legitimate operator state, so it IS forced off). A
legitimate re-tune is recorded by updating the baseline in a PR. `latency_pins_verify.py` exits
0=on-baseline / 1=drift(loud, names box+input+got+want) / 2=connect-or-enumeration failure.

## `apply_latency_pins.py` is the DELIBERATE writer counterpart (#1003) — DRY-RUN default

The verify path is passive-report; to actually PUSH a newly-agreed baseline onto a live box there
is `scripts/apply_latency_pins.py` — the ONLY sanctioned WRITER of `genlock_latency_ms_src` here.
It reads the SAME baseline json and applies each explicit per-source pin over WS, but is
**DRY-RUN by default** (prints `live -> want` per source, writes nothing); `--execute` is the only
path that writes, so a promotion is deliberate + operator-invoked in a **NO-E2E maintenance
window**, never automatic at launch. Idempotent (a source already on-baseline is a no-op),
read-back verified, FAIL LOUD on a read-back mismatch (never a half-set source). **GOTCHA (#1168):
`apply_pins` signals its fail-loud read-back mismatch by raising `SystemExit` (a `BaseException`, NOT
an `Exception`).** So any BEST-EFFORT wrapper around it (a restore/teardown handler that must log and
continue, never abort) MUST catch `except BaseException`, not `except Exception` — an `except
Exception` lets the `SystemExit` escape and MASK whatever the caller was doing (the #1168 abort-restore
bug: a restore-time read-back failure replaced the original per-camera `AlignmentImpossible` with the
generic writer error; caught by review, fixed to `except BaseException` in `qr_align_pins._restore_after_abort`).
It REFUSES the imag floor-sentinel box (`_all_ndi_inputs_ms`) — imag's 3ms floor is `imag_latency_enforce.py`'s
domain (`imag-min-latency-3ms-always`), never promoted. CLI mirrors the verify tool:
`apply_latency_pins.py --box strih --host 10.77.9.202 [--execute]` (DRY-RUN without `--execute`).
This is the sanctioned "operator/gate legitimately re-tunes → record in a PR → apply to the rig"
loop; the verify-at-start still never writes. **`--pins '{"NDI cam1":3,...}'` (JSON inline or
`@file`) pushes a COMPUTED set instead of the committed baseline** — the manual RUNBOOK path (a
supervisor pushing a specific set by hand). The #1003 floor-3 aligner (`scripts/qr_align_pins.py`)
does NOT shell out to `--pins`; it imports and calls `apply_pins()` DIRECTLY for its per-run plan —
the SAME read-back-verified, fail-loud writer. Either way the write is strih-only: `--pins` refuses
an underscore/imag-floor sentinel key, and the aligner is only ever handed the strih align sources.

## Fail CLOSED on enumeration, honest-None on a per-input read (two different rules)

The imag floor path enumerates live NDI inputs; a failed/malformed `GetInputList` must FAIL CLOSED
(raise → exit 2), never a vacuous "0 inputs ⇒ all OK" (`.claude/rules/burn-target-enumeration.md`
/ `camera-active-set.md`). So the enumerate `GetInputList` runs with `ignore_err=False`, and a
floor box that read zero inputs is exit 2. A per-INPUT `GetInputSettings` read is the opposite: a
missing source/key is an honest `None` (N/A), never a fabricated floor value — mirrors
`latency_pins_snapshot.read_pin`.

## An ABSENT pin on a GENLOCK BUILD is the build DEFAULT, not DRIFT (#1295)

`GetInputSettings` returns only EXPLICITLY-saved settings, so a source whose `genlock_latency_ms_src`
was never persisted reads honest-`None`. On a STOCK build that absence genuinely means "no genlock"
→ keep the N/A + DRIFT report. But the genlock DistroAV fork REGISTERS the per-source defaults
(`vendor/distroav/src/ndi-source.cpp` `ndi_source_getdefaults`: `obs_data_set_default_bool
genlock_fifo=true`, `obs_data_set_default_int genlock_latency_ms_src=3`), so on a genlock box an
absent per-input key means the build DEFAULT (3 ms, FIFO on) is in effect — OK, not drift. This bit
RESOLUME-SNV live (2026-09-13): its `cg_scenes` collection was saved by stock OBS, so all nine
`sp-*_video` inputs read `got=N/A want=3ms` DRIFT while the FIFO audit proved the default was live.

So `latency_pins_verify.py` reads the box's **build identity over the EXISTING WS** —
`read_genlock_default_ms(ws)` = `GetInputDefaultSettings(inputKind=ndi_source).genlock_latency_ms_src`
(the SAME call `obs_phase2._effective_input_settings` uses, so NO new transport: not ssh, not the
`:8899` bundle-state facet, not a `GENLOCK_BUILD_SHA.txt` file read). A non-None default == genlock
build. `read_live_pins` returns `(pins, build_default)`; `diff_pin`/`verify_box` take an optional
`build_default` so an absent key resolves to `got=default(N)` (OK when it matches the agreed pin, a
named `got=default(N)ms want=...` DRIFT when it does not). `build_default=None` (stock build, or a
defaults-read failure — best-effort, conservatively treated as stock) keeps the prior N/A-DRIFT path
verbatim. The #149 obs_phase2 note that "genlock_fifo is NOT in the type defaults" is STALE — it
predates the fork adding these to `get_defaults`; confirmed by the `ndi-source.cpp` lines above.

## Baseline scope

**strih baseline is the FLOOR-3 live model (issue 1168, 2026-09-15): `NDI cam1..cam7 = 3`** — every
on-air strih camera (the current `CAMERA_ACTIVE_SET`, now 7 cameras) at the 3 ms floor. This retired
the old `cam1/cam2/cam3` set + the stale `cam3 = 20` reference: the per-run `[4i/8align]` aligner
(`scripts/qr_align_pins.py`) OWNS any relative offset (re-derived live, NEVER committed here), so the
report-only reference is the floor for every camera. **A post-align above-floor offset legitimately
REPORTS drift at OBS start** — the aligner's offsets persist in the scene collection, so a later
`latency_pins_verify.py` read on an offset camera reads a named `got=Nms want=3ms` DRIFT (report-only,
never overwritten): that is an align offset, not a real drift. cam2 (the projection probe, EXCLUDED
from `CAMERA_ALIGN_SET`, issue 1216) is not floored by the aligner but its report-only floor reference
is 3 (issue 1168 lever 1 recorded cam2 6 → 3; re-verify cam2's A/V before applying live). stream pins
`NDI 2ME PGM` with a `{want_ms, tolerance_ms}` band (the A/V-align hold; a band absorbs
ordinary re-tuning while catching a gross revert). imag uses the `_all_ndi_inputs_ms` floor
sentinel (always 3). **NOTE the split (#1003):** the drift-guard REFERENCE set (this file) ≠ the
per-run ALIGNMENT set. Alignment covers `CAMERA_ALIGN_SET` (a SUPERSET incl. cam4 — every on-air
strih camera), because cam4 is on-air even though excluded from the measurable E2E sweep; the
per-run floor-3 pins are re-derived live, never committed here.

## Production alignment = the per-run FLOOR-3 auto-align, NOT a hand-baked baseline (#1003 owner rework, 2026-08-20)

**The DELIVERY-equalized DEEP promotion (`cam1=90/cam2=160/cam3=184` + stream hold `791`) was
REJECTED by the owner and REVERTED (`0aaa2fc93`) to the shallow `3/6/20` + `915`.** The owner's
binding corrections, now the model:

1. **FLOOR-3, relative-only, never absolute depth.** The slowest (max-transport) on-air strih
   camera gets pin **3** (floor); every other gets `3 + its RELATIVE delivery delta`. The rejected
   deep set added ~180 ms of needless chain latency. Alignment compensates ONLY relative inter-card
   differences.
2. **Deltas are RE-DERIVED robustly**, from the SIMULTANEOUS painter-QR screenshot spread (a
   barrier `GetSourceScreenshot` of every on-air strih input → painter dual-QR decode → the EXACT
   `gen_ts_ns` delivery delta), medianed over rounds, underrun/undecodable-excluded. NOT the MEQ
   single delivery-p50 sample (which baked in a degraded cam1 grabber — a 94 ms delta between
   identical cards on one switch is nonsense).
3. **ALL on-air cameras incl. cam4** are aligned (`CAMERA_ALIGN_SET`, a SUPERSET of
   `CAMERA_ACTIVE_SET` — the offline-ack "outside-measured-set" covers only the E2E sweep, never
   production alignment).
4. **It is an AUTOMATIC per-run process**: `scripts/qr_align_pins.py`, wired as
   `recording-e2e.sh`'s BLOCKING `[4i/8align]` step — measure → align (floor 3) → RE-MEASURE →
   ABORT the run (per-camera named reason) if it stays misaligned (`frame_id` spread ≤ 1 is the
   owner's "same monotonic + time in every QR" parity gate).

So the strih block in `latency-pins-baseline.json` is now ONLY the reverted-shallow drift-guard
REFERENCE (report-only, `latency_pins_verify.py`); the AUTHORITATIVE per-run pins are re-derived
LIVE each run and are NEVER committed (owner: "nie jednorazová ručne pečená baseline"). The
baseline's `_align_model` key documents this. **DOMAINS the aligner never crosses:** the stream
`NDI 2ME PGM` hold (operator A/V-align domain) and imag's 3 ms floor are never in the align set, so
they are never written; the independent SOURCE-side cross-camera spread gate (recording-verdict)
stays a separate blocking proof. The `MEASUREMENT_EQ=1` deep-pin profile is a SEPARATE opt-in
experiment (superseded by floor-3 for production alignment), mutually exclusive with the
`[4i/8align]` step in one run.

## Two per-source-latency mechanisms coexist — the baseline json is authoritative (#757)

There are TWO per-source strih latency facets, and they must not be confused:
1. **`scripts/latency-pins-baseline.json` + `latency_pins_verify.py` (#1061) — AUTHORITATIVE.**
   WS key `genlock_latency_ms_src`, live-read REPORT-ONLY at OBS start (wired in
   `scripts/launch-obs-genlock.sh`). This is where the true post-pivot per-source pins live and
   get re-recorded on a legitimate operator re-tune.
2. **`vendor/README.md`'s `genlock_source_latency_strih` row + `scripts/drift-guard.sh`'s
   `--compare genlock_source_latency=` facet — DORMANT, backstop only.** Nothing invokes the
   per-source live-compare (only `--check-pins` is wired, in `.github/workflows/ci.yml`, and it
   validates STRUCTURE only). Post-#757 that row is a **clamp-range backstop** (`camN=range:3-2000`,
   the same #390 model as `genlock_source_latency_stream`) — NOT hard-pinned ms values, which
   re-go-stale on the next operator re-tune. Never re-hardcode the live values (`cam1=3,cam2=6,...`)
   into it; they belong in the baseline json.

## Editing the baseline VALUES couples to a hardcoded verify-test fixture

`tests/python/test_latency_pins_verify.py::test_main_drift_exits_1_clean_exits_0` hardcodes the
strih baseline pins as a fixture (the "clean" live read that must exit 0, plus a revert that must
exit 1). So ANY change to the strih/stream baseline VALUES in `latency-pins-baseline.json` must
update that fixture in the same PR, or it goes RED (the clean read now looks like drift). The
`normalize_spec`/`diff_pin`/`verify_box` tests use their OWN in-test literals (not the file), and
`test_baseline_file_loads_and_has_the_three_boxes` only checks STRUCTURE — only the drift fixture
carries the concrete values. When promoting/re-tuning pins, `grep -n "NDI cam1" tests/python/` +
run `pytest tests/python/test_latency_pins_verify.py` before pushing (Tier-0: pytest runs freely,
no cargo). #1003's deep promotion changed strih 3/6/20→90/160/184 + stream 915→791 and updated this
fixture; the owner-rework REVERT (`0aaa2fc93`) then changed the baseline VALUES back to 3/6/20 + 915
but LEFT this fixture (and `test_apply_latency_pins_1003.py`'s reverted-baseline class) asserting the
deep numbers — a classic incomplete-revert dangling test. Both were re-pointed to the reverted
shallow set as part of the floor-3 rework. **issue 1168 lever 1 (2026-09-01)** then re-tuned cam2
6→3 (the projection probe's leftover pin — cam2 is EXCLUDED from `CAMERA_ALIGN_SET`, issue 1216, so
the per-run aligner never floors it). **issue 1168 (2026-09-15)** then refreshed strih to the FLOOR-3
live model **`NDI cam1..cam7 = 3`** (retiring the stale cam3=20 reference + extending the cam1..cam3
set to the 7-camera on-air fleet), and updated exactly the SAME two file-reading fixtures:
`test_main_drift_exits_1_clean_exits_0`'s clean/drift reads (now the floor set clean / a single-pin
drift) and `test_apply_latency_pins_1003.py`'s two reverted-baseline assertions (now
`all(strih[cam{n}] == 3 for n in 1..8)` / the explicit-pins `{cam{n}: 3}`) — the ~38 OTHER `cam2:6`
literals across the suite are independent test data (NOT the file) and were correctly left untouched.
Lesson restated: a baseline-VALUES change (any direction) must update the file-reading fixture(s) in
the SAME change, and ONLY those.

## Local verification of a `vendor/README.md` pin/doc edit — Tier-0 blocks `cargo test`

camera-box Tier-0 forbids local `cargo test` runs AND disables the `# airuleset:build-ok` bypass
(#477) — so the drift-guard integration tests (`tests/drift_guard.rs`) cannot be RUN locally; the
full suite runs at CI/integration. Verify a pin/doc edit locally with the bash script directly
(runs freely, no cargo build) — it exercises the SAME `validate_nonempty` +
`validate_source_latency_range` CI runs and echoes each extracted `pinned_setting` value, so you
can eyeball the exact pin string your `tests/drift_guard.rs` real-manifest assertion will read:

```bash
./scripts/drift-guard.sh --check-pins   # green = all pins present + well-formed + vendored source matches
cargo fmt --all --check                 # rustfmt parses (so a test-file edit is brace/format-balanced)
```

A live read-back (READ-ONLY) confirms the baseline is current:
`OBS_PASSWORD=<local secret> python3 scripts/latency_pins_verify.py --box strih --host 10.77.9.202`
(exit 0 = live pins match the committed baseline).
