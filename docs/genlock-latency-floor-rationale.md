# Genlock 3 ms latency floor — why it is a real jitter floor, not an arbitrary margin (#272)

Decision record answering #272: given DanteSync disciplines the cluster clock to
µs-precision (sub-ms PTP), why does `GENLOCK_LATENCY_MS_MIN` / `GENLOCK_LATENCY_MS_DEFAULT`
still sit at 3 ms instead of sub-ms? Satisfies #272's "Outcome" — either an
empirically-lowered floor, or a documented root cause so #105's "lowest latency" claim
stays honest. This is the DOCUMENTED-ROOT-CAUSE half; the empirical sweep that could still
lower it further is a separate, supervisor/user-driven runbook (§7).

---

## 1. The premise, and where it breaks down

DanteSync's PTP servo genuinely disciplines `CLOCK_REALTIME` to sub-ms across the cluster
(`docs/phase3-clock-and-build-decision.md` §1) — that part of the premise is correct. The
genlock release deadline (`genlock_present_ts_reserve`,
`vendor/obs-studio/libobs/obs-source.c:4674`) is `wall_now − reserve_ms`, computed from
that SAME disciplined clock
(`genlock_wall_now_ns`, `obs-source.c:4625`). So the CLOCK feeding the deadline is not the
bottleneck.

**What the premise misses:** the reserve does not exist to compensate for clock
*inaccuracy*. It exists to absorb the time between a frame being CAPTURED (with a
DanteSync-disciplined timestamp) and that frame actually ARRIVING, decoded, at the genlock
FIFO — i.e. **frame-arrival jitter**, not clock drift. A µs-accurate shared timeline does
not shrink NDI network transmission time, DistroAV's receive-thread scheduling latency, or
OS thread-scheduling jitter on the machine running OBS. Those three are the real jitter
sources, and they are 1-2 orders of magnitude larger than the clock's own µs-level
uncertainty.

## 2. The measured jitter is real, and already exceeds 3 ms on one hop

`obs-source.c:4674` documents the measured NDI arrival jitter directly in the code that
uses it:

```c
/* ... The buffer need only cover the measured arrival jitter (1.6ms strih->stream,
 * 8.1ms cam1->strih), so a ~3ms reserve replaces the 33ms whole-frame preload while
 * staying zero-loss. ...  */
```

- **strih→stream** (both Windows boxes, LAN, single NDI hop): **1.6 ms** measured jitter —
  comfortably under the 3 ms floor, so this hop has headroom.
- **cam1→strih** (Linux appliance → Windows, LAN, single NDI hop): **8.1 ms** measured
  jitter — this ALREADY EXCEEDS the 3 ms floor. The floor is not "too generous" for this
  hop; it is already below the observed jitter and only holds because the FIFO (depth ≥
  `GENLOCK_AUTO_PRELOAD_MIN`, `src/probe/genlock.rs`) absorbs the excess as an occasional
  hold rather than a drop.

This one number settles the premise: if the shared clock's own uncertainty were the
dominant term, cam1→strih (same DanteSync-disciplined cluster, same PTP servo as
strih→stream) would show the SAME jitter as strih→stream. It shows 5× more, because the
dominant term is per-hop **arrival** jitter (NDI/DistroAV receive scheduling on that
specific link), not the clock.

## 3. 3 ms was empirically validated at zero loss — not chosen arbitrarily

`reserve_ms=3` was not picked as a round number; it was validated live:

- **#184 / PR #224** (merged `19472506e`, 2026-06-24 —
  ["genlock: sub-frame MS-granular jitter reserve (#184) — reserve=3ms validated
  zero-loss"](https://github.com/zbynekdrlik/camera-box/pull/224)) ran a live,
  both-hops recording at `reserve_ms=3` and confirmed zero distinct-frame loss
  (`docs/autopilot-log.md`, `## #184` entry, 2026-06-24: *"DECISION: reserve=3ms HOLDS
  zero-loss → merge #184, prod LEFT ON reserve=3ms (validated)"*). This is the origin of
  the 3 ms constant, not an assumption.
- **#257** (`obs-source.c:4550-4562`) later hard-locked 3 ms as a build const (no env
  override) precisely BECAUSE it was already the validated production value — removing the
  env knob removed a foot-gun, it did not change the number.

## 4. The worst-observed jitter (28 ms) was root-caused to CPU scheduling, not the clock — and fixed

Under box load, `ts_head_skew_ms` was observed spiking to **28 ms** on CAM2 — far above
both the 3 ms floor and the 8.1 ms cam1→strih baseline. This was root-caused and fixed,
and the cause was NOT the clock:

- **#289 / PR #304** (["Pin cam-box capture+emit to the isolated core + IRQ affinity
  (#289)"](https://github.com/zbynekdrlik/camera-box/pull/304), merged 2026-06-28; see
  `src/affinity.rs:8`: *"#289: CAM2 55-60fps wobble + underruns + 28ms head_skew"*).
  Root cause (`docs/autopilot-log.md`, 2026-06-28 entry): `isolcpus=3` reserved a CPU core
  on every box, but NOTHING was pinned to it — the realtime capture/emit thread ran on the
  LOADED general cores (USB kworkers, ssh, painter) instead, so it wobbled under load. The
  fix pinned the SCHED_FIFO capture/emit thread (and IRQs) to the isolated core.
  Root cause = **CPU scheduling contention**, not clock imprecision. This is the same
  jitter axis as §1-2 (arrival jitter), just a load-dependent tail of it — never the clock.

## 5. Where the three mirrored floors live (unchanged by this investigation)

The 3 ms floor is a compile-time constant, mirrored in three places, cross-checked at
build/runtime by a `_Static_assert` (C) and the `tests/genlock_preload.rs` vendored-source
guard, and pinned in the `#390` drift-guard backstop (`scripts/drift-guard.sh:45-54`,
`range:3-2000` — the sane [MIN, MAX] backstop for a per-source calibration-tracked
latency pin):

| Mirror | Location |
|---|---|
| Global default/floor (C) | `vendor/obs-studio/libobs/obs-source.c:4561-4562` (`GENLOCK_LATENCY_MS_MIN` / `GENLOCK_LATENCY_MS_DEFAULT` = 3) |
| Per-source floor/default (C++, DistroAV UI) | `vendor/distroav/src/ndi-source.cpp:41-42` (`PROP_GENLOCK_LATENCY_MS_MIN` / `PROP_GENLOCK_LATENCY_MS_DEFAULT` = 3) |
| Rust mirror (probe-side contract test) | `src/probe/genlock.rs:108-117` (`GENLOCK_LATENCY_MS_DEFAULT` / `GENLOCK_LATENCY_MS_MIN` = 3) |

**This investigation does NOT change any of the three.** Lowering the floor requires an
empirical sweep at LOWER reserve values, which needs floor-varied OBS *builds* (the
constant cannot be varied at runtime post-#257) — that is a build-matrix change, out of
scope for this PR, and is the subject of §6/§7 below.

## 6. Measurement tooling shipped in this PR

To make a future sweep fast to evaluate (rather than a manual log read), this PR adds a
Tier-0 pure kernel + a thin CLI:

- **`src/jitter_audit.rs`** — parses the periodic `genlock-fifo audit` log line (emitted
  every ~5 s per genlocked source, `obs-source.c` `genlock_audit_log`) into an
  `AuditSample`, groups samples by source, and `summarize`s a captured window into the
  DELTA loss/backpressure counters (`underruns`, `holds`, `dropped_due`, `late_holds`,
  `overruns`, `relocks`) plus the `ts_head_skew_ms` jitter distribution (max/mean absolute
  skew) and peak FIFO depth. Pure, cross-platform, unit-tested (10 tests, default
  features — no OBS/rig needed to run them).
- **`src/bin/genlock-jitter-report.rs`** — thin CLI: feed it a captured OBS log (stdin or
  `--file`), get a per-source report table. Exits 2 (fail closed) if no `genlock-fifo
  audit` lines are found, so a wrong log file is never silently read as "zero loss".

This is the **"per-run reserve→loss collector"**: run it once per captured segment and
compare `delta_dropped_due` / `delta_late_holds` (real loss) and `max_abs_head_skew_ms`
(jitter headroom) across segments.

## 7. What is still open — the empirical sweep (supervisor/user runbook, NOT this PR)

To go BELOW the validated 3 ms floor (rather than just document why it is honest), the
following steps are needed. They require rebuilding OBS with a lower
`GENLOCK_LATENCY_MS_MIN`/`_DEFAULT`, deploying that build, and recording live — all rig
actions this investigation does not perform:

1. **Build a floor-varied OBS.** Temporarily lower `GENLOCK_LATENCY_MS_MIN` /
   `GENLOCK_LATENCY_MS_DEFAULT` in `obs-source.c` (and the `ndi-source.cpp` mirror, kept in
   lock-step by the `_Static_assert` + `tests/genlock_preload.rs` guard) to a candidate
   value (e.g. 2, 1, or 0.5 ms — note the log format is millisecond-granular; a sub-1ms
   candidate needs the granularity checked first). This is a throwaway build, NOT a change
   to land on `dev`/`main`.
2. **Deploy + record.** Push the candidate build to strih + stream, run a real recording
   window (the existing `scripts/recording-e2e.sh` zero-loss harness is the natural
   vehicle), capture the OBS log for the run.
3. **Analyze with the new tooling.** `genlock-jitter-report --file <captured obs.log>` —
   read `delta_dropped_due` / `delta_late_holds` (any non-zero = real loss at this
   candidate) and `max_abs_head_skew_ms` (how close to the edge the candidate is running).
4. **Repeat per candidate**, building the reserve→loss table: `reserve_ms | delta_loss |
   max_abs_skew_ms`. The safe floor is the LOWEST candidate with zero loss and comfortable
   skew headroom (not the lowest candidate that merely "mostly" holds).
5. **Decide.** If a lower candidate holds zero-loss with headroom: lower
   `GENLOCK_LATENCY_MS_MIN`/`_DEFAULT` in all three mirrors, update the `#390` drift-guard
   pin (`scripts/drift-guard.sh` `GENLOCK_LATENCY_MS_MIN`), add a locking test recording
   the new validated floor (the `#184`/PR #224 pattern), and update this document. If no
   lower candidate holds: this document + §1-4 stand as the closing justification for
   #272, and #105's "lowest latency" claim is fully truthful at 3 ms.
6. **Also worth correlating (secondary, blocked on #141):** the live PTP offset/drift at
   the moment of a captured skew spike, to formally rule the clock in/out per-sample
   (rather than only architecturally, as done in §1-2 above). This needs the `#141`
   DanteSync/PTP exporter (currently open, part of the skipped observability EPIC
   #138-143) — not required to close #272, since §1-4 already rule the clock out
   architecturally and empirically (cam1→strih vs strih→stream jitter asymmetry on the
   SAME clock).

## 8. Bottom line

- The 3 ms floor absorbs **NDI-receive + render-tick + CPU-scheduling** arrival jitter —
  it has nothing to do with the DanteSync clock's own (sub-ms) precision.
- It is not arbitrary: it was empirically validated at zero loss (#184/PR #224), and one
  production hop (cam1→strih, 8.1 ms) already exceeds it.
- The worst observed jitter spikes (28 ms) were root-caused to CPU scheduling
  contention (#289/PR #304) and fixed by core pinning — again, not the clock.
- Whether it can go LOWER than 3 ms is an open, empirical question that needs floor-varied
  builds + live recordings (§7) — out of scope for a clean code PR. The tooling in §6 is
  ready for whoever runs that sweep.
