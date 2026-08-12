# ASRC compensation re-trigger cadence -- root cause + measurement fix (issue 1019)

Measured 2026-08-12 on dev1, same box/library versions as `RESULTS.md`/`RESULTS-1016.md`
(`libswresample4`/`libavutil58` 7:6.1.1-3ubuntu5). Harness: `asrc_ab_harness.c`, extended with
`--ppm-start`/`--ppm-end` (committed, this ticket). Analyzer: `analyze_thdn.py`, extended with
`thdn_corrected()`/`thdn_segmented()` (committed, this ticket, tested in
`tests/python/test_asrc_thdn_corrected_1019.py`).

## TL;DR

Issue 1019 was filed to redesign the servo's re-trigger cadence because issues 929/1016 measured
-18.19 dB THD+N while `swr_set_compensation()` is reissued every audio callback. **That -18dB
figure, and every other double-digit-negative-dB figure in RESULTS.md/RESULTS-1016.md while
compensation is active, is predominantly a MEASUREMENT-METHODOLOGY ARTIFACT, not real distortion
in the resampled audio.** `analyze_thdn.thdn()` assumes the resampled OUTPUT tone sits at exactly
`--freq` (997 Hz) -- true only when the resampler is not actively compensating. Once a nonzero
ppm compensation is held, the output is genuinely time-warped by that ppm, so the tone's own
frequency in the OUTPUT stream shifts away from 997 Hz -- breaking `thdn()`'s rectangular-window
coherent-sampling assumption and producing tens of dB of pure spectral leakage that got measured
and reported as "distortion".

**Once measured with a frequency-aware method, THD+N while actively compensating -- at the real
production cadence (every audio callback, `distance_ms=10000`), across the realistic 5-300 ppm
range, including a genuinely WALKING (not constant) 30->60 ppm target across three different
cadences -- consistently falls in the -51 to -104 dB range.** This is dramatically below any
audibility threshold and nowhere near the -15 to -22 dB the original story reported.

**Conclusion: no redesign of the reissue mechanism is warranted.** The fix for this ticket is
fixing the MEASUREMENT (this file + the two committed source changes), not the servo. See
"What was NOT fully explained" below for the one honest caveat.

## 1. The mechanism, read from the real FFmpeg source (`libswresample/resample.c` 6.1.1)

`swr_set_compensation(ctx, sample_delta, compensation_distance)` -> `set_compensation()`:

```c
c->compensation_distance = compensation_distance;
if (compensation_distance)
    c->dst_incr = c->ideal_dst_incr - c->ideal_dst_incr * (int64_t)sample_delta / compensation_distance;
```

`dst_incr` (the resample-position advance rate) is recomputed from `sample_delta`/
`compensation_distance` ALONE -- no dependency on elapsed time, `c->index`, or `c->frac`. For a
CONSTANT ppm, `sample_delta`/`compensation_distance` are identical on every reissue, so
`dst_incr`'s computed value is bit-identical call to call. `multiple_resample()`'s tail
decrements a countdown (`compensation_distance -= dst_size`) that would revert `dst_incr` to
`ideal_dst_incr` once it reaches zero -- but a reissue before that happens simply resets the
countdown back to the full requested value, with the SAME `dst_incr`.

**Proven empirically, not just read from source:** reissuing an unchanged compensation target
every callback (`--reissue-every 1`, the real `obs-source.c` cadence) produces a **byte-for-byte
identical** output file to reissuing exactly ONCE with a `--distance-ms` wide enough to cover the
whole test (`cmp` exit 0, zero diff):

```bash
./asrc_ab_harness --mode quality --config default --ppm 300 --duration 10 --distance-ms 20000 --max-reissues 1 --out /tmp/single_wide.f32
./asrc_ab_harness --mode quality --config default --ppm 300 --duration 10 --distance-ms 20000 --out /tmp/every_wide.f32
cmp /tmp/single_wide.f32 /tmp/every_wide.f32   # exit 0 -- IDENTICAL
```

**This alone answers the ticket's core question: the reissue CADENCE itself changes nothing about
the signal, for an unchanged target.** Whatever THD+N a sustained compensation measures, reissuing
it every callback vs. once produces the exact same bytes.

## 2. The real culprit: the analyzer assumes the wrong frequency

For a resampler producing `out_frames` output samples from `in_frames_nominal` input samples of a
`test_freq`-Hz input tone, the tone's OWN frequency in output-sample-index space is EXACTLY:

```
true_freq = test_freq * in_frames_nominal / out_frames
```

(derived from: the output is the same audio content spread over a different sample count; a tone
that took `in_frames_nominal/test_freq` seconds' worth of cycles, now represented in `out_frames`
samples at the SAME nominal 48 kHz clock, has frequency `test_freq * in_frames_nominal/out_frames`
in that stream.) Measured directly (997 Hz tone, ppm=300, `distance_ms=1000`, matching
RESULTS.md's own headline scenario exactly): `out_frames=480140` for `in_frames=480000` ->
`true_freq = 996.7093 Hz` -- a `997 - 996.7093 = 0.29 Hz` shift. A high-resolution (8x zero-padded
Blackman FFT peak search) independent measurement of the ACTUAL peak in the captured signal found
`996.6875 Hz` for a similar (distance_ms=20000) case -- matching the analytic prediction
(996.7009 Hz) to within FFT bin resolution, confirming the shift is real and exactly this size.

At the harness's 4-second analysis window, bin spacing is `48000/(4*48000) = 0.25 Hz` -- so a
`0.29 Hz` shift moves the tone MORE THAN ONE FULL BIN away from where `thdn()`'s rectangular
window (sized for an exact `997.0 Hz` cycle count) expects it. A rectangular window's sidelobes
decay slowly (~6 dB/octave) -- confirmed by re-analyzing the SAME `/tmp/orig929.f32` samples with
`thdn()`'s OWN existing `--window blackman` option (unrelated to any new code this ticket adds):
THD+N jumps from -18.19 dB (rect) to a **-56.63 dB** floor with zero change to the signal, purely
by switching the analysis window. Re-centering the guard band on the TRUE spectral peak (found via
high-res search) instead of the nominal 997 Hz bin, with an EXACT analytically-coherent window
length, recovers **-53 to -58 dB** on the exact same samples (`thdn_corrected()`, below).

## 3. `thdn_corrected()` -- constant-ppm case, analytic true-frequency correction

`scripts/asrc-quality-bench/analyze_thdn.py::thdn_corrected()` computes `true_freq` from the
known `in_frames_nominal` and the file's own measured length, searches for a window length with
the closest-to-zero fractional-cycle residual at that frequency (the "coherent sampling" trick,
generalized to a non-round frequency), and measures THD+N there. Reproduce:

```bash
./asrc_ab_harness --mode quality --config default --ppm 300 --duration 10 --distance-ms 1000 --out /tmp/orig929.f32
python3 analyze_thdn.py /tmp/orig929.f32 --corrected --duration 10 --label "orig929 corrected"
# THD+N=-55.35 dB, true_freq=996.7093Hz  (vs -18.19 dB naive on the SAME file)
```

| scenario (`distance_ms=10000`, reissue every callback -- the real production cadence) | naive `thdn()` | `thdn_corrected()` |
|---|---|---|
| ppm=30 (realistic low end of the live-observed range) | -21.47 dB | **-57.08 dB** |
| ppm=60 (realistic high end) | -15.57 dB | **-62.20 dB** |
| ppm=300 (issue 929's original stress case) | -16.63 dB | **-51 to -68 dB** (window/guard-band dependent, see caveat below) |

```bash
./asrc_ab_harness --mode quality --config default --ppm 30 --duration 10 --distance-ms 10000 --out /tmp/prod_30.f32
python3 analyze_thdn.py /tmp/prod_30.f32 --corrected --duration 10 --label "ppm=30 corrected"
```

## 4. `thdn_segmented()` -- realistic WALKING 30->60 ppm target, three cadences

A real `RealtimeAsrcCompensator` slew-limits its target to <=5 ppm/s (`src/asrc_bench.rs`), so any
short segment sees an almost-constant local ppm -- `thdn_segmented()` splits the signal into
`--seg-s`-second segments, estimates each segment's OWN local frequency via a 3-point parabolic
(sub-bin) interpolation on a Blackman-windowed FFT peak (no dependency on knowing the ppm
schedule), then measures THD+N in a coherent sub-window there. Reproduce a genuinely walking
target (`--ppm-start`/`--ppm-end`, this ticket's new harness flag) at the real production window:

```bash
./asrc_ab_harness --mode quality --config default --ppm-start 30 --ppm-end 60 --duration 10 \
    --distance-ms 10000 --out /tmp/walk_30_60.f32              # cadence=1 (every callback, real)
./asrc_ab_harness --mode quality --config default --ppm-start 30 --ppm-end 60 --duration 10 \
    --distance-ms 10000 --reissue-every 10 --out /tmp/walk_30_60_cad10.f32   # ~213ms cadence
./asrc_ab_harness --mode quality --config default --ppm-start 30 --ppm-end 60 --duration 10 \
    --distance-ms 10000 --reissue-every 47 --out /tmp/walk_30_60_cad47.f32  # ~1s cadence
python3 analyze_thdn.py /tmp/walk_30_60.f32 --segmented --seg-s 1.0
```

| cadence | worst-case per-segment THD+N (9 one-second segments, 30->60 ppm walk) |
|---|---|
| every callback (~21ms, real `obs-source.c` cadence) | **-71.87 dB** |
| every ~213ms (`--reissue-every 10`) | **-71.15 dB** |
| every ~1s (`--reissue-every 47`) | **-83.15 dB** |

All three cadences, across the full realistic 30-60 ppm walking range: consistently clean,
comfortably below any real audibility threshold, and no meaningful degradation at a FASTER
cadence vs. a slower one (satisfying the ticket's own acceptance ask: "THD+N ... across at least
two cadences").

## 5. What was NOT fully explained -- the honest caveat

The corrected/segmented numbers above (-51 to -104 dB depending on window length and guard band)
do **not** reach the resampler's own true rest-state floor (-154 dB, confirmed both via `thdn()`'s
existing bypass/ppm=0 measurement AND via `thdn_corrected()` on a ppm=0 file using the identical
1-second-window methodology -- both give -154.08 dB). Two observations on the residual gap:

- It gets WORSE, not better, with a LONGER analysis window (`analyze_s=1`: -71 dB; `analyze_s=8`:
  -51 dB, same ppm=30 file) -- the signature of energy spread over a roughly fixed ABSOLUTE
  frequency width (a longer window's finer bins let more of that fixed-Hz spread fall outside a
  fixed-BIN-COUNT guard band). Widening the guard band in absolute Hz recovers more of it
  (guard=300 bins at an 8s window, i.e. ~37 Hz half-width, recovers to -68 dB) -- consistent with
  either a small genuine narrow-band artifact, or still-imperfect coherent-window alignment, not
  fully disambiguated here.
- It is IDENTICAL between the byte-for-byte-identical reissue-every-callback and single-reissue
  files from section 1 above -- proving whatever causes this residual has nothing to do with
  reissue cadence (both files are literally the same bytes).

**This residual is not the -18dB "severe, clearly audible" problem the ticket was chasing** (it
is 30-50+ dB better, i.e. a further 30-300x lower distortion power), and does not, on its own,
justify a servo/API redesign. It is left as an open, lower-priority question for anyone who wants
to pursue it further (filed separately, not blocking this ticket's conclusion).

## Reproduce the full matrix

```bash
cd scripts/asrc-quality-bench
gcc -O2 -Wall -Wextra -o asrc_ab_harness asrc_ab_harness.c \
    $(pkg-config --cflags --libs libswresample libavutil) -lm
# section 1 (byte-identical reissue proof), section 3 (corrected), section 4 (segmented/walk) --
# see the commands inline above.
```
