---
paths:
  - "vendor/obs-studio/libobs/util/platform-windows.c"
  - "src/os_clock_discipline.rs"
  - "tests/os_clock_discipline_parity_1372.rs"
---

# The Windows OBS media clock follows the dantesync tick (issue 1372 part A)

libobs paces its audio thread, video thread, the ASRC servo and every output timestamp off
`os_gettime_ns()`. On Windows that used to be raw QPC. dantesync disciplines the SYSTEM time
(`SetSystemTimeAdjustmentPrecise`) and never touches QPC. So Windows OBS ran up to ~20 ppm off
the Dante network, and VBAN between the PCs slipped packets. Linux needs nothing: adjtimex slews
`CLOCK_MONOTONIC` too.

`os_gettime_ns()` in `vendor/obs-studio/libobs/util/platform-windows.c` (the
`camera-box issue 1372 BEGIN … END` block) now integrates QPC deltas at the current system-time
rate. The Tier-0 authority is `src/os_clock_discipline.rs`.

## The API semantics — measured, not guessed

- **The rate is `inc / adj`, NOT `adj / inc`.** `GetSystemTimeAdjustmentPrecise(&adj, &inc,
  &disabled)`: a LARGER `adj` SLOWS the time-of-day clock.
  - Measured 1:1 per second on win-resolume, 25.9.2026: sys−QPC ppm vs `(adj−inc)/inc` ppm read
    `+19.98/−19.10`, `+11.50/−11.80`, `+4.40/−3.40`.
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
  Read QPC + `GetSystemTimePreciseAsFileTime` + the adjustment once a second and print the per-second
  `sys−QPC ppm / (adj−inc)/inc ppm` pairs. Do not use `0UL` literals (PS 5.1 has no `UL` suffix);
  use `[uint64]0` or keep it inside the C#.

## Design invariants the parity gate pins

- `ns = base_ns + floor(floor(dqpc·1e9/freq) · num/den)`: integer only (`util_mul_div64`, exact
  128-bit on MSVC x64).
- Rebase ONLY on a rate change, at the current value. The clock stays monotonic and loses < 1 ns
  per dantesync update.
- Poll every `freq / OS_CLK_POLL_DIV` counts (250 ms). One poller at a time (interlocked flag);
  everyone else keeps the segment.
- The QPC read happens INSIDE the shared SRW lock, so a rebase can never land between a thread's
  counter read and the segment it applies: no backwards step across threads.
- `|adj − inc|` is clamped to `inc / 1000` (1000 ppm).
- The first value equals the old raw-QPC ns (startup unchanged).
- An NTP date step changes system TIME, not the rate, so it never reaches this clock.
- **`os_sleepto_ns` must wait on `os_gettime_ns()`, never on raw QPC counts.** The audio/video
  threads compute targets on the disciplined clock. A raw-count conversion drifts from it at the
  dantesync rate (~36 ms per hour) and they wake at the wrong time. `os_sleepto_ns_fast` already
  used `os_gettime_ns`.

## The verification pattern: lift the Win32-dependent glue and run it on a FAKE Win32 layer

The file compiles only on the Windows runner, and dev1 has no mingw. The glue (SRW locks, QPC,
`GetProcAddress`, `InitOnce`) is still testable. `tests/os_clock_discipline_parity_1372.rs`:

1. Lifts the whole BEGIN…END block plus `os_sleepto_ns` VERBATIM.
2. Prepends a fake `windows.h`: typedefs, a scripted `QueryPerformanceCounter`, a `Sleep` that
   advances the fake counter, SRW locks that count pairing errors, and a fake adjustment function
   handed out by the fake `GetProcAddress`.
3. Compiles it with `cc -Werror -Wconversion -Wformat=2` and runs scenarios, requiring
   read-by-read equality with the Rust authority.

The Rust module is included with `#[path = "../src/os_clock_discipline.rs"]`, so the gate is
std-only and runs locally:

```bash
CARGO_MANIFEST_DIR=<wt> rustc --test --edition 2021 tests/os_clock_discipline_parity_1372.rs -o /tmp/t && /tmp/t
```

Gotchas:

- **Fake `FARPROC` is `void (*)(void)`.** gcc's `-Wcast-function-type` (in `-Wextra`) exempts
  that type, so the shipped `(get_system_time_adjustment_precise_t)proc` cast compiles under
  `-Werror`.
- **Keep the `GetProcAddress(kernelbase, "GetSystemTimeAdjustmentPrecise")` call on ONE line.** A
  wrap puts a space after `(`, and the squished pwsh anchor stops matching.
- **Every mutation was watched going RED** (rate flipped to `adj/inc`, rebase dropped, poll every
  read, clamp dropped, init at 0, `disabled` ignored, `os_sleepto_ns` back on raw QPC counts). A
  mutation that only makes the harness fail to COMPILE (the stock `os_sleepto_ns` trips
  `-Wconversion`) proves nothing about behaviour. Write a conversion-clean variant of the old logic
  so the sleep test is shown to bite on behaviour.

## Live verification after deploy (supervisor)

It is a libobs-only change, so the fast obs.dll path deploys it.

- The genlock audit's `wall_qpc_drift_ms=` term (obs-source.c, the wall-vs-monotonic drift) must
  go FLAT on Windows boxes. It moved ~10 ppm before. Only dantesync phase steps remain as jumps.
- The cg VBAN frame-counter rate vs a Dante-clocked stream must be within ±1 ppm (issue 1372 part
  C).

## Known limit

A plugin that stamps raw QPC now drifts vs `os_gettime_ns` at the discipline ratio. This is the
same class as today's device-clock drift, and libobs rebases source timing.

- Example: win-wasapi `useDeviceTiming`, and its process-capture path.
- The rig's Windows audio arrives via ASIO / NDI / VBAN, not device-timed WASAPI.
