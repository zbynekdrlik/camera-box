---
paths:
  - "scripts/lib/cambox-parallel-restore.sh"
  - "tests/harness_cambox_parallel_restore_*.rs"
  - "tests/harness_cambox_parallel_stagger_*.rs"
---

# cleanup() cambox parallel-restore group (#712/#713/#715/#1085)

cleanup()'s device-restore phase in `scripts/recording-e2e.sh` backgrounds cam1 (SOURCE) + every
active secondary (the ALL_CAMBOX loop) + cam2/painter into ONE shared
`CAMBOX_PARALLEL_PIDS`/`CAMBOX_PARALLEL_LABELS`/`CAMBOX_PARALLEL_IPS` group, then one
`cambox_parallel_wait_and_report`. The whole lib is SOURCED into cleanup()'s `set +e` EXIT trap, so
NOTHING in it may `exit` and every step must be guarded (`|| return 0`, `2>/dev/null || true`) —
the trap must always run to completion (#649/#675/#712 warn-only discipline).

## Launch-stagger (#1085) — the connection-burst fix

`cambox_parallel_stagger` is called as the FIRST statement inside each `( ... ) &` restore subshell,
BEFORE its ssh. It reads `${#CAMBOX_PARALLEL_PIDS[@]}` — the count of restores ALREADY launched at
FORK time (the parent appends this box's PID only AFTER the `&`), i.e. this box's 0-based launch
index — and sleeps `index * CAMBOX_PARALLEL_STAGGER_MS` (default 300ms; `0`/non-integer disables;
EMPTY falls to the default). So N restores spread their CONNECTION establishment instead of bursting
in the same instant (the dev1-side burst #715/#675 measured as ~100% rejected within ~1.93s).

- **Fork-time index is race-free**: a `( ... ) &` subshell reads a copy-on-write SNAPSHOT of the
  array at fork; the parent's later `+=("$!")` never reaches the already-forked child. So `stagger`
  reading the count in the subshell yields exactly (boxes launched before this one).
- **Cancellation nuance — do NOT overstate it.** In-subshell stagger is NOT strictly better than a
  parent-side inter-launch `sleep` for a GH-Actions cancellation: a box still in its pre-connect
  stagger sleep when the kill lands has NOT issued its restore either way, so the stranding window is
  the same small bounded `<=(N-1)*gap` (~1.5s) — far smaller than the pre-#712 per-box-sequential
  loop's (up to N*CLEANUP_SSH_TIMEOUT), backstopped by the #715 retry / #684 FINAL / dead-man nets.
  In-subshell is chosen for CLEANLINESS (parent never blocks; one self-contained lib call), and the
  `no-overstatement` memory rule applies — state the bounded tradeoff, never "no box unreached".

## Explicit-IP retry (#1085 retires #715's label→IP parse)

The launch sites record `CAMBOX_PARALLEL_IPS` in lockstep with PIDS/LABELS.
`cambox_parallel_wait_and_report` records a failed box's IP into `CAMBOX_PARALLEL_FAILED_IPS`;
`cambox_parallel_retry_failed` iterates by INDEX and PREFERS that explicit IP, prune both `_still`
arrays in lockstep. `cambox_parallel_label_ip` is kept ONLY as a fail-open fallback for a caller
that did not populate the IP array (e.g. the #715 unit tests — they set PIDS/LABELS only). The
painter is still NEVER pruned (is-active can't tell a black monitor from a live one, #863).

## Static-anchor gotcha when editing the launch sites (#1085, self-hit)

The `tests/harness_cambox_parallel_restore_71*.rs` drivers extract the loop/phase from
`recording-e2e.sh` and RUN it (faking `timeout`/`sshpass`). Two things bite:

- **Adding a per-launch behaviour that changes wall-clock (like the stagger) breaks the #712/#713
  timing tests' parallelism bounds** — those drivers must `export CAMBOX_PARALLEL_STAGGER_MS=0` to
  isolate ssh-round-trip timing; the stagger gets its own timing test.
- **A line placed as the first statement INSIDE a `( )` subshell sits ABOVE that subshell's ssh
  target (`root@"$CAM1_IP"` etc.).** So a test region sliced FROM the ssh anchor (`&body[ssh_pos..]`)
  EXCLUDES it — anchor on the ssh target and check a short window BEFORE it (`phase[..tpos].rfind`),
  or slice from the subshell's `(`. (This PR's own wiring test hit exactly this.)
- The lib no-`exit` guard: anchor on statement forms (`\nexit `, `; exit `, `|| exit `), never a
  bare `" exit "` (comment-trippable).
