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
