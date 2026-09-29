---
paths:
  - "scripts/recording-e2e.sh"
  - "tests/harness_recording_e2e_*.rs"
  - "tests/harness_e2e_execute_verdict_703.rs"
---

# `recording-e2e.sh`'s cleanup() ALWAYS restores stream's genlock latency on exit — any late OBS write must compose with that, not race it

`cleanup()` is the bash `EXIT` trap (`trap cleanup EXIT HUP INT TERM`) — it runs on EVERY exit
path, including a clean `exit "$GATE"`. Near the end of `cleanup()` it calls `obs_phase2.py
teardown --host "$STREAM"`, which restores `NDI 2ME PGM`'s `genlock_latency_ms_src` back to
whatever it was **snapshotted at the START of this run** (the `#358`/`#691` delivery-verify
snapshot/restore pattern, `_snapshot_and_set_test_latency`/`_restore_test_latency` in
`scripts/obs_phase2.py`).

**Consequence:** any OBS write to that same property done LATER in the main script body (e.g. at
`[8/8]`, post-verdict) is a no-op the moment the script exits — this restore call runs
unconditionally a few seconds later and silently overwrites it. This bit `#856` (wiring
`av_sync_calibrate.py --apply` in): the fix could not simply call the apply where the correction
is computed; it had to be placed **inside `cleanup()` itself, immediately AFTER the
`obs_phase2.py teardown --host "$STREAM"` call**, gated on a var (`AV_SYNC_APPLY_OFFSET_MS`,
declared empty BEFORE the trap installs, same convention as `AV_SYNC_CALIBRATED_MS`/
`IMAG_PREV_SCENE`) so it only fires on a real, computed correction and composes with the restore
instead of racing it.

**Rule for any future step that needs a LATE OBS write to survive to the next run:** do the
computation in the main body (best-effort, never touching `$GATE`), stash the RESULT in a
pre-trap-declared variable, and do the actual write inside `cleanup()`, after the existing
`obs_phase2.py teardown --host "$STREAM"` line. Never write directly at the computation site if
the property in question is one `cleanup()`'s teardown call also restores.

## The byte-window static-anchor test in `harness_e2e_execute_verdict_703.rs`

`recording_e2e_execute_mode_runs_the_merge_and_propagates_its_exit_code` slices a FIXED byte
window (currently 6200 bytes) starting at the merge call
(`"$VERDICT_BIN" "${MERGE_ARGS[@]}" || GATE=$?`) and asserts `exit "$GATE"` appears inside it.
Any new step you add between the merge call and that exit (a report step, a combine step, a
snapshot step) pushes `exit "$GATE"` further away and can silently trip this test — WIDEN the
window by roughly the new step's own byte size, following the file's own running comment trail
(`#758`/`#756`/`#827`/`#856`, each documenting why the window grew). This is a SANCTIONED,
expected edit when adding a genuinely new step there — not a hack — but always re-measure the
actual byte delta rather than guessing a round number.

## A NEW rig mutation whose OFF lives late in cleanup() needs a quick OFF right after the StopRecord-first block (issue 1302)

A cancelled job gets SIGINT, then SIGKILL a few seconds later. cleanup() runs the StopRecord-first
block, then the camera device restores (tens of seconds worst case), and only then the late calls
such as `cg_chain_cleanup`. So a burn or other toggle that is turned OFF only there is still ON after
a cancel. This matters most when the state PERSISTS: the cg OBS `genlock_burn` is saved in its scene
collection and no sweep covers that box.

The pattern (`cg_chain_cleanup_burns_first` in `scripts/lib/cg-chain-e2e.sh`):
- one call line right after the StopRecord-first block;
- ONE quick OFF per owed mutation, under a short per-call timeout;
- each OFF is its OWN background job, so it never delays the device restores and one slow endpoint
  never queues another's OFF;
- tag the job's lines as a first pass, so a one-try LEAK is not read as the verdict;
- key it on an "owes an OFF" flag: set BEFORE the ON is sent, cleared only by a verified OFF, set
  again by an unverified OFF. A "verified ON" flag misses a signal between the ON request and its
  read-back, and it misses an OFF that ended in a LEAK;
- the late cleanup call waits for the jobs, then retries with the normal attempts (the verdict).
