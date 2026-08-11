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
    """Returns (thdn_db, thdn_pct, fundamental_dbfs, rms_full, freqs, power, fbin).

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
    fundamental_dbfs = 20.0 * np.log10(np.sqrt(fundamental_power) / (n * cg) * np.sqrt(2)) if fundamental_power > 0 else float("-inf")

    return thdn_db, thdn_pct, fundamental_dbfs, rms_full, freqs, power, fbin


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("path", help="raw f32le mono PCM file")
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--freq", type=float, default=997.0)
    ap.add_argument("--skip-s", type=float, default=1.0)
    ap.add_argument("--analyze-s", type=float, default=4.0)
    ap.add_argument("--window", default="rect", choices=["rect", "blackman"])
    ap.add_argument("--label", default="")
    args = ap.parse_args()

    samples = load_f32(args.path)
    thdn_db, thdn_pct, fund_dbfs, rms_full, freqs, power, fbin = thdn(
        samples, args.rate, args.freq, skip_s=args.skip_s, analyze_s=args.analyze_s, window_name=args.window)

    print(f"{args.label:40s} THD+N={thdn_db:7.2f} dB ({thdn_pct:10.6f} %)  fundamental~{fund_dbfs:6.1f} dBFS  "
          f"n_samples={len(samples)}")


if __name__ == "__main__":
    sys.exit(main())
