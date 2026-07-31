---
paths:
  - "scripts/phase_sync_calibrate.py"
  - "scripts/latency_pins_snapshot.py"
  - "scripts/phase_sync_active_floor_check.py"
  - "tests/python/test_phase_sync_calibrate.py"
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
