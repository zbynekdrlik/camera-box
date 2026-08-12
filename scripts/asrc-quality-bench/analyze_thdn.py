#!/usr/bin/env python3
"""camera-box #929 -- THD+N analyzer for the ASRC resampling-quality A/B harness.

Reads a raw f32le mono 48kHz PCM file produced by asrc_ab_harness --mode quality and computes
THD+N (Total Harmonic Distortion + Noise) against the AES17-style 997 Hz test tone, using the
standard notch-out-the-fundamental spectral method: FFT the (windowed) signal, sum all bin power
EXCEPT a narrow guard band around the fundamental, express that as a ratio to the fundamental's
own power.

This is a measurement tool, not part of the Rust build or CI -- run manually via run_ab.sh, its
printed numbers get pasted into the issue-929 review comment. No project runtime dependency is
introduced (numpy is already present in the dev1 environment used to run this).
"""
import sys
import argparse
import numpy as np


def load_f32(path):
    return np.fromfile(path, dtype=np.float32)


def thdn(samples, sample_rate, test_freq, skip_s=1.0, analyze_s=4.0, guard_bins=3, window_name="rect"):
    """Returns (thdn_db, thdn_pct, fundamental_rms_dbfs, rms_full, freqs, power, fbin).

    fundamental_rms_dbfs is the RMS level of the fundamental bin (NOT peak dBFS -- for the
    harness's -1 dBFS *peak* test tone this reads ~-4.0 dBFS, i.e. peak - 3.01 dB, the expected
    RMS-of-a-sinusoid relationship). It is a diagnostic-only field, not used in any RESULTS.md
    table.

    Uses COHERENT sampling: since test_freq=997 Hz is an exact integer and sample_rate=48000 Hz,
    any window length that is a WHOLE NUMBER OF SECONDS contains an exact integer number of tone
    cycles (997 * K, K=analyze_s seconds) -- so a plain rectangular window has (in the ppm=0,
    non-resampled case) essentially ZERO spectral leakage, unlike an arbitrary-length Blackman
    window whose ~-58 dB first-sidelobe floor would otherwise BE the measurement's noise floor
    (verified: an earlier arbitrary-length-Blackman version of this script measured -55 dB on a
    literal bypass/no-resampler case -- that was the window's own leakage floor, not a signal
    property). skip_s/analyze_s MUST both be whole seconds for this to hold; the caller enforces
    that by construction (asrc_ab_harness always runs whole-second durations).
    """
    if abs(skip_s - round(skip_s)) > 1e-9 or abs(analyze_s - round(analyze_s)) > 1e-9:
        raise ValueError("skip_s and analyze_s must be whole seconds for coherent sampling")
    skip = int(round(skip_s * sample_rate))
    n = int(round(analyze_s * sample_rate))
    if len(samples) < skip + n:
        raise ValueError(f"signal too short: have {len(samples)} samples, need {skip + n}")
    x = samples[skip:skip + n]

    if window_name == "rect":
        window = np.ones(n)
    elif window_name == "blackman":
        window = np.blackman(n)
    else:
        raise ValueError(f"unknown window {window_name}")
    xw = x * window
    cg = np.sum(window) / n  # coherent-gain correction

    spec = np.fft.rfft(xw)
    power = (np.abs(spec) ** 2)

    freqs = np.fft.rfftfreq(n, d=1.0 / sample_rate)
    bin_hz = sample_rate / n
    fbin = int(round(test_freq / bin_hz))

    lo = max(0, fbin - guard_bins)
    hi = min(len(power), fbin + guard_bins + 1)
    fundamental_power = float(np.sum(power[lo:hi]))

    total_power = float(np.sum(power[1:]))  # exclude DC bin
    noise_and_harmonic_power = total_power - fundamental_power
    if noise_and_harmonic_power < 0:
        noise_and_harmonic_power = 0.0

    thdn_ratio = np.sqrt(noise_and_harmonic_power / fundamental_power) if fundamental_power > 0 else float("inf")
    thdn_db = 20.0 * np.log10(thdn_ratio) if thdn_ratio > 0 else float("-inf")
    thdn_pct = thdn_ratio * 100.0

    rms_full = np.sqrt(np.mean(x.astype(np.float64) ** 2))
    fundamental_rms_dbfs = 20.0 * np.log10(np.sqrt(fundamental_power) / (n * cg) * np.sqrt(2)) if fundamental_power > 0 else float("-inf")

    return thdn_db, thdn_pct, fundamental_rms_dbfs, rms_full, freqs, power, fbin


def _find_coherent_n(true_freq, sample_rate, target_n, search_radius=4000, max_n=None):
    """Searches integer window lengths near `target_n` for the one containing the closest to a
    WHOLE number of cycles of `true_freq` -- the coherent-sampling analog of thdn()'s whole-second
    trick, but for a frequency that is NOT a round number (see thdn_corrected()'s doc comment).
    `max_n` hard-caps the search from ever growing PAST a caller-known boundary (thdn_segmented()
    passes its own segment length here -- a segment's analysis window must never read into the
    NEXT segment, which can hold a completely different frequency once the ppm target has moved
    on; searching only ever SHRINKS toward that cap, never grows past it).
    Returns (n, residual_fractional_cycles)."""
    best_n, best_frac = target_n, 1.0
    lo = max(1, target_n - search_radius)
    hi = target_n + search_radius
    if max_n is not None:
        hi = min(hi, max_n + 1)
        lo = min(lo, max_n)
    for n in range(lo, hi):
        cycles = n * true_freq / sample_rate
        frac = abs(cycles - round(cycles))
        if frac < best_frac:
            best_n, best_frac = n, frac
    return best_n, best_frac


def thdn_corrected(samples, sample_rate, test_freq, in_frames_nominal, skip_s=1.0, analyze_s=4.0, guard_bins=5):
    """THD+N for a signal actively resampled by a CONSTANT ppm compensation -- corrects the
    window-leakage trap thdn() does not know about (issue 1019).

    thdn() assumes the OUTPUT tone sits at exactly `test_freq` -- true only when the resampler
    is NOT actively compensating. Once `swr_set_compensation()` holds a nonzero ppm for the whole
    analysis window, the output is genuinely time-warped by that ppm (measured: for a resampler
    that emits `out_frames` output samples for `in_frames_nominal` input samples of a
    `test_freq`-Hz input tone, the tone's OWN frequency in output-sample-index space is
    `test_freq * in_frames_nominal / out_frames`, exactly, not `test_freq`) -- so a rectangular
    window sized for `test_freq`'s whole-cycle count is no longer coherent, and leaks tens of dB
    of energy across nearby bins. This is NOT audible distortion; it is thdn()'s own coherent-
    window assumption breaking. See RESULTS-1019.md for the empirical proof (a signal that is
    provably clean by the corrected method here still measures ~-16 to -22 dB under the naive
    thdn() once compensation is active, entirely explained by this effect: switching thdn()'s
    OWN window to `blackman` -- which does not depend on coherence -- independently recovers
    ~40 dB on the exact same samples with zero change to the signal).

    `in_frames_nominal` is the KNOWN nominal input length (duration_s * sample_rate) -- the exact
    analytic quantity the harness's own --duration implies; not a measurement, a fact about how
    the file was generated.

    Returns (thdn_db, thdn_pct, true_freq_hz, window_n).
    """
    out_frames = len(samples)
    true_freq = test_freq * in_frames_nominal / out_frames
    skip = int(round(skip_s * sample_rate))
    target_n = int(round(analyze_s * sample_rate))
    if len(samples) < skip + target_n:
        raise ValueError(f"signal too short: have {len(samples)} samples, need {skip + target_n}")
    n, _residual = _find_coherent_n(true_freq, sample_rate, target_n)
    if len(samples) < skip + n:
        n = target_n  # search overshot past EOF on a short file -- fall back to the nominal length
    x = samples[skip:skip + n].astype(np.float64)

    spec = np.fft.rfft(x)  # rectangular window -- valid now that the window IS coherent
    power = np.abs(spec) ** 2
    bin_hz = sample_rate / n
    fbin = int(round(true_freq / bin_hz))

    lo = max(0, fbin - guard_bins)
    hi = min(len(power), fbin + guard_bins + 1)
    fundamental_power = float(np.sum(power[lo:hi]))
    total_power = float(np.sum(power[1:]))
    noise_and_harmonic_power = max(total_power - fundamental_power, 0.0)

    thdn_ratio = np.sqrt(noise_and_harmonic_power / fundamental_power) if fundamental_power > 0 else float("inf")
    thdn_db = 20.0 * np.log10(thdn_ratio) if thdn_ratio > 0 else float("-inf")
    thdn_pct = thdn_ratio * 100.0
    return thdn_db, thdn_pct, true_freq, n


def _estimate_peak_freq(x, sample_rate, nominal_freq, search_hz=20.0, zero_pad=8):
    """High-resolution peak-frequency estimate near `nominal_freq`, via a Blackman-windowed FFT
    (Blackman's own leakage floor does not depend on coherence, so this works even when the true
    frequency is unknown -- used by thdn_segmented() for a WALKING ppm target, where no single
    global true_freq exists to compute analytically like thdn_corrected() does).

    A realistic ASRC ppm (single digits to a few hundred) shifts a 997 Hz tone by well under one
    FFT bin even at 8x zero-padding (e.g. 30 ppm -> ~0.03 Hz, vs an 8x-padded 1s window's own
    0.125 Hz bin spacing) -- a bare argmax cannot resolve that, it just returns the same discrete
    bin regardless. Bin-spacing zero-padding alone does not fix this; SUB-BIN precision needs
    3-point parabolic (quadratic) interpolation on the log-magnitude around the discrete peak
    (the standard DSP estimator -- see e.g. Jacobsen & Kay) -- applied here on TOP of the
    zero-padded spectrum for extra headroom."""
    n = len(x)
    xw = x * np.blackman(n)
    nfft = n * zero_pad
    spec = np.fft.rfft(xw, n=nfft)
    mag = np.abs(spec)
    freqs = np.fft.rfftfreq(nfft, d=1.0 / sample_rate)
    bin_hz = sample_rate / nfft
    half_span = int(round(search_hz / bin_hz))
    center = int(round(nominal_freq / bin_hz))
    lo = max(1, center - half_span)
    hi = min(len(mag) - 1, center + half_span + 1)
    k = lo + int(np.argmax(mag[lo:hi]))
    if k <= 0 or k >= len(mag) - 1 or mag[k] <= 0:
        return float(freqs[k])
    log_mag = np.log(mag[max(k - 1, 0):k + 2] + 1e-300)
    denom = log_mag[0] - 2.0 * log_mag[1] + log_mag[2]
    delta = 0.5 * (log_mag[0] - log_mag[2]) / denom if denom != 0 else 0.0
    delta = float(np.clip(delta, -1.0, 1.0))  # a well-formed peak has |delta| < 0.5; clip guards edge noise
    return float(freqs[k] + delta * bin_hz)


def thdn_segmented(samples, sample_rate, test_freq, seg_s=1.0, skip_s=1.0, n_segments=None, guard_bins=5,
                    search_hz=20.0):
    """Per-segment THD+N for a WALKING ppm compensation target (issue 1019's realistic ppm-program
    acceptance case) -- there is no single global true_freq to correct for like thdn_corrected()
    uses (the servo's target genuinely changes over time), so this instead: (1) estimates the
    LOCAL true frequency in each `seg_s`-second segment via a coherence-independent high-res peak
    search (_estimate_peak_freq), then (2) finds a coherent sub-window near that estimate and
    measures THD+N in it exactly like thdn_corrected() does, per segment.

    A real ASRC servo (RealtimeAsrcCompensator, src/asrc_bench.rs) slew-limits its target to
    <=5ppm/s, so any segment much shorter than a second sees an almost-constant local ppm -- this
    is what makes per-segment coherent analysis valid for a realistic walk, unlike a single
    whole-file window (which would need ONE true_freq for the entire signal).

    Returns a list of dicts, one per segment: {index, start_s, freq_hz, thdn_db, thdn_pct, n}.
    """
    skip = int(round(skip_s * sample_rate))
    seg_n_nominal = int(round(seg_s * sample_rate))
    available = len(samples) - skip
    max_segments = available // seg_n_nominal
    n_segments = max_segments if n_segments is None else min(n_segments, max_segments)

    results = []
    for i in range(n_segments):
        seg_start = skip + i * seg_n_nominal
        x_est = samples[seg_start:seg_start + seg_n_nominal].astype(np.float64)
        local_freq = _estimate_peak_freq(x_est, sample_rate, test_freq, search_hz=search_hz)

        # max_n=seg_n_nominal: NEVER read past this segment's own boundary -- the next segment
        # can hold a different frequency once a walking ppm target has moved on (issue 1019
        # regression: an unclamped search overshot into the next segment and picked up its
        # different content mid-window, costing ~10dB of spurious "distortion").
        n, _residual = _find_coherent_n(local_freq, sample_rate, seg_n_nominal, search_radius=seg_n_nominal // 4,
                                         max_n=min(seg_n_nominal, len(samples) - seg_start))
        x = samples[seg_start:seg_start + n].astype(np.float64)

        spec = np.fft.rfft(x)
        power = np.abs(spec) ** 2
        bin_hz = sample_rate / n
        fbin = int(round(local_freq / bin_hz))
        lo = max(0, fbin - guard_bins)
        hi = min(len(power), fbin + guard_bins + 1)
        fundamental_power = float(np.sum(power[lo:hi]))
        total_power = float(np.sum(power[1:]))
        noise_and_harmonic_power = max(total_power - fundamental_power, 0.0)
        ratio = np.sqrt(noise_and_harmonic_power / fundamental_power) if fundamental_power > 0 else float("inf")
        db = 20.0 * np.log10(ratio) if ratio > 0 else float("-inf")

        results.append({
            "index": i,
            "start_s": seg_start / sample_rate,
            "freq_hz": local_freq,
            "thdn_db": db,
            "thdn_pct": ratio * 100.0,
            "n": n,
        })
    return results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("path", help="raw f32le mono PCM file")
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--freq", type=float, default=997.0)
    ap.add_argument("--skip-s", type=float, default=1.0)
    ap.add_argument("--analyze-s", type=float, default=4.0)
    ap.add_argument("--window", default="rect", choices=["rect", "blackman"])
    ap.add_argument("--label", default="")
    ap.add_argument("--corrected", action="store_true",
                     help="issue 1019: use thdn_corrected() instead of the naive coherent-window "
                          "method -- required for any file generated with active (--ppm nonzero) "
                          "compensation; requires --duration (or --in-frames) to know the exact "
                          "nominal input length")
    ap.add_argument("--segmented", action="store_true",
                     help="issue 1019: use thdn_segmented() -- for a --ppm-start/--ppm-end walking-"
                          "ppm file, where no single global true_freq exists")
    ap.add_argument("--duration", type=float, default=None,
                     help="nominal input duration in seconds (in_frames_nominal = duration*rate) "
                          "-- used by --corrected")
    ap.add_argument("--in-frames", type=int, default=None,
                     help="explicit nominal input frame count -- overrides --duration for --corrected")
    ap.add_argument("--seg-s", type=float, default=1.0, help="--segmented: segment length in seconds")
    args = ap.parse_args()

    samples = load_f32(args.path)

    if args.segmented:
        segs = thdn_segmented(samples, args.rate, args.freq, seg_s=args.seg_s, skip_s=args.skip_s)
        worst_db = max(s["thdn_db"] for s in segs) if segs else float("-inf")
        for s in segs:
            print(f"{args.label:40s} seg={s['index']:3d} t={s['start_s']:6.2f}s freq={s['freq_hz']:9.4f}Hz "
                  f"THD+N={s['thdn_db']:7.2f} dB ({s['thdn_pct']:10.6f} %)  n={s['n']}")
        print(f"{args.label:40s} worst_segment_THD+N={worst_db:7.2f} dB  n_segments={len(segs)}")
        return 0

    if args.corrected:
        in_frames = args.in_frames
        if in_frames is None:
            if args.duration is None:
                raise SystemExit("--corrected requires --duration or --in-frames")
            in_frames = int(round(args.duration * args.rate))
        thdn_db, thdn_pct, true_freq, n = thdn_corrected(
            samples, args.rate, args.freq, in_frames, skip_s=args.skip_s, analyze_s=args.analyze_s)
        print(f"{args.label:40s} THD+N={thdn_db:7.2f} dB ({thdn_pct:10.6f} %)  true_freq={true_freq:9.4f}Hz  "
              f"n={n}  n_samples={len(samples)}")
        return 0

    thdn_db, thdn_pct, fund_rms_dbfs, rms_full, freqs, power, fbin = thdn(
        samples, args.rate, args.freq, skip_s=args.skip_s, analyze_s=args.analyze_s, window_name=args.window)

    print(f"{args.label:40s} THD+N={thdn_db:7.2f} dB ({thdn_pct:10.6f} %)  fundamental_rms~{fund_rms_dbfs:6.1f} dBFS  "
          f"n_samples={len(samples)}")


if __name__ == "__main__":
    sys.exit(main())
