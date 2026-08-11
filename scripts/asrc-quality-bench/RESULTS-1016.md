# ASRC compensation quantization + re-trigger cadence -- measured results (issue 1016)

Measured 2026-08-11 on dev1, same box/library versions as `RESULTS.md` (issue 929). Harness:
`asrc_ab_harness.c`, extended with `--distance-ms` and `--max-reissues` (both committed).
Reproduce with the commands below, or extend `run_ab.sh`'s existing matrix with `--distance-ms
10000` where it currently omits the flag (defaults to the pre-#1016 1000ms).

## 1. The quantization-no-op fix -- widening `distance_ms` from 1000 to 10000

`--mode compensation` sweep, `distance_ms=10000` (the post-#1016 real caller value,
`ASRC_COMPENSATION_DISTANCE_MS` in `obs-source.c`), 20 s runs:

| requested ppm | achieved ppm (distance_ms=10000) | achieved ppm (distance_ms=1000, pre-fix, RESULTS.md) |
|---|---|---|
| 1 | 0.0000 | 0.0000 |
| 2 | 2.0833 | 0.0000 |
| 5 | 4.1667 | 0.0000 |
| 8 | 8.3333 | 0.0000 |
| 50 | 50.0000 (exact) | 41.6667 |
| 300 | 291.6667 (unchanged -- 300ppm was already above the OLD floor too) | 291.6667 |

The new zero-effect floor is `~1.0417 ppm` (down from `~10.4167 ppm`), covering essentially all
of issue 929's own "typically single-digit ppm" characterization. `achieved_ppm` matches the
predicted `round(ppm/1e6*distance_samples)/distance_samples*1e6` value exactly at every tested
point -- this is a pure, stateless quantization-resolution improvement; no cross-call state was
added (see issue 1016's design comment for why a fractional/delta-sigma accumulator was
considered and rejected as unnecessary).

```bash
./asrc_ab_harness --mode compensation --config default --ppm 5 --duration 20 --distance-ms 10000
```

## 2. Trade-off: newly-active small-ppm compensation is now audibly (mildly) lossy

Before this fix, small ppm was a complete no-op -- transparent audio, because the resampler
never actually adjusted anything. After this fix, it DOES adjust, at the cost of the same
re-trigger-cadence distortion mechanism issue 929 already measured for large ppm (see part 3
below and issue 1019):

| scenario | THD+N |
|---|---|
| ppm=5, distance_ms=1000 (pre-fix, no-op) | -144.70 dB (transparent, but ZERO correction) |
| ppm=5, distance_ms=10000 (this fix, real correction) | **-38.82 dB** (~1.15%, audible but mild) |
| ppm=300, distance_ms=1000 (pre-existing, unaffected by this fix) | -18.19 dB (~12.3%, severe) |
| ppm=300, distance_ms=10000 (same reissue cadence, wider window) | -16.63 dB (~14.7%, same order) |

Widening the window does NOT meaningfully change high-ppm THD+N (still cadence-bound, not
window-bound) -- the -18.19 -> -16.63 dB shift is a small, bounded side effect of the wider
window itself, not a new problem. The genuinely NEW trade-off is that small-ppm compensation goes
from "silent because inert" to "audible but mild (-38.8dB) because it now actually corrects" --
disclosed and accepted on issue 1016's design comment.

```bash
./asrc_ab_harness --mode quality --config default --ppm 5 --duration 10 --distance-ms 10000 --out /tmp/x.f32
python3 analyze_thdn.py /tmp/x.f32
```

## 3. Why the re-trigger cadence (problem 2, issue 1019) was NOT fixed in the same PR

Two new experiments, both against the real system libswresample, using the new `--max-reissues`
flag:

**Reissuing the SAME unchanged value still costs -18dB.** issue 929's original -18.19dB
measurement used a CONSTANT ppm=300 the whole 10s run, so `sample_delta` never actually differs
call to call (always rounds to 14) -- confirming distortion is not about the VALUE changing, it
is inherent to calling `swr_set_compensation()` at all, repeatedly.

**Skipping reissue is unsafe -- the compensation REVERTS.** A single `swr_set_compensation(ctx,
14, 48000)` call at the very start of a 20s run (ppm=300, distance_ms=1000) achieves
`achieved_ppm=14.5833` over the WHOLE 20s -- i.e. exactly the one ramp's own delta (14 samples)
and NOTHING further in the remaining ~19s:

```bash
./asrc_ab_harness --mode compensation --config default --ppm 300 --duration 20 --max-reissues 1
# achieved_ppm=14.5833  (not ~292, proving the ramp reverts to 1:1 once its own
#                         distance_samples window elapses with no further reissue)
```

This rules out "only reissue when the target changes" as a sufficient fix on its own: the
mechanism genuinely needs to be reissued before its own window closes to sustain ANY continuous
correction, and issue 929's own sweep already showed reissuing even at the natural ~1s period
(`reissue_every=47`, matching `distance_ms=1000`) still measures -18.32dB -- almost as bad as
every-callback reissue. Only reissuing ONCE for an entire run reaches -144.5dB. There is no free
lunch with the current API-usage pattern; this genuinely needs the architecture-level redesign
issue 1016 itself asked for, split into #1019.
