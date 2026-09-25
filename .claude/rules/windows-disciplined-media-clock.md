---
paths:
  - "vendor/obs-studio/libobs/util/platform-windows.c"
  - "vendor/obs-studio/libobs/util/windows/qpc-timestamp.h"
  - "vendor/obs-studio/plugins/win-wasapi/win-wasapi.cpp"
  - "vendor/obs-studio/plugins/obs-browser/browser-client.cpp"
  - "vendor/obs-studio/plugins/vlc-video/vlc-video-source.c"
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
    integrated every 50 ms: `10500/10679`, `11000/10970`, `11500/11107`, `10700/10749`… Over
    39 s the largest per-second deviation was 393 ns (≈ 0.4 ppm, FILETIME's 100 ns quantum plus
    the 50 ms sampling), with no bias. The sign and the 1:1 magnitude decide `inc/adj` over
    `adj/inc`.
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
- **Stamps taken on another clock are mapped by their AGE** (`util/windows/qpc-timestamp.h`):
  `disciplined_now − (clock_now − stamp)`, measured on the stamp's own clock.
  - Before issue 1372 these stamps sat on the raw-QPC `os_gettime_ns()` timeline by construction.
    Unmapped, they would drift ~72 ms/h at 20 ppm against the disciplined mixer. libobs keeps
    direct timestamps (`timing_adjust = 0`) below `MAX_TS_VAR` = 2 s, so nothing corrects that
    before it snaps.
  - The sources:
    - **win-wasapi** raw-QPC `qpcPosition`. Process capture always stamps with it; device capture
      does with `use_device_timing`, which is the Desktop Audio default.
    - **obs-browser** CEF audio pts: `base::TimeTicks` ms, QPC-based on Windows. The CEF docs say
      "ms since the Unix Epoch", but `libcef/browser/audio_capturer.cc` computes
      `audio_capture_time - base::TimeTicks()`.
    - **vlc-video** `libvlc_clock()`, whose age is measured on VLC's own clock (no QPC
      assumption).
  - A stamp more than `OS_FOREIGN_STAMP_MAX_AGE_NS` (60 s) from its clock's now is not on that clock
    (e.g. an epoch value) and passes through unchanged, as before issue 1372.
  - The raw-QPC variant brackets `os_gettime_ns()` with two counter reads and uses their midpoint,
    retried while they are more than 50 µs apart, so a preemption cannot skew the age.
  - It is header-only, so obs.dll gets no new export. Linux/macOS builds are untouched (`_WIN32`
    only): there, TimeTicks, VLC's clock and `os_gettime_ns()` are the same monotonic clock.

## The verification pattern: lift the Win32 glue and run it on FAKE Win32 layers

The file compiles only on the Windows runner, and dev1 has no mingw. `tests/os_clock_discipline_parity_1372.rs`
lifts the whole BEGIN…END block, `os_sleepto_ns` and the `qpc-timestamp.h` helpers VERBATIM,
and compiles them with `cc -Werror -Wconversion -Wformat=2` twice:

1. **A scripted fake** (typedefs, a scripted QPC, a fake adjustment handed out by the fake
   `GetProcAddress`, a `Sleep` that advances the counter). It runs scenarios read by read against
   the Rust authority, plus the sleep and WASAPI checks.
2. **A threaded fake:** pthreads, GCC atomics, `CLOCK_MONOTONIC` as a 10 MHz QPC running 1000×
   fast, the rate flipping ±1000 ppm on every 250 µs poll. Four threads assert that no read goes
   below a value another thread already returned. It runs until ≥ 1000 polls and ≥ 50k reads
   (at least 2 s, at most 10 s), so a slow runner takes longer instead of flaking.

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
  - 300 µs before 1 in 16 QPC samples;
  - the writer for 400 µs after its closing increment (before it releases the poller flag).

  - Why the stalls must be long: a stale read is only `Δrate × gap` ahead (0.2 % at 2000 ppm), so
    another thread must read within 0.2 % of the gap to see it.
  - With these stalls, both ordering mutations produce violations every run: the reader sampling
    after validation gave ~2700, the writer sampling before the odd increment 8–16. The correct
    code gives 0 in every run.
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

- **This needs a FULL bundle, not only the fast obs.dll.** The clock is in obs.dll, but the stamp
  mapping is in `win-wasapi.dll`, `obs-browser.dll` and `vlc-video.dll`. The rig uses them (live
  25.9.2026):
  - stream `Stream_Obs` has a `vlc_source` ("NL playlist") and two browser sources;
  - resolume `cg_scenes` has nine browser sources, six with `reroute_audio` (YouTube / VDO.Ninja
    audio into the cg mix);
  - neither has a WASAPI or DirectShow source.

  With obs.dll alone, those VLC and browser stamps drift against the disciplined mixer.
- The genlock audit's `wall_qpc_drift_ms=` (obs-source.c) goes FLAT on Windows apart from
  dantesync phase steps. It moved ~10 ppm before.
- **The stream `asrc: source 'mbc' estimated=` reading moves.** The mixer clock is now the
  system-time rate, so it reads about `−f_phase` again (the pre-#1325 relation), not the ≈ −6 ppm
  "Dante vs QPC crystal" value. That is the EXPECTED post-deploy state, not an obs.dll regression.
  Judge mbc by a flat `buffered_ms` (`asrc-residual-floor.md`).
- The cg VBAN frame-counter rate vs a SYSTEM-TIME-disciplined peer must be within ±1 ppm
  (issue 1372 part C). Against a Dante-clocked peer, expect `f_phase`.

## Known limit

DirectShow sources (win-dshow) stamp with the filter graph's reference-clock stream time, not a clock
OBS can read "now" on, so the age mapping does not apply. Such a source drifts vs the disciplined
mixer at the discipline rate, like any capture device whose clock differs from OBS's. There is none
on the rig.

Third-party plugins (the ASIO input on stream, `obs-vban` on resolume) are not vendored. If one
stamps with its own raw-QPC read instead of `os_gettime_ns()`, it drifts the same way. Check its
timestamps before relying on it.
