---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/lib/stray-session-check.sh"
  - "scripts/obs_phase2.py"
  - "tests/harness_stray_session_check_1271.rs"
  - "tests/harness_rig_busy_recheck.rs"
---

# Never mutate the rig while a broadcast may be LIVE — guard BEFORE every mutation (#1271)

The job-start `scripts/rig-busy-gate.sh` (`obs_phase2.py rig-busy-check`, #406/#312) passes MINUTES
before `recording-e2e.sh`'s mutations run — a production broadcast can start in that window. Two live
incidents proved a single check is not enough: run 33571774966 restarted the whole cam fleet's
binary (`[0/8]` parity auto-align) while the stream box was broadcasting; run 33573594588 started a
broadcast DURING the ~5 min `[1/8]` build (after an early `[0/8]` check passed) and `[2/8]`/`[2b/8]`
then deployed to all 7 cams while live.

## The rule

**The read-only rig-busy guard must run IMMEDIATELY BEFORE EVERY rig-mutation step**, reusing ONE
shared function — `stray_session_check_assert HERE STRIH STREAM [WHAT]` in
`scripts/lib/stray-session-check.sh` (the #675 sourced-helper pattern). Current call sites, each a
BARE statement (never `$(...)`/pipe/`if` — so its `exit 1` propagates): before the bkshading-relay
pause, before the `[0/8]` camera-box/painter parity auto-align, before the `[2/8]` cam1 deploy, and
before the `[2b/8]` ALL_CAMBOX deploy loop. The existing pre-`[4/8]` reroute re-check
(`obs_phase2.py rig-busy-check` inline) STAYS. **Any NEW rig-mutation step added to
`recording-e2e.sh` gets its own `stray_session_check_assert` immediately before it** — the test
`a_stray_session_guard_precedes_every_fleet_mutation_1271` pins a guard before each mutation banner;
add the new mutation's anchor to its `muts` list.

- It does NOT re-define "REAL broadcast" — it CALLS the shared `rig-busy-check` (streaming and/or
  recording on strih/stream), reads `busy`, refuses on `busy=true`. Never duplicate the per-box loop.
- SEMANTICS: fail-OPEN (WARN + proceed) ONLY when NO readable box is busy. On a partial outage
  (one box WS-unreachable → `busy=None`) it REFUSES if any box it COULD read is busy (`rig_busy_check`
  emits `diagnostics` on its error path for this). Refusing during a live broadcast is the point.
- On refusal it names WHAT is streaming per box: the ingest SERVER url + `GetStreamStatus.outputDuration`
  via the additive `obs_phase2.py stream-detail`. `redact_stream_server(server, key)` redacts
  STRUCTURALLY (urlsplit → drop query/fragment/userinfo, where an SRT `?streamid=`/rtmp `user:pass@`
  secret hides) PLUS a key-substring pass — never print the key, even partially.
- Pass `--password "${OBS_PASSWORD:-}"` to any single-host `obs_phase2.py` call (record/stream-status/
  stream-detail); only the `rig-busy-check` subparser env-defaults it. Without it, the detail read
  silently returns nothing on a WS-auth'd box.

## Anchor traps this touched (recording-e2e.sh is the static-anchor minefield)

- **A comment/guard-label mentioning `rig-busy-check` BEFORE the real pre-`[4/8]` re-check hijacks
  `harness_rig_busy_recheck.rs`'s `.find("rig-busy-check")`** (three tests key on the FIRST
  occurrence being the real re-check). Keep `rig-busy-check` a single literal occurrence in
  recording-e2e.sh — reword comments to "rig-busy state read" / name `rig-busy-gate.sh` (which does
  NOT contain the substring `rig-busy-check`).
- **Never put a `[N/8]` bracket literal in a guard-CALL line or its comment** — a `.find("[2/8]")`
  banner anchor would hit the earlier guard line instead of the real banner. Use unbracketed WHAT
  labels ("the cam1 camera-box deploy", not "[2/8] ...").
- Inserting a guard before an anchored line is safe as long as the guard call/comment carries none
  of that region's `.find()`/`.split()`/adjacency literals. Verify with the occurrence-count sweep
  (`git show origin/dev:…` vs new: flag any full-literal 1→0 / 1→2) AND re-check the specific
  adjacent ordering tests (#808 bkshading trap order, #1202 parity-gate order, #1138 frame-probe,
  the #252 `for _hbs` count, the `for _cn_ip_burn` count) — a guard adds no loop, so those counts
  must stay unchanged.

## Tier-0 verification (cargo blocked locally)

The lib is behavior-testable WITHOUT the harness: source it under real `set -euo pipefail` with a
fake `$HERE/obs_phase2.py` answering `rig-busy-check`+`stream-detail` (a plain `bash run.sh`, NOT
`bash -c` — the worktree guard refuses `bash -c` sourcing, #1265) and drive FAKE_BUSY through
idle/recording/streaming/partial/unreachable. The pure `redact_stream_server` + `rig_busy_check`
error path are pytest-covered. The Rust static-anchor + behavioral tests type-check + run only at CI.
