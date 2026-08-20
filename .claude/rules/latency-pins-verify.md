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
read-back verified, FAIL LOUD on a read-back mismatch (never a half-set source). It REFUSES the
imag floor-sentinel box (`_all_ndi_inputs_ms`) — imag's 3ms floor is `imag_latency_enforce.py`'s
domain (`imag-min-latency-3ms-always`), never promoted. CLI mirrors the verify tool:
`apply_latency_pins.py --box strih --host 10.77.9.202 [--execute]` (DRY-RUN without `--execute`).
This is the sanctioned "operator/gate legitimately re-tunes → record in a PR → apply to the rig"
loop; the verify-at-start still never writes.

## Fail CLOSED on enumeration, honest-None on a per-input read (two different rules)

The imag floor path enumerates live NDI inputs; a failed/malformed `GetInputList` must FAIL CLOSED
(raise → exit 2), never a vacuous "0 inputs ⇒ all OK" (`.claude/rules/burn-target-enumeration.md`
/ `camera-active-set.md`). So the enumerate `GetInputList` runs with `ignore_err=False`, and a
floor box that read zero inputs is exit 2. A per-INPUT `GetInputSettings` read is the opposite: a
missing source/key is an honest `None` (N/A), never a fabricated floor value — mirrors
`latency_pins_snapshot.read_pin`.

## Baseline scope

strih baseline covers the default `CAMERA_ACTIVE_SET` (cam1/cam2/cam3); retired-grabber pins
(cam4..7) are deliberately excluded so a stale pin is never checked. stream pins `NDI 2ME PGM`
with a `{want_ms, tolerance_ms}` band (the A/V-align hold; a band absorbs ordinary re-tuning while
catching a gross revert). imag uses the `_all_ndi_inputs_ms` floor sentinel (always 3).

**Post-#1003 promotion (2026-08-20): strih carries the DELIVERY-equalized aligned DEEP pins
`cam1=90/cam2=160/cam3=184` (NOT the old shallow `3/6/20`) and the stream hold is `791` (NOT 915).**
These are the measurement-eq resolver's output — `python3 scripts/e2e_measurement_pins.py resolve
--profile scripts/e2e-measurement-pins.json` prints exactly `{cam1:90, cam2:160, cam3:184}` +
`stream_hold_ms 791`, locked by `test_apply_latency_pins_1003.py`'s provenance test. A profile
re-derivation (staleness) that changes those numbers therefore fails that Tier-0 test until the
baseline is RE-PROMOTED — the deliberate coupling that keeps production == the validated derivation.
CAVEAT: after promotion the measurement-eq profile's own `production_pin_ms`/`production_hold_ms`
references (`3/6/20`, `971`) are STALE (production IS the deep pins now), so `MEASUREMENT_EQ=1` must
NOT be re-run against the promoted production without first re-basing the profile — the profile was
the vehicle to VALIDATE the pins; once promoted it is redundant for pin-setting.

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
no cargo). #1003 promotion changed strih 3/6/20→90/160/184 + stream 915→791 and had to update this
one fixture.

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
