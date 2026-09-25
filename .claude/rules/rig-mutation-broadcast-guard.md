---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/lib/stray-session-check.sh"
  - "scripts/obs_phase2.py"
  - "tests/harness_stray_session_check_1271.rs"
  - "tests/harness_rig_busy_recheck.rs"
  - "scripts/lib/strih-lx-deploy.sh"
  - "tests/deploy_genlock_fleet_strih_lx_exec_1317.rs"
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

**The SAME shared guard is reused OUTSIDE `recording-e2e.sh` too** — `scripts/bkshading-deploy-relay.sh`
calls `stray_session_check_assert` as its rig-busy PREFLIGHT before the first cambox ssh/scp (a relay
deploy DURING live production fork-wedged cam1, issue 1229 2026-09-13). Its `--force-live` flag is the
ONE sanctioned bypass (supervisor-only, logged). So ANY new dev1-orchestrated tool that MUTATES a
rig/cambox box (deploy, restart, reconfigure) reuses THIS guard — never a fresh per-box WS loop; when
editing the guard, remember the deploy tool is a second live caller, not just the E2E harness.

**Third caller — the strih-lx genlock deploy (`scripts/lib/strih-lx-deploy.sh` `strih_lx_apply`,
issue 1317).** The deploy STOPS the production strih OBS, so it runs the guard TWICE through one
helper `_strih_lx_broadcast_guard`: at preflight (after the identity + installer checks, immediately
before the first mutation, the stage prep — exit 4, nothing changed) and AGAIN immediately before the
graceful stop (staging a ~2 GB bundle can take minutes — exit 4 in step `stop`, OBS NOT stopped). Both
read the strih-lx dial IP + the obs-fleet stream host (`deploy-genlock-fleet.sh` sources the guard
lib). A caller that needs its OWN exit code (the deploy's contract is 0/3/4/5, the guard `exit 1`s)
runs the guard in a SUBSHELL and maps a non-zero rc — never re-implement the busy read to get a
different code: `{ err="$( ( stray_session_check_assert … ) 2>&1 1>&3 3>&- )"; rc=$?; } 3>&1` keeps
the guard's stdout flowing and captures its stderr, which `stray_session_busy_summary` (in the SAME
guard lib, reusable by every caller) parses — the `rig-busy-check:` JSON + the `<box> streaming:`
detail lines — to name what is live in the exit-4 message. So the guard's refusal TEXT is a parsed
interface: keep the `    rig-busy-check: <json>` and `    <label> streaming: <detail>` line shapes
stable, or update the summary + its test together. The fail-open WARNING is caller-neutral (only the
E2E harness has a job-start gate; the strih-lx deploy records the both-unreadable case as an
accepted risk in its lib header). The test seam is `STRIH_LX_OBS_PHASE2_DIR` (a fake `obs_phase2.py`,
like `BKSHADING_DEPLOY_OBS_PHASE2_DIR`; its `live-after-stage` mode drives the pre-stop refusal); the
test harness also drops `OBS_PASSWORD`, because the guard passes it as an argv the fake logs.

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
