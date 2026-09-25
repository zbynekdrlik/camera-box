---
paths:
  - "vendor/obs-studio/libobs/util/platform-windows.c"
  - "vendor/obs-studio/libobs/util/windows/qpc-timestamp.h"
  - "vendor/obs-studio/plugins/win-wasapi/win-wasapi.cpp"
  - "src/os_clock_discipline.rs"
  - "tests/os_clock_discipline_parity_1372.rs"
---

# The Windows OBS media clock runs at the dantesync-disciplined rate (issue 1372 part A)

libobs paces its audio thread, video thread, the ASRC servo and every output timestamp off
`os_gettime_ns()`. On Windows that used to be raw QPC. dantesync disciplines the SYSTEM time
(`SetSystemTimeAdjustmentPrecise`) and never touches QPC. So Windows OBS ran up to ~20 ppm off
every other disciplined box, and VBAN between the PCs slipped packets. Linux needs nothing: adjtimex
slews `CLOCK_MONOTONIC` too.

`os_gettime_ns()` in `vendor/obs-studio/libobs/util/platform-windows.c` (the
`camera-box issue 1372 BEGIN … END` block) now integrates QPC deltas at the current system-time
rate. The Tier-0 authority is `src/os_clock_discipline.rs`.

## What it follows, and what it does NOT

- It follows dantesync's **system-time RATE**: the PTP frequency plus the NTP phase slew. It
  matches every other dantesync-disciplined box (other Windows OBS, strih-lx, the hub).
- It does **NOT** follow dantesync's phase **steps**. Live on win-resolume, a −146 µs step landed
  inside a 40 s window; the rate matched 1:1 around it. So `os_gettime_ns` vs system time over a
  window with a step reads the step as "drift". Judge the rate per second, never across a step.
- It is **not** the Dante grandmaster's own clock. Something clocked by Dante itself (ASIO/DVS, a
  VB-Matrix on a Dante clock) still differs by dantesync's steady `f_phase` (the Dante-GM-vs-UTC
  offset, `asrc-residual-floor.md`). A "VBAN vs a Dante-clocked peer" check keeps that offset.
  Compare against a system-time-disciplined peer, or subtract `f_phase`.

## The API semantics — measured, not guessed

- **The rate is `inc / adj`, NOT `adj / inc`.** `GetSystemTimeAdjustmentPrecise(&adj, &inc,
  &disabled)`: a LARGER `adj` SLOWS the time-of-day clock.
  - Measured on win-resolume, 25.9.2026, as per-second sys−QPC ns vs the `inc/adj` prediction
    integrated every 50 ms: `10500/10679`, `11000/10970`, `11500/11107`, `10700/10749`… (39 s,
    each within the 100 ns FILETIME quantum + sampling jitter).
  - dantesync steers the same way: `new_adj = inc − ppm·freq/1e6` (its `src/clock/windows.rs`
    says "increasing adjustment slows the clock").
  - The MS docs only say "adjusted clock update frequency" and never give the direction.
- **Units are QPC counts, not 100 ns** (that is the legacy non-Precise API). `inc` equals the QPC
  frequency (10 000 000 on Windows 10+). The MS SetSystemTimeAdjustmentPrecise sample adjusts by
  `ppm · QPCfreq / 1e6`.
- **kernel32.dll does NOT export it.** `GetProcAddress(kernel32)` returns 0 live; kernelbase.dll
  has it. So it is resolved at runtime (`InitOnceExecuteOnce` + `GetProcAddress(kernelbase, …)`)
  and a static import is never used. Missing API, failed read or disabled adjustment → rate `1/1`
  = stock QPC behaviour.
- To re-measure on a box, use a PowerShell `Add-Type` probe with `DllImport("kernelbase.dll")`.
  Read QPC + `GetSystemTimePreciseAsFileTime` + the adjustment. Integrate `inc/adj − 1` over the
  QPC time and compare per second. Do not use `0UL` literals (PS 5.1 has no `UL` suffix); keep the
  loop inside the C#.

## Design invariants the gates pin

- `ns = base_ns + floor(floor(dqpc·1e9/freq) · num/den)`: integer only (`util_mul_div64`, exact
  128-bit on MSVC x64, no `_udiv128` overflow since the quotient is ≤ ~1.001 × raw ns).
- Rebase ONLY on a rate change, at the current value. The clock stays monotonic and loses < 1 ns
  per dantesync update.
- Poll every `freq / OS_CLK_POLL_DIV` counts (250 ms). One writer at a time (`os_clk_polling`
  CAS). The adjustment syscall is outside the critical window.
- **Readers take no lock and write no shared memory: a sequence counter.** The writer makes it odd,
  samples its rebase count, updates, and makes it even. A reader samples QPC BETWEEN two equal even
  sequence reads. So a reader never applies an old segment to a count taken after a rebase, and no
  thread reads the clock backwards.
  - The first design used an SRW lock (shared on the hot path). Review found that a waiting
    exclusive writer queues new shared readers behind a preempted one: an inversion onto the
    audio/render threads. The `os_sleepto_ns` spin loop also bounced the lock line.
  - A reader that finds the writer mid-window yields (`YieldProcessor`, then `SwitchToThread`
    every 64 spins) rather than spinning against a preempted writer.
- `|adj − inc|` is clamped to `inc / 1000` (1000 ppm; dantesync's `DRIFT_MAX_PPM` is 500).
- The first value equals the old raw-QPC ns (startup unchanged).
- **`os_sleepto_ns` waits on `os_gettime_ns()`, never on raw QPC counts.** The audio/video threads
  compute targets on the disciplined clock. A raw-count conversion drifts from it at the dantesync
  rate (~36 ms per hour).
- **Raw-QPC stamps are mapped by their AGE** (`util/windows/qpc-timestamp.h`,
  `os_raw_qpc_100ns_to_gettime_ns`): `disciplined_now − (raw_now − raw_stamp)`. WASAPI stamps its
  buffers with raw QPC `qpcPosition` (process capture always, device capture with
  `use_device_timing`, which defaults to true for Desktop Audio). `ts * 100` would drift about
  72 ms/h at 20 ppm. libobs keeps direct timestamps (`timing_adjust = 0`) below `MAX_TS_VAR` = 2 s,
  so nothing corrects that before it snaps. It is a header-only helper, so obs.dll gets no new
  export.

## The verification pattern: lift the Win32 glue and run it on FAKE Win32 layers

The file compiles only on the Windows runner, and dev1 has no mingw. `tests/os_clock_discipline_parity_1372.rs`
lifts the whole BEGIN…END block, `os_sleepto_ns` and the two `qpc-timestamp.h` helpers VERBATIM,
and compiles them with `cc -Werror -Wconversion -Wformat=2` twice:

1. **A scripted fake** (typedefs, a scripted QPC, a fake adjustment handed out by the fake
   `GetProcAddress`, a `Sleep` that advances the counter). It runs scenarios read by read against
   the Rust authority, plus the sleep and WASAPI checks.
2. **A threaded fake:** pthreads, GCC atomics, `CLOCK_MONOTONIC` as a 10 MHz QPC running 1000×
   fast, the rate flipping ±1000 ppm on every 250 µs poll. Four threads assert that no read goes
   below a value another thread already returned.

The Rust module is included with `#[path = "../src/os_clock_discipline.rs"]`, so the gate is
std-only and runs locally:

```bash
CARGO_MANIFEST_DIR=<wt> rustc --test --edition 2021 tests/os_clock_discipline_parity_1372.rs -o /tmp/t && /tmp/t
```

Gotchas:

- **A threaded monotonicity gate is BLIND without injected stalls.** The race windows are
  nanoseconds, and a backwards step is only `Δrate × gap` (0.2 % of the gap at 2000 ppm). With no
  stalls, moving the reader's QPC sample outside the validated window gave 0 violations in 1 s. The
  fake therefore stalls:
  - randomly around QPC reads;
  - the writer for 200 µs right after it samples its rebase count (a thread-local flag set by the
    fake adjustment call, which only the writer makes);
  - the writer for 50 µs after its closing increment.

  With these, both ordering mutations (reader sample after validation; writer sample before the odd
  increment) produce tens to hundreds of violations every run, and the correct code produces 0
  (5/5 runs).
- **Fake `FARPROC` is `void (*)(void)`.** gcc's `-Wcast-function-type` (in `-Wextra`) exempts that
  type, so the shipped cast compiles under `-Werror`.
- **Keep single-line anchors single-line.** `GetProcAddress(kernelbase, "GetSystemTimeAdjustmentPrecise")`
  and the `if ((seq & 1) == 0) { QueryPerformanceCounter(&count); *seg = os_clk_state;` window must
  not wrap. A wrap puts a space after `(` and the squished pwsh anchors stop matching.
- **Every mutation was watched going RED:** rate `adj/inc`, rebase dropped, poll every read, clamp
  dropped, init at 0, `disabled` ignored, `os_sleepto_ns` on raw counts (a conversion-clean variant;
  the stock one only fails to COMPILE under `-Wconversion`, which proves nothing about behaviour),
  and both sequence-ordering mutations.
- Generating the Rust test from a Python edit script: a `\n` inside a C string in a Rust `r#"…"#`
  block must be written as `\\n` in a non-raw Python string, or the C `printf` gets a literal line
  break (the `ci-testing-gotchas.md` raw-byte class).

## Live verification after deploy (supervisor)

- The clock is a libobs change, so the fast obs.dll path deploys it. The WASAPI mapping is in
  `win-wasapi.dll` and ships only with a FULL bundle. The rig's active collections (stream
  `Stream_Obs`, resolume `cg_scenes`) have no WASAPI or DirectShow source (checked 25.9.2026), so
  the fast path is complete for the rig.
- The genlock audit's `wall_qpc_drift_ms=` (obs-source.c) goes FLAT on Windows apart from
  dantesync phase steps. It moved ~10 ppm before.
- **The stream `asrc: source 'mbc' estimated=` reading moves.** The mixer clock is now the
  system-time rate, so it reads about `−f_phase` again (the pre-#1325 relation), not the ≈ −6 ppm
  "Dante vs QPC crystal" value. That is the EXPECTED post-deploy state, not an obs.dll regression.
  Judge mbc by a flat `buffered_ms` (`asrc-residual-floor.md`).
- The cg VBAN frame-counter rate vs a SYSTEM-TIME-disciplined peer must be within ±1 ppm
  (issue 1372 part C). Against a Dante-clocked peer, expect `f_phase`.

## Known limit

DirectShow sources (win-dshow) stamp with the filter graph's reference-clock stream time, not
absolute QPC, so the age mapping does not apply. Such a source drifts vs the disciplined mixer at
the discipline rate, like any capture device whose clock differs from OBS's. None is on the rig.
