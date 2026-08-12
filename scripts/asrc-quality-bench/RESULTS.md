# ASRC resampling quality -- measured A/B results (issue 929)

Measured 2026-08-11 on dev1 (Ubuntu, system `libswresample4`/`libavutil58` 7:6.1.1-3ubuntu5,
`libswresample` API 4.12.100). Harness + analyzer: `asrc_ab_harness.c` / `analyze_thdn.py` in this
directory. Reproduce with `./run_ab.sh`.

## 1. What the library actually defaults to (not what the issue assumed)

`audio_resampler_create()` calls `swr_alloc_set_opts2(..., 0, NULL)` -- no extra options, mono,
`AV_SAMPLE_FMT_FLTP` (OBS's internal mix format is always `AUDIO_FORMAT_FLOAT_PLANAR`). Reading
the resulting `SwrContext`'s AVOption values back (before AND after `swr_init()`) -- reproducible
directly from the committed harness via `./asrc_ab_harness --mode dumpopts --config default`, not
just an out-of-band probe:

```
filter_size=32  phase_shift=10 (1024 phases)  linear_interp=1  exact_rational=1  cutoff=0(auto)  filter_type=2(kaiser)
```

**`linear_interp` and `exact_rational` are ALREADY ON.** The issue's Context section claimed
`linear_interp=0`; that is incorrect for the FFmpeg/libswresample version family this vendored OBS
build links against (verified directly against the AVOption table, not assumed from memory).

`swr_set_compensation()` on a `resampler=soxr` context returns `-22` (rejected) -- confirmed
directly: soxr genuinely cannot back this servo's dynamic-compensation use case, exactly as the
issue said.

## 2. THD+N matrix (AES17-style 997 Hz tone, -1 dBFS, coherent whole-second FFT windows)

| config | filter_size | phase_shift | cutoff | THD+N @ 0 ppm (at rest) | THD+N @ 300 ppm (actively compensating, real reissue cadence) |
|---|---|---|---|---|---|
| bypass (no resampler at all) | -- | -- | -- | -154.08 dB | -154.08 dB |
| default (current vendor code) | 32 | 10 (1024 phases) | auto | -154.08 dB | -18.19 dB |
| maxq_moderate | 128 | 12 (4096 phases) | 0.95 | -154.08 dB | -18.19 dB |
| maxq_extreme | 512 | 18 (262144 phases) | 0.99 | -154.08 dB | -18.19 dB |

`-154 dB` is the harness's own measurement floor (double-precision FFT power sum on a coherently
windowed, whole-cycle-count segment) -- i.e. **the resampler is indistinguishable from a bit-exact
bypass at rest**, regardless of engine config. This is dramatically better than the issue's own
speculative "-85...-95 dB" estimate.

**All three engine configs measure IDENTICAL THD+N while compensating** (to 2 decimal places),
despite `maxq_extreme` having 16x the filter taps and 256x the polyphase resolution of `default`.
Engine quality is not the bottleneck once compensation is active.

## 3. Isolating the real cause of the -18 dB figure: reissue cadence, not engine quality

`config=default`, `ppm=300`, sweeping how often `swr_set_compensation()` is re-issued
(`--reissue-every N` blocks, 1 block = 1024 frames = ~21.3 ms):

| reissue every | THD+N |
|---|---|
| 1 block (real `obs-source.c` cadence -- every audio callback) | -18.19 dB |
| 4 blocks (~85 ms) | -18.19 dB |
| 47 blocks (~1 s, matches the `distance_ms=1000` window itself) | -18.32 dB |
| 469 blocks (once for the whole 10 s run -- a single clean ramp) | **-144.54 dB** (transparent) |

Re-issuing before an in-flight ramp completes (`swr_set_compensation` "replaces any still-pending
compensation on each call", per the OBS wrapper's own doc comment) is what produces the audible
distortion -- not the resampler's static filter quality. This is a servo re-trigger-cadence /
rounding problem, filed separately as **#1016** (out of scope for issue 929's own ask).
**Update:** #1016's quantization-rounding half is fixed; its re-trigger-cadence half is now
tracked as **#1019** with further empirical evidence -- see `RESULTS-1016.md` in this directory.

## 4. CPU cost (ns per 1024-frame block; a block's own real-time budget is ~21,333,333 ns)

`CLOCK_PROCESS_CPUTIME_ID` timing on a shared, loaded dev box is noisy run-to-run (this box was
carrying multiple concurrent `cargo`/CI-style builds while measuring) -- absolute ns values below
are ONE representative run; the table also gives a load-normalized multiplier from a SEPARATE
interleaved measurement (default and maxq_extreme alternated within the same few seconds, so both
see the same momentary system load, cancelling most of the noise):

| config | @ 0 ppm | @ 300 ppm | interleaved multiplier vs default @ 300 ppm |
|---|---|---|---|
| default | 760 ns (0.0036%) | 15,575 ns (0.073%) | 1x (baseline) |
| maxq_moderate | 725 ns (0.0034%) | 70,843 ns (0.33%) | ~4-5x |
| maxq_extreme | 783 ns (0.0037%) | 7,974,658 ns (**~37% of one block's real-time budget, per source**) | **~500-900x** (3 interleaved pairs: 17.4-18.3 ms vs 18.6-21.5 us) |

The exact multiplier moves with system load (interleaved runs under HIGHER contention measured
CLOSER to 900x, not lower) -- the number to trust is the ORDER OF MAGNITUDE (hundreds to
approaching a thousand times), not a single decimal-precise figure. That conclusion is robust
regardless of load: `maxq_extreme` is never remotely close to free.

## 5. Decision

**No change to `audio_resampler_create()`'s engine settings.** At rest, current defaults are
already measurement-floor-transparent. While compensating, `maxq_moderate`/`maxq_extreme` buy
ZERO measured THD+N improvement over `default` while costing multiple-times to several-hundred-times
more CPU per source (see caveat above). The real audible cost during compensation is the
re-trigger cadence in `audio_resampler_set_compensation_ppm()`'s caller, a different
function/subsystem -- see #1016.

This satisfies issue 929's own acceptance criterion (3): "if NOT warranted: the measurement
proving current settings are already transparent is the deliverable."

## Reproduce

```bash
cd scripts/asrc-quality-bench
gcc -O2 -Wall -Wextra -o asrc_ab_harness asrc_ab_harness.c \
    $(pkg-config --cflags --libs libswresample libavutil) -lm
./run_ab.sh
```
