#!/usr/bin/env python3
"""issue 1192 -- speech-arrival envelope correlation for scripts/lipsync-test-mode.sh.

WHY (issue 1174 forensics -> issue 1192): after `lipsync-test-mode.sh start` launches the mpv
playback on cam2, the HDMI->mic (mbc/Dante) audio SINK LOCK is flaky per audio-stream-start -- it
sometimes latches, sometimes does not (and can latch/unlatch spontaneously mid-run). When it does
not latch, the asset speech never physically reaches the mic chain, so a whole 5-minute recording
round captures perfect video with DEAD speech (SyncNet 16/16 UNMEASURABLE), discovered only ~20 min
later after a pull+chunk+measure on dev2. The host side is ALWAYS healthy in that state (PCM
RUNNING, hw_params OK, ELD valid, mode matched), so the lock is UNREADABLE from cam2 -- the only
reliable signal is a CONTENT check on what actually arrived: does the recorded mbc audio contain the
asset's speech?

Volumedetect is explicitly NOT the criterion (issue 1174): the mic chain runs AGC whose operating
point is pinned to the loud QPSK marker, so it pumps AMBIENT up to the universal ~-5.3 dBFS ceiling
even when the asset speech is entirely absent -- a level check false-passes. The sufficient signal
is a SHAPE match: the amplitude ENVELOPE of the recorded audio vs the LOCAL asset. Measured live
(issue 1174): ~0.22-0.35 when the speech never arrived (mic captures uncorrelated ambient),
0.976 when it did (envelope corr 0.976, SyncNet conf 6.4). A threshold of ~0.6 separates them
cleanly.

METHOD (the ticket's spec): decode both signals to 8 kHz mono, take a 20 ms RECTIFIED envelope
(mean of |sample| over each 20 ms window), then the NORMALIZED (mean-subtracted, i.e. Pearson)
correlation of the probe envelope against the asset envelope at EVERY asset-loop offset -- the mpv
playback loops the ~60 s asset with `--loop-file=inf`, so a ~15 s probe window starts at an
arbitrary (and possibly wrap-around) phase of the loop; the best offset is the real alignment and
its correlation is the arrival verdict.

PURE + CLI split (repo convention, e.g. av_sync_combine_offsets.py): the correlation math is pure
stdlib functions (rectified_envelope / pearson / best_loop_correlation), unit-tested offline with
synthetic fixtures in tests/python/test_lipsync_envelope_corr.py (no ffmpeg, no numpy, no network).
The thin CLI decodes the two files via ffmpeg to 8 kHz mono s16le and prints `corr=<value>` -- a
LOW corr is a valid MEASUREMENT (exit 0), never an error; the caller (lipsync-test-mode.sh) applies
the LIPSYNC_ARRIVAL_CORR_MIN threshold and drives the retry loop. Only a genuine failure (missing
file, ffmpeg/decode error, empty/too-short audio) exits nonzero.
"""
import argparse
import array
import math
import subprocess
import sys


def rectified_envelope(samples, sample_rate, win_ms=20):
    """The 20 ms RECTIFIED amplitude envelope: mean of |sample| over each consecutive
    ``win_ms`` window. ``samples`` is any iterable of ints/floats (raw PCM, one channel).
    Returns a list of floats, one per full window (a trailing partial window is dropped so
    every envelope point covers the same span). An empty/too-short input yields ``[]``.
    """
    win = int(round(sample_rate * win_ms / 1000.0))
    if win <= 0:
        raise ValueError(f"window size must be positive (sample_rate={sample_rate}, win_ms={win_ms})")
    data = list(samples)
    n_full = len(data) // win
    env = []
    for w in range(n_full):
        base = w * win
        acc = 0.0
        for i in range(base, base + win):
            v = data[i]
            acc += v if v >= 0 else -v
        env.append(acc / win)
    return env


def pearson(a, b):
    """Pearson correlation coefficient of two EQUAL-LENGTH sequences, in [-1.0, 1.0].
    Returns 0.0 when either sequence has zero variance (a flat window correlates with
    nothing) or is shorter than 2 -- never raises on degenerate input, so a silent stretch
    of the recording reads as "no correlation", never a crash.
    """
    n = len(a)
    if n != len(b):
        raise ValueError(f"pearson needs equal-length sequences, got {n} and {len(b)}")
    if n < 2:
        return 0.0
    ma = sum(a) / n
    mb = sum(b) / n
    num = 0.0
    va = 0.0
    vb = 0.0
    for i in range(n):
        da = a[i] - ma
        db = b[i] - mb
        num += da * db
        va += da * da
        vb += db * db
    if va <= 0.0 or vb <= 0.0:
        return 0.0
    return num / math.sqrt(va * vb)


def best_loop_correlation(probe_env, asset_env):
    """The best NORMALIZED (Pearson) correlation of ``probe_env`` against ``asset_env`` over
    EVERY asset-loop offset. The asset plays on an infinite loop, so the probe window aligns to
    some phase of the loop and may wrap across the loop boundary -- so the asset envelope is
    treated as circular: for each offset k in [0, len(asset_env)) the probe is compared against
    ``asset_env[(k+i) % len(asset_env)]``. Returns the max Pearson over all offsets.

    Requires a non-empty probe and asset. When the probe is LONGER than one asset loop (should
    not happen -- the probe window is ~15 s, the asset ~60 s), the asset is tiled to cover it so
    the comparison stays well-defined.

    Cost is O(asset_windows * probe_windows) pure Python (~2M inner ops for a 60 s asset / 15 s
    probe at 8 kHz / 20 ms envelopes ≈ 1-2 s per call) -- fine at the probe sizes this ships with;
    if a much larger asset/probe or a tighter cadence is ever used, this is the place to reach for
    a vectorized cross-correlation (an FFT-based one) instead.
    """
    p = len(probe_env)
    a = len(asset_env)
    if p == 0 or a == 0:
        raise ValueError(f"empty envelope (probe={p}, asset={a})")
    # Tile the asset to at least (offset span + probe length) so every circular window is a real
    # slice; one extra copy covers any wrap for p <= a, more copies cover a pathological p > a.
    reps = 2 + (p // a)
    tiled = asset_env * reps
    best = -1.0
    for k in range(a):
        window = tiled[k:k + p]
        c = pearson(probe_env, window)
        if c > best:
            best = c
    return best


def _decode_pcm(path, sample_rate, audio_map="", timeout_s=60):
    """Decode ``path`` to mono ``sample_rate`` Hz signed-16-bit little-endian PCM via ffmpeg and
    return a list of ints. ``audio_map`` (e.g. "0:a:1") selects a specific audio stream; empty
    lets ffmpeg pick the default. Raises RuntimeError on any ffmpeg failure / empty output.

    ``timeout_s`` bounds the decode -- this runs inside lipsync-test-mode.sh's arrival-verify loop
    on the rig, so a hung ffmpeg (a pathological/partial input) must FAIL LOUD (RuntimeError ->
    exit 2 -> the caller's retry, then fail-loud) rather than stall the whole rig op. A truncated
    probe from the moov-atom race errors fast on its own; the timeout is the belt for anything that
    would otherwise hang.
    """
    cmd = ["ffmpeg", "-v", "error", "-nostdin", "-i", path]
    if audio_map:
        cmd += ["-map", audio_map]
    cmd += ["-ac", "1", "-ar", str(sample_rate), "-f", "s16le", "-"]
    try:
        proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout_s)
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"ffmpeg timed out ({timeout_s}s) decoding {path} -- treating as undecodable")
    if proc.returncode != 0:
        raise RuntimeError(
            f"ffmpeg failed to decode {path} (rc={proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace').strip()}"
        )
    raw = proc.stdout
    if len(raw) < 2:
        raise RuntimeError(f"ffmpeg produced no audio samples for {path} (empty/too short)")
    if len(raw) % 2:
        raw = raw[:-1]  # drop a stray trailing byte so array('h') never raises
    samples = array.array("h")
    samples.frombytes(raw)
    if sys.byteorder != "little":
        samples.byteswap()
    return samples.tolist()


def correlate_files(probe_path, asset_path, sample_rate=8000, win_ms=20, audio_map=""):
    """Decode both files and return the best asset-loop envelope correlation. The ``audio_map``
    selects the probe recording's audio stream (the asset is a plain single-stream file)."""
    probe = _decode_pcm(probe_path, sample_rate, audio_map=audio_map)
    asset = _decode_pcm(asset_path, sample_rate)
    probe_env = rectified_envelope(probe, sample_rate, win_ms=win_ms)
    asset_env = rectified_envelope(asset, sample_rate, win_ms=win_ms)
    if not probe_env or not asset_env:
        raise RuntimeError(
            f"envelope empty after decode (probe_env={len(probe_env)}, asset_env={len(asset_env)}) "
            f"-- probe recording or asset too short for a {win_ms}ms window"
        )
    return best_loop_correlation(probe_env, asset_env)


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="issue 1192: speech-arrival envelope correlation (probe recording vs local "
        "asset). Prints `corr=<value>` on stdout; a low corr is a valid measurement (exit 0), "
        "only a genuine decode/IO error exits nonzero. The caller applies the threshold."
    )
    ap.add_argument("--probe", required=True, help="the pulled probe-recording file (its mbc audio)")
    ap.add_argument("--asset", required=True, help="the local lipsync asset file")
    ap.add_argument("--sample-rate", type=int, default=8000, help="decode/envelope rate (default 8000)")
    ap.add_argument("--win-ms", type=int, default=20, help="envelope window ms (default 20)")
    ap.add_argument(
        "--audio-map",
        default="",
        help="ffmpeg -map selector for the probe recording's audio stream (e.g. 0:a:1); "
        "empty = ffmpeg's default stream",
    )
    args = ap.parse_args(argv)
    try:
        corr = correlate_files(
            args.probe,
            args.asset,
            sample_rate=args.sample_rate,
            win_ms=args.win_ms,
            audio_map=args.audio_map,
        )
    except (RuntimeError, ValueError, OSError) as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2
    print(f"corr={corr:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
