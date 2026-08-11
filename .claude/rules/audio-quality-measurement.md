---
paths:
  - "scripts/asrc-quality-bench/**"
  - "src/asrc_compensation_quantization.rs"
---

# Offline audio-quality (THD+N) measurement -- coherent sampling + CI-only-vendor harness pattern (#929)

## The window-leakage trap (cost real turns before it was found)

A THD+N/spectral measurement's OWN analysis window can BE the measured noise floor, masking
whatever real fidelity number you're trying to prove. First cut of `analyze_thdn.py` FFT'd an
arbitrary-length segment through a `np.blackman()` window and measured **-55 dB even on a literal
bypass (no resampler at all)** -- that number is Blackman's own first-sidelobe leakage floor
(~-58 dB), not a property of the signal. It looked plausible enough to almost ship as "the
resampler's artifact floor".

**Fix: coherent sampling.** If the test tone's frequency is an exact integer (997 Hz, the AES17
standard) and the sample rate is too (48000 Hz), ANY window length that is a WHOLE NUMBER OF
SECONDS contains an exact integer number of tone cycles -- a plain RECTANGULAR window over that
segment then has (for an unresampled/1:1 signal) essentially ZERO spectral leakage, because there
is no partial-cycle discontinuity at the window edges for the DFT to smear across bins. Coherence
does NOT require the window to start at zero phase -- only that its LENGTH is an integer multiple
of the tone's period. This flips the measured floor from -55 dB (an artifact of the analysis) to
-154 dB (`np.float64` FFT power-sum precision) on the exact same bypass case -- a ~100 dB swing
from a methodology fix alone, with zero change to the signal being measured.

**When this recurs:** any future audio-fidelity measurement in this repo (verifying issue-1016's
eventual servo-cadence fix, any other resampler/codec/DSP quality check) should pick a coherent
test tone + whole-second analysis window FIRST, before trusting any dB number the harness prints.
A -50-something dB floor that shows up identically across configs that should behave very
differently is the tell that you're measuring your own window, not the signal.

## Standalone-harness-outside-vendor-tree pattern (reusable for any CI-only vendor code)

`vendor/obs-studio/libobs` (and the rest of `vendor/**`) has NO local compile path on this box
(CI-only, project CLAUDE.md "Local Build Policy") -- so a real A/B of vendored libswresample
behavior could not link the vendor tree directly. Instead: link the box's OWN system
`libswresample-dev`/`libavutil-dev` (same library family, confirmed same AVOption defaults),
calling the EXACT SAME API sequence the vendor call site uses (same `swr_alloc_set_opts2` args,
including sample FORMAT -- `AV_SAMPLE_FMT_FLTP`, not `_FLT`; OBS's internal mix format is always
`AUDIO_FORMAT_FLOAT_PLANAR`, confirmed `obs.c:1626` -> `obs-source.c:4043`; for mono the two
formats are byte-identical, but match it anyway so the claim of "exact call shape" is literally
true, not just close enough). This is buildable/runnable on dev1 with a plain `gcc` + `pkg-config`
one-liner, entirely outside Cargo (no new runtime dep on the shipped binary) -- see
`scripts/asrc-quality-bench/asrc_ab_harness.c`'s header comment and `run_ab.sh` for the working
pattern. Reusable any time a ticket needs to measure/prove something about vendored C code this
repo cannot compile locally.

**Make every claimed fact reproducible FROM the committed harness, not just an out-of-band probe.**
A review caught that RESULTS.md's "what the library actually defaults to" table had no
corresponding code path in the harness itself -- the fact was true (independently re-verified) but
a future reader following "reproduce with `./run_ab.sh`" couldn't check it themselves. Fix: add a
`--mode dumpopts` that prints the AVOption values a freshly-built context actually holds (before
AND after `swr_init()`, since init can normalize/clamp some options for certain ratios). Any
harness that makes a "here's what the library actually does" claim should have a mode that PROVES
it on demand, not just a one-off developer-session printout copied into a doc.

## CPU-timing noise on a shared, loaded box

`CLOCK_PROCESS_CPUTIME_ID` per-block timing is noisy on dev1 when other sessions/worktrees are
compiling concurrently (measured a resampler-config CPU-cost multiplier move from ~513x to
~500-900x between single-run and load-matched interleaved measurements). Report CPU multipliers
as an order-of-magnitude range from an INTERLEAVED comparison (alternate configs within the same
few seconds, so both see the same momentary contention) rather than a single decimal-precise
figure from two runs taken minutes apart under different load -- the interleaving cancels most of
the noise even though the absolute numbers still vary.

## `swr_set_compensation()` REVERTS to 1:1 if not reissued before its own window elapses -- proven with `--max-reissues` (#1016)

Don't assume "ramps toward a target, then holds" means the achieved correction persists forever
once the ramp completes. It does NOT: issuing `swr_set_compensation(ctx, 14, 48000)` (a 300ppm-
derived delta over a nominal 1s window) exactly ONCE and then running the resampler for a FULL
20s with no further reissue measures `achieved_ppm=14.5833` over the WHOLE 20s -- i.e. the total
correction achieved is exactly the ONE ramp's own delta (14 samples) and NOTHING further accrues
after the window closes. Reproduce with `asrc_ab_harness.c`'s `--max-reissues N` flag (caps the
TOTAL reissue count, independent of `--reissue-every`'s per-block cadence):

```bash
./asrc_ab_harness --mode compensation --config default --ppm 300 --duration 20 --max-reissues 1
# achieved_ppm=14.5833, NOT ~292 -- proves the compensation reverts, doesn't hold
```

This rules out "only reissue when the target value changes" as a safe fix for the re-trigger-
cadence THD+N problem (issue 929/#1019): continuous compensation genuinely REQUIRES reissuing
before `distance_ms` elapses, so skipping a reissue because the value is unchanged would silently
stop correcting a sustained drift. It is ALSO not sufficient by itself even ignoring safety:
reissuing the exact SAME unchanged value every callback (a constant-ppm test, `sample_delta`
identical call to call) still costs the full -18.19dB THD+N issue 929 measured -- distortion is
not about the value CHANGING, it's inherent to calling `swr_set_compensation()` at all,
repeatedly. Before proposing any "smarter reissue schedule" fix for this class of problem, run
BOTH experiments (a single reissue over a long window; repeated reissue of an unchanging value)
through the harness first -- both are one-line invocations and either one alone can invalidate an
otherwise-plausible design.

## Widening `distance_ms` is a pure, stateless fix for integer-rounding quantization floors (#1016)

`swr_set_compensation()`'s integer sample-delta rounding (`round(ppm/1e6 * distance_samples)`,
`distance_samples = output_freq*distance_ms/1000`) floors any `|ppm|` under half a quantum
(`1e6/distance_samples` ppm) to a complete no-op. The achieved rate for values ABOVE that floor
is `round(ppm/1e6*distance_samples)/distance_samples*1e6` -- this depends ONLY on
`distance_samples`, not on how often the (unchanged) call is reissued, as long as reissue still
happens well inside the window. So widening `distance_ms` (finer quantum = lower floor) is a
correct, STATELESS fix requiring no cross-call accumulator state -- verified empirically at
several `distance_ms` values via `--distance-ms` (achieved_ppm matched the predicted formula
exactly every time, including an EXACT match with zero rounding error when `distance_samples` is
an integer multiple of `1e6/ppm`, e.g. 300ppm at `distance_ms=10000` -> `distance_samples=480000`
-> `delta=144` -> `achieved=300.0000` exactly). A cross-call fractional/delta-sigma accumulator
was considered and rejected as unnecessary for exactly this reason -- see issue 1016's design
comment for the full reasoning. **Gotcha it's easy to trip while writing this up:** don't
copy-paste an achieved-ppm value from one `distance_ms` column into another in a results table --
a review caught exactly this (the pre-fix 291.6667 value pasted into the post-fix column instead
of the correctly-recomputed 300.0000); always regenerate EACH cell from the harness, never assume
two columns share a value just because they're both "above the old floor".
