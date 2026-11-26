---
paths:
  - "scripts/recording-verdict-on-strih-snv.sh"
  - "tests/recording_verdict_on_strih_lx_lowprio.rs"
  - "scripts/recording-verdict-on-stream-snv.sh"
---

**It is the ONE Linux on-box decode (issue 1462).** The Linux stream box stream-snv decodes its
`[8/8b]` stream recording through it: `scripts/recording-verdict-on-stream-snv.sh` resolves + checks
the box and `exec`s this script with `STRIH_LX_BOX`/`STRIH_USER`/`STRIH_PW` = the stream box,
`ONBOX_LABEL=stream-snv` (every log line's box name; default `strih-snv`, so strih's output is
unchanged) and `--rec <path>` (the box-neutral alias of `--strih-rec`). Never copy its sha256 gate,
LOWPRIO_SNIPPET or pull-back into another per-box script: add an entry point the same way
(`.claude/rules/stream-box-linux.md`).

# strih-snv in-place decode runs at idle priority on the E-cores (issue 1351 item / issue 1354)

**The Windows sibling refuses the Linux strih (issue 1317 part 3).** `recording-verdict-on-strih.sh`
(the Windows PowerShell planner) no longer defaults `STRIH_BOX` to 10.77.9.202: `STRIH_BOX` is
required and a target `strih_platform` resolves to `linux` is refused via the shared
`strih_platform_refuse_windows_only_mode` (scripts/lib/strih-platform.sh), pointing here.
`recording-e2e.sh` exports `STRIH_BOX="$STRIH"` so its Windows branch (reached only under
`STRIH_PLATFORM=windows`) names the box explicitly. The opt-in `ZERO_LOSS_RESTART_GATE` mode — the
only caller that still plans the Windows decode unconditionally — is refused on a Linux strih by
`strih_zero_loss_restart_preflight "$STRIH" || exit 2`, called right where recording-e2e.sh resolves
the strih, BEFORE the first rig-mutation guard (never after the preflight + a 360 s capture). Porting
that mode to strih-snv is not ticketed yet — it was returned to the supervisor as a follow-up candidate
in the issue-1317 part-3 LANE-RETURN.

`scripts/recording-verdict-on-strih-snv.sh` is the Linux sibling of `recording-verdict-on-strih.sh`:
at `[8/8a]` it ssh-runs `recording-verdict --extract-partial strih …` ON strih-snv (10.77.9.202)
against the recording where it already lives, then scps back only the small partial JSON (+ the
`<partial>-pixels` dir). Nothing is copied off-box.

**Since issue 1387 the prefix ends in the baseline's background runner when the box has it and sudo
allows it:** `sudo -n /usr/local/sbin/obs-box-background-run` = `rig-background.slice` on the CPUs OBS
does not use. The `nice` + E-core text below stays only for a box not yet provisioned (AMD has no
`cpu_atom`, so it fenced nothing there). `.claude/rules/obs-box-baseline.md` has the slice.

**On a box with housekeeping CPUs the decode leaves the slice for the OBS mask at idle priority
(issue 1425, finding 7178516822).** There the slice is the ONE housekeeping task CPU (strih-snv 0),
which also carries rig-NIC IRQs (design 7119179275): measured live, the decode ran on CPU 0 alone
(~1000 frames / 6 min, a 9443-frame recording ~55 min) beside the RX softirq. So the snippet's LAST
clause, when `/etc/strih-housekeeping-cpus.conf` is non-empty (setup-strih writes it only for a fact
other than `none`) and the first line of `/etc/strih-isolated-cpus.conf` is a cpulist (`N` or `N-M`,
comma separated, ASCII digits; `grep -Eqx`; the SHAPE only), sets `LP="nice -n 19 ionice -c 3 chrt
--idle 0 taskset -c <top core>"`, SCHED_IDLE (it runs only when one of those CPUs would idle).
**The top core, never the whole mask (finding 7187104711, 26.11.2026):** the first cut pinned the
whole OBS mask (strih-snv 4-15); live, OBS's dlink pump then woke late (late_blocks ~66/min,
late_to_mix 1 -> 14, wake_delay_peak 78 ms) for exactly the decode's 8 minutes: SCHED_IDLE yields run
time, not the SMT sibling, L2/L3 or memory bandwidth. So the snippet takes the HIGHEST CPU number of
the mask (a pure-shell max over the cpulist's numbers) and pins it plus its SMT siblings, read on the
box from `/sys/devices/system/cpu/cpu<N>/topology/thread_siblings_list` (strih-snv 4-15 -> `14-15`,
stream-snv 2-11 -> `10-11`, the P-core pair; a `7,15` comma form passes through); a siblings value that
is not a cpulist (absent, a non-SMT box) = the single highest CPU. **Expected cost:** ~2 logical CPUs
(~1.25 physical-core equivalents) instead of 12; the decode took ~8 min on the 12-CPU mask and ~55 min
on one busy housekeeping CPU, so an estimated 35-45 min, the longest step of the 115-min E2E job
(strih `[8/8a]` and stream `[8/8b]` run in parallel, so the longer one counts). Read the decode's wall
time and the dlink `late_blocks` / `late_to_mix` over its window at the next E2E. The box prints the prefix it resolved
(`[<box>] launch prefix: ...` on stderr): read it in the E2E log. An empty or malformed mask keeps the
runner / E-core / nice choice; a box without the file keeps today's prefix byte-identical. The pure
`strih_lx_lowprio_prefix` takes the same branch first (3rd argument = an etc root, test seam). stream-snv
execs this script and has housekeeping CPUs, so it gets the same placement; the issue-1367 pixel export
(the byte-pinned twin) too. `tests/recording_verdict_on_strih_lx_lowprio.rs` runs the SHIPPED snippet
with its box paths moved into a fixture and pins it to the pure function. Expected effect (not measured
yet): decode_workers follows the 2-CPU cpuset; read the decode's `Cpus_allowed_list` (= the top
core) and `chrt -p` (SCHED_IDLE) on the box at the next E2E.

Accepted trade-offs (review of the issue-1425 lane, design comment 7178556621; narrowed to one core by
design 7187142302):
- SCHED_IDLE decides who gets a CPU, not the SMT sibling, L3 or memory bandwidth: the decode still
  shares ONE physical core (and L3) with OBS (strih-snv 14-15, stream-snv 10-11; stream-snv's E-cores
  12-19 are housekeeping CPUs and stay out). Issue 1354 measured ~800 relocks per camera from a decode at
  nice 0 on the OBS cores; the top-core SCHED_IDLE placement is not measured yet. The blocking whole-session gate (issue 1534)
  reads the logs after the decode, so a decode that disturbs OBS turns the E2E red by itself. Read the
  genlock-fifo audit relock / underrun deltas over the `[8/8a]` / `[8/8b]` window at the first runs.
- The slice's `CPUWeight=1` / `IOWeight=1` / `MemoryHigh=30%` no longer apply to this path (the
  pre-1387 decode ran without them too); `decode_workers` stays bounded by `MemAvailable`.

## Why STEP 2 must be low-priority + E-core-pinned

The decode is a multi-core QR/pixel sweep over a 1080p60 5-min recording. Run at nice 0, unpinned,
it competes with OBS's `ndir:video` receiver decode threads on the P-cores, so during the
`[7/8]` StopRecord + `[8/8a]` decode window **every live strih-snv NDI input relock-storms for
minutes** (measured live, issue 1354: cam1 +763, cam4 +799, cam5 +832, cam7 +782 relocks vs 0
during the recording itself). It is verdict-neutral (the recording is already closed) but it leaves
every FIFO in a relock regime that the next run's `[4j/8settle]` then has to wait out — and it is
exactly the failure a bulk job on the strih notebook would cause during a production.

## The shape (pure helper + remote-shell replica)

- `strih_lx_lowprio_prefix <sysfs_root>` is the **canonical, pure, network-free encoding**: always
  `nice -n 19`; plus `taskset -c <range>` onto the Intel hybrid E-cores when
  `<root>/devices/cpu_atom/cpus` is a non-empty range; else `nice -n 19` alone (never a bare
  `taskset -c ""`). It reads only the passed root, so a unit test points it at a fake sysfs tree.
  strih-snv (measured 22.9.2026): 16 CPUs, `cpu_atom` = E-cores `12-15`, `cpu_core` = P-cores `0-11`.
- **STEP 2 in `main()` is the REMOTE-SHELL REPLICA of that contract, not a call to the helper** — the
  E-core range must be resolved ON the box (a replacement notebook has its own core map; dev1 has no
  `cpu_atom`). So `LOWPRIO_SNIPPET` is **single-quoted on the dev1 side** (`LP="nice -n 19"; if [ -s
  /sys/devices/cpu_atom/cpus ]; then LP="$LP taskset -c $(cat …)"; fi;`), so dev1 never expands the
  `$(cat …)` — the strih-snv shell does — and `\$LP` (escaped) prefixes the on-box command inside a
  brace group so `mkdir -p` still gates the decode (the `&&` binds the whole group, not just the
  `LP=` assignment the snippet starts with):
  `ssh … "mkdir -p '$OUT_DIR' && { $LOWPRIO_SNIPPET \$LP $ONSTRIHLX_CMD; }"`.
- A THIRD copy lives in `scripts/lib/missing-slot-pixels.sh` (`missing_slot_pixels_lowprio_snippet`,
  the issue-1367 pixel-proof export on strih-snv); `tests/harness_missing_slot_pixels_1367.rs` pins it
  byte-identical to `LOWPRIO_SNIPPET` here, so change both together.
- This is the repo's usual "pure decision + shell replica pinned by a test" pattern: the helper is
  what `tests/recording_verdict_on_strih_lx_lowprio.rs` pins (fixture roots), and a static anchor
  pins that STEP 2's ssh line launches `$ONSTRIHLX_CMD` through the prefix. Keep the two in parity.

## Gotchas

- The binary `scp` and the partial/pixels pull-back stay **byte-identical** — the change is confined
  to STEP 2's launch prefix.
- `$(cat …)` strips the sysfs file's trailing newline (design note); the helper also `tr -d`s
  whitespace so a whitespace-only file falls back to `nice`-only. The remote replica guards on
  `[ -s … ]` instead, so helper and replica agree on the only two states kernel sysfs actually
  emits — absent (non-hybrid → `nice -n 19`) and a clean range (hybrid → `nice -n 19 taskset -c
  <range>`, verified live 12-15) — and diverge only on a whitespace-only `cpus` file, which sysfs
  never produces (the helper's whitespace fallback is a unit-test-only defensive path).
- The only test that touches this script is `harness_strih_platform_1351.rs` (all `.contains()`
  substring anchors — a new helper/comment cannot break a `.find()`/`.split()` slice here), plus the
  new lowprio test. Run the occurrence-count anchor sweep after any edit anyway.
- Acceptance is a supervisor rig step: the next full E2E showing **zero relocks on strih-snv during
  `[8/8a]`**.

## Router entry (moved from CLAUDE.md, issue 1552)

- strih-snv in-place recording decode (`[8/8a]` `recording-verdict-on-strih-snv.sh`) at IDLE priority on the E-cores (`strih_lx_lowprio_prefix` pure encoding + the single-quoted remote-shell replica that resolves `/sys/devices/cpu_atom/cpus` ON the box; the issue-1354 post-run relock-storm root cause; binary scp + partial pull byte-identical; issue 1351) → `.claude/rules/recording-verdict-on-strih-snv.md` (auto-loads on its `paths:`)
