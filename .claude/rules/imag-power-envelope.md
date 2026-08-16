---
paths:
  - "scripts/lib/imag-power-envelope.sh"
  - "scripts/imag-power-envelope.sh"
  - "scripts/imag-power-envelope-guard.sh"
  - "scripts/imag-power-envelope-alert-watchdog.sh"
  - "scripts/imag-obs-watchdog.py"
  - "tests/python/test_imag_obs_watchdog_snapshot_849.py"
  - "tests/harness_imag_power_envelope_1040.rs"
  - "systemd/imag-power-envelope-alert-watchdog.service"
  - "systemd/imag-power-envelope-alert-watchdog.timer"
---

# imag-nb power/thermal envelope (#1040) — the MMIO RAPL PL1 clamp diagnosis + the shared-lib gate pattern

## The diagnosis (why this exists) — a HARDWARE power clamp, not software churn

The imag 60fps render regression (issues 799/880/1029/1030) was NOT churn/GC/scheduling — it was
`thermald`'s DPTF policy programming the **MMIO RAPL PL1 long-term constraint to 25 W**, starving
the iGPU to `gt_act_freq` 600-850 MHz while every SOFTWARE freq knob (`gt_min_freq_mhz`,
`slpc_ignore_eff_freq`, `slpc_ignore_eff_freq`) sat at 1400. **MMIO RAPL wins over the decorative
MSR RAPL values** (MSR showed 200/80 W while MMIO enforced 25). The sustainable envelope is **29 W**
(35 W overheats — TCPU 81→90 °C in 8 s; cooling cannot hold 35 W). Live diagnosis recipe, for the
next time render budget blows: read `/sys/class/powercap/intel-rapl-mmio:*/` (the `package-0` zone's
`long_term` constraint `power_limit_uw`), NOT the MSR `intel-rapl:*` path; a forcewake read of
`i915_frequency_info` shows `Actual` at half of `RPNSWREQ` when clamped. `thermald stop` alone does
NOT change the live MMIO value (the limit persists) — you must re-write PL1.

## The fix shape: purge thermald + a boot oneshot + a loud root guard, all sharing ONE lib

thermald is **PURGED, not masked** (`apt-get purge -y thermald`) — same appliance discipline the
sole-timesync-authority gate enforces (a competing policy engine gets purged; masking leaves its
opaque DPTF/GDDV surface to resurface across package upgrades). PROCHOT stays as the hardware
backstop. The envelope is pinned at boot (`imag-power-envelope.service`, root oneshot mirroring
`imag-igpu-maxperf.service`) and supervised at runtime (`imag-power-envelope-guard.timer`, root
~45 s). Alerting is **dev1-side** (`imag-power-envelope-alert-watchdog`, same topology as
`imag-obs-alert-watchdog` #882 — imag-nb has no airuleset checkout / Discord creds).

## Identity-based selection is mandatory (the presenter-drm cardN hazard, applied to RAPL/sysfs)

NEVER hardcode `intel-rapl-mmio:0` or a constraint index or `card0` or `thermal_zone0`:
- RAPL PL1: iterate `intel-rapl-mmio:*`, select the zone whose `name` reads `package-0`, then the
  constraint whose `constraint_N_name` reads `long_term` (the index varies — a `core` zone can have
  a `long_term` at index 0 that a naive first-match would wrongly pick; the test plants exactly that
  decoy).
- slpc: glob `/sys/class/drm/card*/gt/gt*/slpc_ignore_eff_freq`.
- TCPU: pick the `thermal_zone*` whose `type` reads `x86_pkg_temp`.

## The shared-lib pattern (mirrors scripts/lib/timesync-authority.sh)

`scripts/lib/imag-power-envelope.sh` is **source-only** (`# airuleset:script-ok` bypass on line 2,
NO `set -e`) and holds the REMOTE gather snippet + the PURE verdicts + the guard decision, SHARED by
`drift-guard.sh --check-imag`, `verify-imag.sh` check (u), and the on-box guard/oneshot (fetched to
`/usr/local/lib/imag-power-envelope.sh`). Never two driftable copies. Reuses the generic
`dpkg_status_installed`/`timesync_enabled_state_neutral` from `timesync-authority.sh` (both callers
already source it). The gather emits a `|`-delimited block (ZONE/CONSTRAINT/ENABLED/SLPC/THERMALD/
UNIT/TCPU/ACTFREQ); the verdict returns per-facet `<facet>|<STATUS>|<detail>` lines (pl1/slpc/
thermald/units, STATUS ∈ OK/DRIFT/UNKNOWN). Empty gather → UNKNOWN per facet, never a false DRIFT.
An **in-progress legitimate guard step-down reads as DRIFT — that is CORRECT** (a clamp is a
degradation; the [0/8] preflight refusing during a clamp episode is desired).

## Adding a check to drift-guard's `check_imag_report` (the backward-compatible optional-arg convention, #489/#596/#1040)

`check_imag_report` grows by APPENDING optional positional args (`local x="${13:-}"`), never
inserting — every existing 9/12-arg call site keeps working and defaults the new facet to UNKNOWN.
When you do this, the THREE "everything matches → clean" tests
(`check_imag_report_clean_when_every_value_matches_the_pinned_set_463`,
`..._end_to_end_from_a_realistic_imag_log_463`, `..._dantesync_lock_ok_when_locked_and_pinned_489`)
MUST each gain a CLEAN fixture for the new arg, or they regress from RC=0 to RC=11 (UNKNOWN). The
pinned value comes from `pinned_setting "$readme" <key>` (vendor/README.md); verify-imag reads the
SAME pin (via `imag_power_pl1_pin_from_readme_text`) so the two gates never check different values.

## The guard state machine: decision + streak bookkeeping are SEPARATE pure functions

`imag_power_guard_decision` (stepdown|restore|reassert|hold) and `imag_power_guard_next_streaks`
(the "2 consecutive" HOT/COOL/STEPPED ledger) are BOTH pure + unit-tested — do not inline the
streak update in the guard script (a future off-by-one would pass every decision test). Semantics:
stepdown needs the CURRENT read hot AND ≥1 prior consecutive hot (HOT_STREAK≥1 → this is the 2nd);
restore needs stepped-down + sustained cool; reassert fires ONLY when NOT stepped-down (it must
never fight the guard's own 25 W step-down — a foreign write while stepped-down self-heals on the
next restore; PROCHOT backstops); an unreadable TCPU → hold (never a blind step).

## setup-imag.sh: add the step at the END, never renumber (anchor-collision + TOTAL_STEPS)

`tests/setup_imag_guards.rs` pins ~113 literals AND `TOTAL_STEPS` must equal the actual `step N`
count. Add a new provisioning step at the END (step 22 here), bump `TOTAL_STEPS` and its one guard
assertion — do NOT insert into the middle (renumbering 17 steps would collide with the
`.find("step 20 \"...")` / `.find("step 17 \"...")` ordering tests). Env knobs bake into the ROOT
units via unquoted heredocs (`Environment=IMAG_PL1_W=${IMAG_PL1_W:-29}`); the timer heredoc is
single-quoted (no expansion). Root SYSTEM units (`/etc/systemd/system/`), not user units — sysfs
writes need root, unlike the user-level `imag-obs.service`.

## The i915 wedge-forensic surface (#849) — same sysfs, and what NOT to use

`imag-obs-watchdog.py`'s tier-b wedge `snapshot()` is now hardware-aware (#849): it reads local
`lspci -nn` once and branches (discrete NVIDIA → nvidia-smi forensics with a DERIVED PCI address,
never the old hardcoded `0000:01:00.0`; no-dGPU → the i915 surface). The i915 forensic surface is
the SAME sysfs this rule already documents — GLOB the card path (`/sys/class/drm/card*/gt/gt*/`,
never `card1`; the presenter-drm cardN-renumbering hazard): `rps_act_freq_mhz` (act << max = the
clamp signature), the `throttle_reason_*` set (`pl1`/`thermal`/`prochot`/`status`, `1`=active — the
same PL1-clamp discriminator this rule's guard keys on), RAPL-mmio `package-0`, `fuser /dev/dri`.

**`intel_gpu_top` is DELIBERATELY NOT used as a forensic surface on this box** — it is installed
(`/usr/bin/intel_gpu_top`, intel-gpu-tools **1.28**) but **core-dumps** in EVERY output mode
(`-c`/`-l`/`-J`, with or without `-o -`): `get_num_gts: Assertion '!errno || errno == ENOENT'
failed` (`../tools/intel_gpu_top.c:557`, exit 134). A `command -v intel_gpu_top` guard passes and it
STILL core-dumps, so only a live-works test justifies inclusion (the never-invent-by-analogy rule).
The working sysfs freq+throttle read gives the clamp/starvation signal instead; a genuine i915 hang
shows in the render-thread kernel stacks + `dmesg` (both generic, kept). The hardware detector is the
SAME `imag_has_discrete_nvidia` regex `setup-imag.sh` + `imag_scenes.py` share (a THIRD mirrored
copy in the watchdog, parity-tested — the deploy dirs differ so it can't be imported).

## The #880 throttle-under-floor alert — a SECOND, independent path keyed on throttle_reason

The guard's STEP-DOWN/RE-ASSERT journal alert only fires on a TCPU thermal excursion. But the
DOMINANT clamp under production load is the punit steering the iGPU below the pinned floor at the
MMIO RAPL PL1 **power** budget (act 500-750 MHz vs pinned 1400, `throttle_reason_pl1=1`) — which
produces NO guard step-down, so it was silent judder. `imag-power-envelope-alert-watchdog.sh` now
runs TWO independent alert paths (`alert_from_journal` + `alert_from_throttle`), each with its OWN
dedup state keys (`alert_sig`/`alert_passes` vs `throttle_sig`/`throttle_passes`); a quiet journal
must NOT short-circuit the throttle path (it lives only in the burst).

**Key on `throttle_reason`, NEVER raw `act_freq`.** `act < floor` alone false-fires on benign RC6
idle (the GPU parks low with no load — exactly the idle-sampling artifact issue 880's body flagged;
those samples carry `throttle_reason_status=0`). The clamp discriminator is
`(throttle_reason_pl1=1 OR throttle_reason_thermal=1) AND act < rps_min_freq_mhz`, over a MAJORITY
(`imag_power_throttle_alert_condition`, default ≥50 % via `IMAG_POWER_THROTTLE_ALERT_PCT`) of a
~6 s burst (`imag_power_throttle_burst_remote_snippet`, 12 samples @0.5 s) — majority-of-a-burst =
"sustained, not a transient single-frame dip". The burst is a SEPARATE remote snippet from the
instantaneous `imag_power_envelope_gather_remote_snippet` so drift-guard/verify-imag are never
slowed by a multi-second sample. Two guards learned the hard way (both unit-tested): require a
MIN sample count (`IMAG_POWER_THROTTLE_MIN_SAMPLES`, default 6) so a truncated ssh-dropped burst
(a 2/2 capture = 100 %) is not read as sustained; and the dedup signature must be a STABLE episode
token (`imag_power_throttle_alert_sig` → `imag-throttle:under-floor`), NEVER the fluctuating
`clamped/total` count — embedding the count makes each pass a "new" signature and re-pages every
5 min instead of once-then-suppress-for-~1h.

**The floor is unenforceable in software — do NOT try to "fix" the pin.** A forcewake
`i915_frequency_info` read proves every software knob (`Min/Max/Boost`) is ALREADY 1400 while
`Actual` runs at ~half; the punit legally overrides the request at the power/thermal envelope,
already pinned to the thermal max (29 W). The only remaining headroom is physical cooling (the
technician ticket, issue 1043) — the alert makes the throttling VISIBLE; it does not (and cannot)
raise the ceiling.

## The #799 render-degradation CAUSE discriminator — a THIRD alert path that NAMES the cause

The #880 throttle path above says "the iGPU is clamped". It does NOT say whether OBS render is
actually degrading, and there is a SECOND, distinct cause of the same "render budget blown after
hours, restart clears it" symptom: a **connection-churn render leak** (#799) where render time
creeps while the GPU has HEADROOM (throttle CLEAN, GPU idle-ish) — accumulated per-process state in
the NDI-receive→texture-upload path, cleared by an OBS restart. Without a discriminator a "render
degraded" alert is ambiguous: it could be the known power clamp (issue 880/1043, cooling is the fix)
or the churn leak (#799, a restart clears it). `alert_from_render_discriminator` (a third path in
`imag-power-envelope-alert-watchdog.sh`, own `render_*` dedup keys) FUSES two signals and names it.

- **The fusion table** (pure `imag_render_cause_from_signals RENDER_LINE BURST`, in the shared lib):
  render degraded + throttle **clean** → `churn-leak` (#799, PAGES); render degraded + throttle
  **clamped** → `power-clamp` (issue 880/1043, LOGGED not re-paged — `alert_from_throttle` already
  owns the clamp alert, never double-page); render degraded + throttle **unknown** → `unknown`
  (cannot attribute — never a false churn blame); render healthy/stalled/unknown → no page.
- **Read render over the OBS WS, NOT sysfs** — render stats (`activeFps`/`averageFrameRenderTime`/
  `renderSkipped`) have no sysfs equivalent. The dev1 front `scripts/imag-render-stats.py` (mirrors
  `obs-liveness-probe.py::_render_sample`, the render-signal source) does GetStats×2 and emits one
  `RENDER|<active_fps>|<avg_ms>|<render_skipped_frac>|<render_advanced>` line. It is BEST-EFFORT and
  fail-safe: any WS failure → empty line → discriminator `unknown` → no alert; the watchdog only
  attempts it when the box is reachable (BURST non-empty) and hard-bounds it (`timeout 15` +
  `OBS_OP_TIMEOUT_S=8`) so a hung WS never blows the timer's `TimeoutStartSec` (bumped 30→45).
- **`activeFps` LIES during a full stall (#935)** — trust it ONLY when `render_advanced=true`.
  `imag_render_degraded_from_sample` treats `advanced=false` as `stalled` (defers to the #391
  obs-liveness FpsZero path, no double-alert); `avg_ms`/`skip_frac` are trusted always; the fps<58
  signal only fires when advancement is CONFIRMED. Thresholds MIRROR `src/render_budget.rs` (60fps:
  budget 1000/60≈16.67ms, fps floor 58, skip 5%) — one source of the physical deadline.
- **The 3-state `imag_power_throttle_state` (clamped/clean/unknown) shares ONE burst-parse primitive
  (`_imag_power_throttle_parse_burst`) with the 2-state `imag_power_throttle_alert_condition`** — the
  2-state marker's exact output contract is preserved; only its internals moved onto the primitive.
  `clean` (GPU headroom) vs `unknown` (no FLOOR / < min samples) is the distinction the churn
  discriminator needs and the 2-state function cannot make (it collapses both into empty output).
- **A single WS window can catch a transient**, so the churn page is gated by `obs_watchdog_confirm`
  (2 consecutive `churn-leak` reads) before the shared `obs_watchdog_alert_throttle` dedup applies.
- **Live grounding (2026-08-16, 2d14h uptime):** the box read FLOOR=1400/ACT=750/pl1=1/TCPU=87C
  (power-clamped) while OBS render was HEALTHY (`RENDER|60.00|9.34|0.0000|true`, avg<budget). The
  discriminator correctly stayed quiet (render healthy → no churn page); the throttle path WOULD
  page the clamp. This is exactly why the fusion matters: it prevents a false #799 churn alert when
  the real cause is the known clamp.
