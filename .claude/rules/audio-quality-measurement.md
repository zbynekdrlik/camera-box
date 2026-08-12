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

## ACTIVE compensation shifts the true output-domain tone frequency -- thdn()'s coherent window
## silently stops being coherent once that happens (#1019)

`thdn()` assumes the resampled OUTPUT tone sits at exactly `--freq` (997 Hz). That is true ONLY
when the resampler is NOT actively compensating. Once `swr_set_compensation()` holds a nonzero
ppm for the whole analysis window, the output is genuinely time-warped by that ppm -- for a
resampler producing `out_frames` output samples from `in_frames_nominal` input samples of a
`test_freq`-Hz tone, the tone's OWN frequency in output-sample-index space is EXACTLY
`test_freq * in_frames_nominal / out_frames`, not `test_freq`. A shift of even a fraction of an
FFT bin breaks `thdn()`'s rectangular-window coherent-sampling assumption, and a rectangular
window's sidelobes decay slowly (~6 dB/octave) -- this alone produced tens of dB of pure
measurement-artifact "distortion" that issues 929/1016 mismeasured as a real reissue-cadence
defect. **Before trusting ANY `thdn()` number on a file generated with nonzero `--ppm`, use
`analyze_thdn.py`'s `--corrected` (constant ppm) or `--segmented` (walking ppm) modes instead** --
both compute/estimate the TRUE output-domain frequency first. Cheap sanity check that costs zero
new code: re-run the SAME samples through `thdn()`'s own pre-existing `--window blackman` --
Blackman's leakage floor does not depend on coherence, so if switching windows alone recovers
tens of dB with no change to the signal, you are looking at this exact trap, not real distortion.
Full derivation, tables, and the byte-for-byte reissue-cadence-is-harmless proof:
`scripts/asrc-quality-bench/RESULTS-1019.md`.

**Reusable diagnostic: prove a reissue mechanism harmless by comparing raw bytes, not just
THD+N numbers.** `cmp`-ing the raw `.f32` output of "reissue an unchanged target every callback"
against "reissue it exactly once with a `--distance-ms` wide enough to cover the whole test"
gives `cmp` exit 0 (byte-for-byte identical) -- this is strictly stronger evidence than comparing
two THD+N numbers (which could coincidentally match while the underlying signal differs). Use
this pattern whenever isolating "does re-triggering X change anything" from "is X's steady-state
behavior itself lossy" -- they are separable questions and byte-diffing settles the first one
with zero measurement-methodology risk at all.

**Sub-bin frequency estimation for an UNKNOWN (walking) ppm needs parabolic interpolation, not
just zero-padding.** A realistic single-digit-to-tens ppm shifts a 997 Hz tone by well under one
FFT bin even at 8x zero-padding (e.g. 30 ppm -> ~0.03 Hz, vs an 8x-padded 1s window's own
0.125 Hz bin spacing) -- a bare `argmax` cannot resolve that; it returns the same discrete bin
regardless of how much you zero-pad. Fix: standard 3-point parabolic (quadratic) interpolation on
the LOG-magnitude around the discrete peak (Jacobsen & Kay), applied on top of the zero-padded
spectrum -- see `analyze_thdn._estimate_peak_freq()`. Verified empirically down to ppm=5
(0.005 Hz shift) on a clean synthetic tone: recovers the true frequency to ~1e-5 Hz.

**A per-segment coherent-window search MUST be hard-capped at the segment's own boundary.** An
unclamped `_find_coherent_n` search (the same helper `thdn_corrected()` uses safely for a
WHOLE-FILE constant-ppm window) can overshoot into the NEXT segment when analyzing a WALKING
target split into short segments -- the next segment can hold a genuinely different frequency
once the ppm target has moved on, and reading into it mid-window costs real (not measurement-
artifact) dB from cross-contamination. Pass `max_n=<this segment's own sample count>` -- see
`thdn_segmented()`'s call site and `_find_coherent_n`'s own `max_n` parameter.
