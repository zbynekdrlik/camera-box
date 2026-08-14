---
paths:
  - "scripts/phase_sync_calibrate.py"
  - "scripts/phase_sync_reanchor.py"
  - "scripts/latency_pins_snapshot.py"
  - "scripts/phase_sync_active_floor_check.py"
  - "tests/python/test_phase_sync_calibrate.py"
  - "tests/python/test_phase_sync_reanchor.py"
  - "tests/python/test_latency_pins_snapshot.py"
  - "tests/python/test_phase_sync_active_floor_check.py"
---

# A `main()`-level pytest test that omits `--json-path`/`--out` writes to the REAL dev1 home dir — always pin it to `tmp_path` (#893)

`phase_sync_calibrate.py --apply` with no `--json-path` resolves via
`default_last_json_path()` to `~/.camera-box/phase-sync-last.json` on this dev1 box when
`PROGRAMDATA` is unset — which is exactly the same local fallback path `#893`'s own live evidence
gathering read (`~/.camera-box/phase-sync-last.json, 2026-07-09 16:50`). **A `tests/python/
test_phase_sync_calibrate.py::TestCLI`-style test that calls `phase_sync_calibrate.main()`
through `--apply` without explicitly passing `--json-path tmp_path/...` silently OVERWRITES that
real file with test fixture data** — confirmed live (#893, 2026-07-31): three new active-set-
filter tests did exactly this on the first pass, clobbering the real persisted calibration with
`{"NDI cam5": 999.0, ...}` garbage before the mistake was noticed via `stat`/`cat` on the real
path. It went unnoticed initially because `pytest`'s own captured-output assertions never look at
the filesystem outside `tmp_path`, so nothing in the test itself failed.

**The fix, and the rule for any NEW `TestCLI`/`main()`-level test added to this file:** ALWAYS
pass an explicit `--json-path str(tmp_path / "phase-sync-last.json")` (or equivalent) in every
`sys.argv` list that includes `--apply` — even a test whose main point is something else entirely
(the active-set filter, not the persist path) still triggers the real write as an unavoidable
side effect of calling `main()` with `--apply`. Grep the test file for `"--apply"` in a `sys.argv`
list and confirm a `--json-path` sits alongside it before trusting a new test doesn't touch the
real home directory. If a real file DOES get clobbered by mistake: check whether a legitimate
live recalibration is already planned for the same ticket (it likely is, if you were working on
this file) — the correct real data will overwrite the test garbage as part of that step, so a
separate "restore" is usually unnecessary; just don't skip the real recalibration afterward.

# Recalibrating live pins does NOT require a fresh ~1-hour E2E measurement run — reuse a RECENT green run's own verdict JSON (#893)

The proper per-camera cam→strih transit measurement (`n_camera_strih_samples`/
`n_camera_median_latency_ms`, `src/probe/recording_latency.rs`) only ever gets computed as part
of a genuine `recording-e2e.sh --all-cambox`-shaped run's `recording-verdict` post-processing —
there is no lighter-weight standalone measurement path. Running a FRESH one just to recalibrate
stale pins costs real rig time (a full hardware E2E run took ~59 minutes end-to-end on this repo's
CI, 2026-07-30) and risks colliding with `full-path-e2e.yml`'s own `cancel-in-progress` concurrency
group (see the top-level CLAUDE.md's own GOTCHA on this).

**Instead, download a RECENT green run's own artifact and reuse its `all_cambox_delivery_latency`
block directly as `--measured-json` input:**

```bash
gh run download <run-id> -n recording-e2e-full-path --dir <scratch-dir>   # from inside the repo
python3 -c "
import json
v = json.load(open('<scratch-dir>/verdict-<id>.json'))
block = v['all_cambox_delivery_latency']
measured = {f'NDI cam{n}': block[f'cam{n}']['p50_ms'] for n in (1,2,3,4)}  # or CAMERA_ACTIVE_SET
json.dump(measured, open('measured.json', 'w'), indent=2)
"
python3 scripts/phase_sync_calibrate.py --host 10.77.9.202 --password "" \
  --measured-json measured.json --gate-bin target/release/phase-sync-gate --apply \
  --json-path ~/.camera-box/phase-sync-last.json
```

This is the SAME `--measured-json` contract the harness's own `[4g/8]` auto-pin step feeds the
calibrator — the only difference is the source of the measurement (a recent past run's verdict
vs. a live-just-computed one). The real physical per-camera transit times do not meaningfully
drift hour-to-hour on a static rig, so a same-day (or even few-days-old) green run's measurements
are a faithful basis for recalibration; note the data's age plainly when reporting the result.
Always verify the result with an INDEPENDENT live read-back afterward (a fresh `GetInputSettings`
call, or the `phase-sync-active-floor-gate`/`phase_sync_active_floor_check.py` pair) rather than
trusting only the calibrator's own internal read-back-and-verify step — two independent reads
proving the same state is stronger evidence than one.

**Cheaper still — `phase-sync-last.json` is ITSELF a valid `--measured-json` source.** The
persisted file already stores each camera's `latency_ms`, and that field is the
**pin-INDEPENDENT** transit (`prerecord_phase_calibrate.py` computes it as
`latency_ms + mean_head_skew_ms` — the active pin corrected by the signed deviation from its own
release schedule), **not** the observed delivery latency. So `{c["source"]: c["latency_ms"] for c
in json.load(open(...))["cameras"]}` — optionally filtered to the current `CAMERA_ACTIVE_SET` —
feeds straight back into `phase_sync_calibrate.py --measured-json` with no artifact download and
no rig measurement at all. Do NOT make the same substitution with a verdict's
`all_cambox_delivery_latency[camN].p50_ms` **as a drop-in for a subset re-anchor**: that block is
measured WITH the pins applied, so its per-camera ordering can invert against the true transits
(live 2026-07-31: the verdict showed cam3 as the FASTEST at 47.3ms while its true transit of
81.9ms made it the SLOWEST — it merely sat at the 3ms floor). The artifact path above stays
correct for a FULL recalibration of every camera, where the whole pin set is recomputed together;
the persisted-file path is what you want when re-anchoring an unchanged rig.

# Retiring a camera from `CAMERA_ACTIVE_SET` breaks the phase-sync floor gate — it removes the ANCHOR

The mutual convention pins the SLOWEST camera at the 3ms floor and holds every faster one back by
its head start. So the camera sitting at the floor is, by construction, the slowest box on the
rig — and dropping THAT camera from the active set leaves every survivor pinned above the floor,
failing the `[4h/8]` active-floor preflight (`no active camera at the floor -- lowest active pin
is cam1=21ms`) on the very next run. Nothing drifted physically; the convention lost its
reference. Live: issue 898 retired cam3 (grabber card destroyed), and cam3 was the anchor
(`latency_ms=81.85 -> pin 3`, vs cam1/2/4 at 63.93/63.23/62.36 -> 21/22/22).

**The fix is a re-anchor, not a re-measurement** — feed the calibrator the ACTIVE subset of the
persisted measurements (above) and it re-derives the set: cam1/2/4 went `21/22/22 -> 3/4/5`, a
pure constant −18ms shift that preserves the mutual differences (0/+1/+2) EXACTLY and simply
presents the whole set 18ms earlier. **Expect the mirror-image shift when the camera comes
back**: re-adding the slowest box makes it the anchor again and pushes every other pin back up by
that same constant, so re-activation is never just the one-line `CAMERA_ACTIVE_SET` edit — run
the calibrator in the same breath. Note that `phase-sync-active-floor-gate` is NOT in the CI
`probe-tools-linux-amd64` artifact (the harness builds these gate bins itself at
`recording-e2e.sh`'s own build step); to run the independent check by hand, build just that one
small default-feature bin: `cargo build --release --bin phase-sync-active-floor-gate  # airuleset:build-ok`.

# The `[4h/8]` floor gate now HAS an automatic establisher — `phase_sync_reanchor.py` (#900)

`scripts/recording-e2e.sh` runs a `[4h/8pre]` phase-sync RE-ANCHOR immediately before the
`[4h/8]` active-floor gate, ON by default (opt-out `PHASE_REANCHOR=0`), gated on `ALL_CAMBOX=1`,
FAIL-LOUD. It re-derives the ACTIVE pin set from the persisted `phase-sync-last.json` transits and
applies it, so the gate always has an establisher — closing the "gate always on, its establisher
(`[4g/8]` #757 auto-pin) always off" landmine (issue 898: retiring the floor camera red-lit a
healthy rig with no remedy).

**RE-ANCHOR ≠ RE-MEASURE — this is why it is safe where the `[4g/8]` #757 auto-pin was not.** It
introduces NO new measurement: it reads each camera's pin-INDEPENDENT `latency_ms` (the transit
basis, already persisted), restricts to `CAMERA_ACTIVE_SET`, and re-runs the UNCHANGED
`compute_phase_sync_offsets` kernel. Active-set unchanged ⇒ provable no-op (same transits → same
pins → live pins already match → zero writes); a camera leaving/joining ⇒ a pure CONSTANT shift of
every surviving pin (mutual differences preserved). Keep `PRERECORD_PHASE_CALIBRATE` off — the
re-anchor does NOT re-open the #757 measurement question.

**It PRESERVES the uniform jitter-headroom margin, it does not strip it.** `apply_margin` (issue
757) adds the SAME margin to every camera's kernel offset, and the kernel pins the slowest camera
at `PHASE_SYNC_FLOOR_MS` — so the slowest camera's applied pin is `floor+margin` and is the GLOBAL
MINIMUM offset. Therefore `recover_uniform_margin()` = `min(persisted offset_ms) - floor`, and the
re-anchor re-adds it. This reproduces a margined calibration EXACTLY (not a 1ms churn) because
`round(int + margin_float) == int + round(margin_float)` for the integer kernel offsets — the
recovered integer margin is consistent across all cameras. A margin-free calibration (the standing
default) yields margin 0 → exact no-op. If you recompute pins from `latency_ms` WITHOUT re-adding
this margin you silently STRIP the #757 headroom and churn every run — re-introducing the
copies≈gaps jitter-boundary regression on the very run that gates zero-loss.

**It reads `phase-sync-last.json` but NEVER clobbers it.** The durable file is the read-only
transit basis for ALL cameras; the applied set records only to a RUN-SCOPED `--out-json`
(the harness passes `$OUTDIR/reanchor-strih-pins-${RUN_ID}.json`). Writing the active-only subset
back over the durable file would drop every currently-inactive camera's transit basis. There is a
defensive guard rejecting `--out-json == --persisted-json`.

**FAIL-LOUD is the whole point (unlike `[4g/8]`'s best-effort `set +e`):** a missing/malformed
persisted file, or one that does not cover every active camera, is a genuine "no calibration basis"
state — `phase_sync_reanchor.py` exits nonzero so `[4h/8]` is never reached behind pins nobody set.
The pure decision layer (`load_persisted_transits` / `restrict_to_active` coverage-fail /
`plan_reanchor` no-op-vs-apply / `recover_uniform_margin`) is Tier-0 unit-tested; the harness
wiring/ordering/on-by-default/fail-loud is guarded by `tests/harness_phase_sync_reanchor_900.rs`.
The `main()`/`--apply` tests mock the WS layer and MUST still pin `--persisted-json`/`--out-json` to
`tmp_path` (the real-home-clobber hazard above applies to `main()` regardless of what it is testing).
