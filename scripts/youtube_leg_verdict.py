#!/usr/bin/env python3
"""Issue 1404 -- the YouTube-leg verdict: does the YouTube VOD show what stream OBS sent?

Each measured window compares the YouTube VOD of a run against stream OBS's own program recording of
the same run. Both files carry the cam2 painter tick (60 Hz, dual-QR Vernier) on every frame and the
cam2 QPSK marker in the audio, so everything is compared by CONTENT (painter tick), never by a shared
clock. Ported from the session tools of issue 1404 (comments 6006986090, 6008636005); the parts:
  youtube_leg_ticks.py     painter tick of every frame (both QR halves, gray then blue, by phase)
  youtube_leg_timeline.py  multi-part join, window clamp, coverage, dup/skip, publish joins
  youtube_leg_audio.py     block cross-correlation audio continuity
  this file                A/V via `recording-verdict --av-sync`, the verdict, the CLI

Criteria (docs/superpowers/specs/2026-10-06-youtube-leg-e2e-design.md, the PASS bar):
  1. A/V: per window VOD - recording `recording-verdict --av-sync` offset within +/-150 ms of the
     run's first measured window (YouTube's own fixed term varies per session: the bar is relative).
  2. 0 downstream dup/skip by painter tick (a tick the rig itself repeated cancels out): adjacent
     decoded VOD frames, plus the frame-count balance between ticks both files show unambiguously, so
     content lost or repeated behind undecodable VOD frames is counted too. A VOD that ends before
     the recording's last decoded frame of the window FAILS; a VOD stretch over 1 s that decodes
     nothing where the recording decodes, too many unjudged pairs, or over 2 s of the window without
     an anchor pair is UNKNOWN.
  3. The first VOD frame after every (re)publish is within 0.5 s of the publish, in content time.
     A publish with no VOD frame before it opened the VOD (YouTube starts the VOD at its own live
     transition, 24-42 s after the first publish in the sessions): its join is reported, not judged.
  4. Audio continuous: 0.25 s blocks, no lag jump > 1.5 ms, no low-correlation block (< 0.6) while
     the recording has signal, no silent VOD block, no level drop > 10 dB, no foreign VOD sound where
     the recording is quiet; VOD audio that ends early FAILS; a window whose recording carries too
     little audio signal to judge is UNKNOWN.
  5. Coverage: a window under 90 % cadence-proven frames (either file), one the VOD covers under half
     of, one outside the recording, one with a publish or a painter restart inside: UNKNOWN.
Overall PASS only when every window passes every criterion; a tool/decode/download error or a
timeout is UNKNOWN; a proven FAIL anywhere wins over an UNKNOWN.

CLI (shared with restreamer's release gate):
  youtube_leg_verdict.py --vod <youtube id | local file> --recording <file>[@<record-start-utc>] ...
                         --markers <cam2 qpsk marker csv | http url> --windows <name:start:end> ...
                         --publish <utc> ... [--unpublish <utc> ...] --out <dir>
                         [--probe-bin <recording-verdict>]
  exit 0 = PASS, 1 = FAIL, 2 = UNKNOWN; writes <dir>/youtube-leg-verdict.json.
  Timestamps: epoch seconds, ISO 8601 (2026-10-06T01:40:15.901Z) or compact (20261006T014015.901Z).
  More than one --recording = parts of one session split by an OBS restart; a part without @start is
  placed after the previous one by painter tick. A window must lie inside one publish span: a
  --publish inside it, or an --unpublish (StopStream) inside it or under 1 s after it, is UNKNOWN.
  `--decode-ticks <file>` (diagnostic) prints a file's per-frame tick map and exits.
  `--runs 911016 [--clip-markers <clip>.markers.csv]` (issue 1404 Task 5 part b): also read the
  measurement clip's tick (the CG segments). Every window is then judged on its ONE run segment (a
  window holding a cut between runs is UNKNOWN), and a clip window's A/V pairs the clip's own tick
  and marker (`recording-verdict --av-run 911016` with the clip's marker log). Without `--runs` the
  decode, the cache and the verdict are exactly what they were (restreamer's gate).
"""
import argparse
import datetime
import json
import os
import re
import shutil
import sys
import tempfile
import traceback
import urllib.request

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from youtube_leg_audio import (FOREIGN_DB, LAG_JUMP, LEVEL_DROP_DB, LOW_CORR, SILENT_VOD_DBFS,  # noqa: E402
                               SR, audio_blocks, audio_window, dbfs, drop_samples, load_audio,
                               ncc_best)
from youtube_leg_proc import install_cleanup, run_bounded  # noqa: E402
from youtube_leg_ticks import (CLIP_RUNS, DECODE_SCALE, DECODER_VERSION, PHASE_RADIUS, _qr_tick,  # noqa: E402
                               band_ticks, container_frames, decode_raw, decode_ticks, half_ticks,
                               half_ticks_run, load_raw, load_run_ticks, load_ticks, painter_payload,
                               painter_tick, resolve_ticks, write_ticks)
from youtube_leg_timeline import (DETAIL_LIMIT, END_SLACK_S, RunTimeline, TickClock,  # noqa: E402
                                  carries_runs, clamp_window, continuity, coverage, dupskip,
                                  join_part_rows, painter_restarts, publish_gaps, run_segments,
                                  run_timeline, timestamp_gaps, vod_content_times, vod_pts_for)

__all__ = ["SR", "run_bounded", "audio_blocks", "audio_window", "dbfs", "drop_samples", "load_audio", "ncc_best",
           "CLIP_RUNS", "DECODE_SCALE", "DECODER_VERSION", "PHASE_RADIUS", "_qr_tick", "band_ticks", "decode_raw",
           "decode_ticks", "half_ticks", "half_ticks_run", "load_raw", "load_run_ticks", "load_ticks",
           "painter_payload", "painter_tick", "resolve_ticks", "write_ticks", "container_frames", "timestamp_gaps",
           "DETAIL_LIMIT", "END_SLACK_S", "RunTimeline", "TickClock", "carries_runs", "clamp_window",
           "continuity", "coverage", "dupskip", "join_part_rows", "join_parts", "painter_restarts",
           "publish_gaps", "run_segments", "run_timeline", "vod_content_times", "vod_pts_for",
           "parse_avsync_output", "av_from_outputs", "av_window", "verdict", "parse_utc", "parse_window",
           "parse_recording", "fmt_utc", "main"]

SCHEMA = 1
EXIT_PASS, EXIT_FAIL, EXIT_UNKNOWN = 0, 1, 2

AV_TOLERANCE_MS = 150.0
AV_CLIP_S = 40.0
AV_MIN_MARKER_FRACTION = 0.5  # of the clip's expected markers (one per 0.5 s)
PUBLISH_GAP_MAX_S = 0.5
CADENCE_MIN_PCT = 90.0
MIN_WINDOW_COVERED = 0.5  # a window the VOD covers under half of is UNKNOWN
UNJUDGED_MAX = 5  # more unjudged adjacent VOD pairs than this is UNKNOWN (clean real windows: 0)
AUDIO_END_SLACK_S = 0.5
AUDIO_MIN_MEASURED = 0.9  # of the expected blocks
AUDIO_MIN_SIGNAL = 0.25  # of the measured blocks must carry recording signal (the only judged ones)
VOD_BLIND_MAX_S = 0.1  # more VOD-only undecodable runs (2+ frames, black / slate / a flash) is UNKNOWN
STOP_MARGIN_S = 1.0  # a window must end this long before a StopStream (the VOD loops its last frames)
CLIP_TIMEOUT_S, PROBE_TIMEOUT_S, YTDLP_TIMEOUT_S, URL_TIMEOUT_S = 600, 1800, 3600, 60


def join_parts(parts):
    """parts = [(tick-map path, record start | None), ...] -> (joined rows, t0)."""
    rows, t0, _ = join_part_rows([load_ticks(p) for p, _ in parts], [s for _, s in parts])
    return rows, t0


# ---------------------------------------------------------------- A/V (recording-verdict --av-sync)

def parse_avsync_output(text):
    """The JSON block of `recording-verdict --av-sync` output (log lines, then a line that is exactly
    `{`, the JSON, then more log text)."""
    lines = text.splitlines(keepends=True)
    for n, line in enumerate(lines):
        if line.rstrip("\r\n") == "{":
            j, _ = json.JSONDecoder().raw_decode("".join(lines[n:]))
            return j
    raise ValueError("no JSON block in the recording-verdict --av-sync output")


def _cut_clip(src, start_s, dur_s, out):
    run_bounded(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-ss", f"{start_s:.3f}",
                 "-i", src, "-t", f"{dur_s:.3f}", "-map", "0:v:0", "-map", "0:a:0",
                 "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-c:a", "aac", "-b:a", "192k", out],
                CLIP_TIMEOUT_S, check=True)


def _avsync(probe_bin, clip, markers_csv, av_run=None):
    """`recording-verdict --av-sync` on one clip; `av_run` = pair through that self-marked run's own
    QR tick + marker (the measurement clip: `--av-run 911016`) instead of the cam2 painter's."""
    cmd = [probe_bin, "--stream", clip, "--av-sync", clip, "--av-marker-log", markers_csv]
    if av_run is not None:
        cmd += ["--av-run", str(av_run)]
    r = run_bounded(cmd, PROBE_TIMEOUT_S, text=True)
    text = r.stderr + r.stdout
    with open(f"{clip}.avsync.out", "w") as f:
        f.write(text)
    if r.returncode != 0:
        raise RuntimeError(f"recording-verdict --av-sync exited {r.returncode} on {clip}")
    return parse_avsync_output(text)


def av_from_outputs(rec_j, vod_j, dur_s=AV_CLIP_S):
    """The window's A/V block from the two parsed --av-sync results (sign: video - audio)."""
    floor = AV_MIN_MARKER_FRACTION * dur_s * 2
    for name, j in (("recording", rec_j), ("VOD", vod_j)):
        if j.get("av_offset_ms") is None:
            raise ValueError(f"{name}: no A/V offset measured")
        if (j.get("matched") or 0) < floor:
            raise ValueError(f"{name}: only {j.get('matched')} markers matched (< {floor:.0f})")
    return {"rec_ms": round(rec_j["av_offset_ms"], 1), "vod_ms": round(vod_j["av_offset_ms"], 1),
            "delta_ms": round(vod_j["av_offset_ms"] - rec_j["av_offset_ms"], 1),
            "markers_rec": rec_j.get("matched"), "markers_vod": vod_j.get("matched"),
            "mad_rec_ms": round(rec_j.get("mad_ms") or 0.0, 1), "mad_vod_ms": round(vod_j.get("mad_ms") or 0.0, 1)}


def av_window(rec_file, vod_file, rec_start_s, vod_start_s, markers_csv, probe_bin, dur_s=AV_CLIP_S, workdir=None,
              av_run=None):
    """Cut the same content from both files (tick-matched starts) and measure each clip's A/V. The
    clips are deleted afterwards; each clip's probe output stays next to it as *.avsync.out.
    `av_run`: a CG window's self-marked run (its clips are paired through their own tick + marker)."""
    if workdir is None:
        with tempfile.TemporaryDirectory(prefix="ylv-av-") as tmp:
            return av_window(rec_file, vod_file, rec_start_s, vod_start_s, markers_csv, probe_bin, dur_s, tmp,
                             av_run)
    results = []
    for kind, src, start in (("rec", rec_file, rec_start_s), ("vod", vod_file, vod_start_s)):
        clip = os.path.join(workdir, f"av-{kind}-{start:.3f}.mp4")
        try:
            _cut_clip(src, start, dur_s, clip)
            results.append(_avsync(probe_bin, clip, markers_csv, av_run))
        finally:
            if os.path.exists(clip):
                os.remove(clip)
    return av_from_outputs(results[0], results[1], dur_s)


# ---------------------------------------------------------------- verdict

def _audio_findings(name, au):
    """(kind, message) list for one window's audio block."""
    keys = ("lag_jumps", "low_corr", "silent", "level_drops", "foreign")
    if (au.get("vod_ends_early_s") or 0) > AUDIO_END_SLACK_S:
        return [("FAIL", f"{name}: the VOD audio ends {au['vod_ends_early_s']:.1f} s before the window end")]
    if any(au.get(k) is None for k in keys):
        return [("UNKNOWN", f"{name}: audio not measured ({au.get('error', 'missing')})")]
    blocks, expected, signal = au.get("blocks", 0), au.get("expected_blocks"), au.get("signal_blocks")
    if signal is not None and signal < AUDIO_MIN_SIGNAL * blocks:
        # too little recording signal to judge: its counts (foreign sound above all) mean nothing
        return [("UNKNOWN", f"{name}: the recording carries audio signal in only {signal} of {blocks} blocks")]
    out = []
    if any(au[k] for k in keys):
        out.append(("FAIL", f"{name}: audio " + ", ".join(f"{k} {au[k]}" for k in keys if au[k])))
    if (au.get("rec_ends_early_s") or 0) > AUDIO_END_SLACK_S:
        out.append(("UNKNOWN", f"{name}: the recording audio ends {au['rec_ends_early_s']:.1f} s before the window end"))
    if expected is not None and blocks < AUDIO_MIN_MEASURED * expected:
        out.append(("UNKNOWN", f"{name}: audio measured over {blocks} of {expected} blocks"))
    return out


def verdict(windows, publishes):
    """{overall: PASS|FAIL|UNKNOWN, reasons, criteria}. A proven FAIL anywhere wins over an UNKNOWN."""
    fails, unknowns = [], []
    status = {k: "PASS" for k in ("av", "dupskip", "publish", "audio", "coverage")}

    def bad(crit, msg, kind):
        (fails if kind == "FAIL" else unknowns).append(msg)
        if status[crit] != "FAIL":
            status[crit] = kind

    if not windows:
        bad("coverage", "no window measured", "UNKNOWN")
    ref = next((w["av"]["delta_ms"] for w in windows if (w.get("av") or {}).get("delta_ms") is not None), None)
    for n, w in enumerate(windows):
        name = w.get("name", f"window {n + 1}")
        for e in w.get("errors") or ():
            bad("coverage", f"{name}: {e}", "UNKNOWN")
        ds = w.get("dupskip") or {}
        if ds.get("dup") is None or ds.get("skip") is None:
            if not w.get("errors"):
                bad("dupskip", f"{name}: dup/skip not measured ({ds.get('error', 'missing')})", "UNKNOWN")
            continue  # nothing of this window was measured
        if ds["dup"] or ds["skip"]:
            bad("dupskip", f"{name}: {ds['dup']} downstream dup / {ds['skip']} skip", "FAIL")
        if (ds.get("vod_ends_early_s") or 0) > END_SLACK_S:
            bad("dupskip", f"{name}: the VOD ends {ds['vod_ends_early_s']:.1f} s before the recording's "
                           "last decoded frame of the window", "FAIL")
        if (ds.get("vod_blind_s") or 0) > VOD_BLIND_MAX_S:
            bad("dupskip", f"{name}: the VOD decodes nothing for {ds['vod_blind_s']:.2f} s (runs of 2+ frames) "
                           "where the recording decodes (black or a slate on YouTube?)", "UNKNOWN")
        if (ds.get("unanchored_s") or 0) > END_SLACK_S:
            bad("dupskip", f"{name}: {ds['unanchored_s']:.1f} s of the window has no frame both files show "
                           "unambiguously", "UNKNOWN")
        if ds.get("unjudged", 0) > UNJUDGED_MAX:
            bad("dupskip", f"{name}: {ds['unjudged']} VOD frame pairs could not be judged", "UNKNOWN")
        cov = w.get("coverage") or {}
        for side in ("rec", "vod"):
            pct = cov.get(f"{side}_cadence_pct")
            if pct is None or pct < CADENCE_MIN_PCT:
                bad("coverage", f"{name}: {side} cadence-proven {pct} % < {CADENCE_MIN_PCT:.0f} %", "UNKNOWN")
        covered = w.get("covered_fraction")
        if covered is not None and covered < MIN_WINDOW_COVERED:
            bad("coverage", f"{name}: the VOD covers only {covered:.0%} of the window", "UNKNOWN")
        av = w.get("av") or {}
        if av.get("delta_ms") is None:
            bad("av", f"{name}: A/V not measured ({av.get('error', 'missing')})", "UNKNOWN")
        elif abs(av["delta_ms"] - ref) > AV_TOLERANCE_MS:
            bad("av", f"{name}: A/V VOD - recording {av['delta_ms']:+.1f} ms is "
                      f"{av['delta_ms'] - ref:+.1f} ms off the first window (> {AV_TOLERANCE_MS:.0f})", "FAIL")
        for kind, msg in _audio_findings(name, w.get("audio") or {}):
            bad("audio", msg, kind)
    for p in publishes:
        if not p.get("judged", True):
            continue
        when = fmt_utc(p["utc"]) if isinstance(p["utc"], (int, float)) else p["utc"]
        if p.get("gap_s") is None:
            bad("publish", f"publish {when}: no VOD frame after it", "FAIL")
        elif p["gap_s"] > PUBLISH_GAP_MAX_S:
            bad("publish", f"publish {when}: first VOD frame {p['gap_s']:.2f} s late "
                           f"(> {PUBLISH_GAP_MAX_S} s)", "FAIL")
    overall = "FAIL" if fails else ("UNKNOWN" if unknowns else "PASS")
    return {"overall": overall, "reasons": fails + unknowns, "criteria": status}


# ---------------------------------------------------------------- CLI

def parse_utc(s):
    """Epoch seconds, ISO 8601 or compact ISO (YYYYMMDDTHHMMSS[.fff]Z) -> epoch seconds."""
    s = s.strip()
    if re.fullmatch(r"\d+(\.\d*)?", s):
        return float(s)
    m = re.fullmatch(r"(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})(\.\d+)?Z", s)
    if m:
        g = m.groups()
        s = f"{g[0]}-{g[1]}-{g[2]}T{g[3]}:{g[4]}:{g[5]}{g[6] or ''}Z"
    d = datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))
    if d.tzinfo is None:
        raise ValueError(f"timestamp without a time zone: {s}")
    return d.timestamp()


def _is_utc(s):
    try:
        parse_utc(s)
    except ValueError:
        return False
    return True


def parse_window(arg):
    """name:start:end (start/end in any parse_utc form; ISO colons are fine)."""
    name, _, rest = arg.partition(":")
    found = [(parse_utc(rest[:m.start()]), parse_utc(rest[m.end():])) for m in re.finditer(":", rest)
             if _is_utc(rest[:m.start()]) and _is_utc(rest[m.end():])]
    if not name or len(found) != 1 or found[0][1] <= found[0][0]:
        raise ValueError(f"bad --windows value {arg!r} (want name:start:end)")
    return name, found[0][0], found[0][1]


def parse_recording(arg):
    """file[@record-start-utc] -> (file, start | None)."""
    head, sep, tail = arg.rpartition("@")
    if sep and head and _is_utc(tail):
        return head, parse_utc(tail)
    return arg, None


def fmt_utc(x):
    if x is None:
        return None
    return (datetime.datetime.fromtimestamp(x, tz=datetime.timezone.utc)
            .isoformat(timespec="milliseconds").replace("+00:00", "Z"))


def fetch_vod(vod, out):
    if os.path.isfile(vod):
        return vod
    if not re.fullmatch(r"[A-Za-z0-9_-]{11}", vod):
        raise ValueError(f"--vod {vod!r} is neither a file nor a YouTube id")
    path = os.path.join(out, f"vod-{vod}.mp4")
    if not os.path.isfile(path):
        run_bounded(["yt-dlp", "-q", "--no-progress", "--socket-timeout", "30", "-f",
                     "bv*[height<=1080][fps<=30][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=1080][fps<=30]+ba/b",
                     "--merge-output-format", "mp4", "-o", path, f"https://www.youtube.com/watch?v={vod}"],
                    YTDLP_TIMEOUT_S, check=True)
    return path


def fetch_markers(src, out, name="markers.csv"):
    if re.match(r"https?://", src):
        path = os.path.join(out, name)
        with urllib.request.urlopen(src, timeout=URL_TIMEOUT_S) as r, open(path, "wb") as f:
            shutil.copyfileobj(r, f)
        return path
    if not os.path.isfile(src):
        raise ValueError(f"marker log {src} not found")
    return src


def tick_cache_key(src, runs=()):
    """The tick-map cache key: source, decoder, OpenCV; a run-scoped decode adds its `runs` (the
    default key is the one restreamer's gate has always written)."""
    st = os.stat(src)
    key = (f"source={os.path.abspath(src)} size={st.st_size} mtime={int(st.st_mtime)} "
           f"decoder=v{DECODER_VERSION} scale={DECODE_SCALE} phase_radius={PHASE_RADIUS} opencv={_cv2_version()}")
    return key + (f" runs={','.join(str(r) for r in runs)}" if runs else "")


def cached_ticks(src, cache, workers, runs=()):
    """The file's tick map, decoded once per (source, decoder, runs) key and kept in the out dir:
    (index, pts, tick) rows, or run-scoped (index, pts, tick, run) rows when `runs` is given."""
    key = tick_cache_key(src, runs)
    if os.path.isfile(cache):
        with open(cache) as f:
            if f.readline().strip() == f"# {key}":
                return load_run_ticks(cache) if runs else load_ticks(cache)
    if runs:
        rows = decode_ticks(src, workers, runs=tuple(runs))
        write_ticks(cache, rows, header=key)
        return [r[:3] + (r[6],) for r in rows]
    rows = decode_ticks(src, workers)
    write_ticks(cache, rows, header=key)
    return [r[:3] for r in rows]


def _cv2_version():
    import cv2

    return cv2.__version__


def _guard(fn, *a, **kw):
    try:
        return fn(*a, **kw)
    except Exception as e:  # a measurement error is UNKNOWN for that criterion, never a crash
        print(f"youtube_leg_verdict: {fn.__name__} failed: {type(e).__name__}: {e}", file=sys.stderr)
        return {"error": f"{type(e).__name__}: {e}"}


def measure_window(ctx, name, a, b):
    """One window's dup/skip, coverage, audio and A/V; errors land in w['errors'] / each block."""
    rec_rows, vod_rows, t0, starts = ctx["rec_rows"], ctx["vod_rows"], ctx["t0"], ctx["starts"]
    w = {"name": name, "start": fmt_utc(a), "end": fmt_utc(b), "errors": []}
    inside = [p for p in ctx["publishes"] if a < p < b]
    stops = [u for u in ctx["unpublishes"] if a < u < b + STOP_MARGIN_S]
    if inside or stops:
        what = (f"a publish at {fmt_utc(inside[0])} is inside the window" if inside else
                f"the stream stopped at {fmt_utc(stops[0])}, inside the window or under {STOP_MARGIN_S:.0f} s after it")
        w["errors"].append(f"{what} (one publish span per window)")
        return w
    ds = _guard(dupskip, rec_rows, vod_rows, t0, a, b)
    if "run" in ds:  # run-scoped rows (--runs): the window's run, judged on its own tick line
        w["run"] = ds.pop("run")
    w["dupskip"] = ds
    if "error" in ds:
        w["errors"].append(ds["error"])
        return w
    a2, b2, cov_end = ds.pop("start_utc"), ds.pop("end_utc"), ds.pop("coverage_end_utc")
    w["clamped_start"], w["clamped_end"] = fmt_utc(ds.pop("clamped_start_utc")), fmt_utc(ds.pop("clamped_end_utc"))
    w["covered_fraction"] = round((b2 - a2) / (b - a), 3)
    w["coverage"] = coverage(rec_rows, vod_rows, t0, a2, cov_end)  # a tail the VOD lacks counts as unproven
    k = max([i for i, s in enumerate(starts) if s <= a2 + 1e-6] or [0])
    if k + 1 < len(starts) and b2 > starts[k + 1]:
        w["errors"].append("the window spans two recording parts")
        return w
    if k not in ctx["rec_audio"]:
        ctx["rec_audio"][k] = load_audio(ctx["recs"][k][0])
    w["audio"] = _guard(audio_window, ctx["rec_audio"][k], ctx["vod_audio"], rec_rows, vod_rows, t0, a2, cov_end,
                        rec_pts0=starts[k] - t0)  # audio needs no painter: to the coverage end, not the VOD's
    w["audio"].pop("start_utc", None)
    rec_p, vod_p = vod_pts_for(rec_rows, vod_rows, t0, a2)
    markers, av_run = ctx["markers"], None
    if w.get("run") in ctx["runs"]:  # a CG window: the clip carries its own marker (--av-run)
        markers, av_run = ctx["clip_markers"], w["run"]
    if rec_p is None or vod_p is None:
        w["av"] = {"error": "window start tick not found in the VOD"}
    elif markers is None:
        w["av"] = {"error": f"no marker log for run {av_run} (--clip-markers)"}
    else:
        w["av"] = _guard(av_window, ctx["recs"][k][0], ctx["vod_file"], rec_p - (starts[k] - t0), vod_p,
                         markers, ctx["probe_bin"], min(AV_CLIP_S, b2 - a2), ctx["out"], av_run)
    return w


def measure(args):
    os.makedirs(args.out, exist_ok=True)
    windows_in = [parse_window(s) for s in args.windows]
    publishes = [parse_utc(p) for p in args.publish]
    recs = [parse_recording(r) for r in args.recording]
    vod_file = fetch_vod(args.vod, args.out)
    runs = tuple(args.runs)
    ctx = {"recs": recs, "vod_file": vod_file, "markers": fetch_markers(args.markers, args.out),
           "probe_bin": args.probe_bin, "out": args.out, "rec_audio": {}, "publishes": publishes,
           "unpublishes": [parse_utc(u) for u in args.unpublish], "runs": runs,
           "clip_markers": (fetch_markers(args.clip_markers, args.out, "clip-markers.csv")
                            if args.clip_markers else None)}
    part_rows = [cached_ticks(p, os.path.join(args.out, f"ticks-rec-{k + 1}.tsv"), args.workers, runs)
                 for k, (p, _) in enumerate(recs)]
    ctx["vod_rows"] = cached_ticks(vod_file, os.path.join(args.out, "ticks-vod.tsv"), args.workers, runs)
    ctx["rec_rows"], ctx["t0"], ctx["starts"] = join_part_rows(part_rows, [s for _, s in recs], restarting=runs)
    ctx["vod_audio"] = load_audio(vod_file)
    windows = [measure_window(ctx, name, a, b) for name, a, b in windows_in]
    pubs = publish_gaps(ctx["rec_rows"], ctx["vod_rows"], ctx["t0"], publishes)
    v = verdict(windows, pubs)
    for p in pubs:
        for key in ("utc", "first_vod_frame_utc", "last_vod_frame_before_utc"):
            p[key] = fmt_utc(p[key])
    tool = {"decoder": DECODER_VERSION, "opencv": _cv2_version()}
    if runs:
        tool["runs"] = list(runs)
    return {"schema": SCHEMA, "overall": v["overall"], "tool": tool,
            "criteria": {"status": v["criteria"], "av_tolerance_ms": AV_TOLERANCE_MS,
                         "publish_gap_max_s": PUBLISH_GAP_MAX_S, "cadence_min_pct": CADENCE_MIN_PCT,
                         "lag_jump_ms": 1000.0 * LAG_JUMP / SR, "low_corr": LOW_CORR,
                         "silent_vod_dbfs": SILENT_VOD_DBFS, "level_drop_db": LEVEL_DROP_DB, "foreign_db": FOREIGN_DB,
                         "vod_blind_max_s": VOD_BLIND_MAX_S},
            "recording_starts": [fmt_utc(s) for s in ctx["starts"]], "windows": windows, "publishes": pubs,
            "reasons": v["reasons"]}


def write_result(out, result):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, "youtube-leg-verdict.json")
    with open(f"{path}.tmp", "w") as f:
        json.dump(result, f, indent=1)
    os.replace(f"{path}.tmp", path)
    return path


def main(argv=None):
    ap = argparse.ArgumentParser(description="Issue 1404 YouTube-leg verdict (exit 0 PASS, 1 FAIL, 2 UNKNOWN)")
    ap.add_argument("--decode-ticks", help="diagnostic: print this file's tick map and exit")
    ap.add_argument("--vod")
    ap.add_argument("--recording", action="append", default=[])
    ap.add_argument("--markers")
    ap.add_argument("--windows", action="append", default=[])
    ap.add_argument("--publish", action="append", default=[])
    ap.add_argument("--unpublish", action="append", default=[], help="a StopStream time (utc)")
    ap.add_argument("--out")
    ap.add_argument("--probe-bin", default=os.environ.get("RECORDING_VERDICT_BIN", "recording-verdict"))
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--runs", type=int, action="append", default=[], choices=CLIP_RUNS,
                    help="also read this reserved run as a tick (the measurement clip 911016 in the CG segments); "
                         "every window is then judged on its own run segment")
    ap.add_argument("--clip-markers", help="the measurement clip's own marker log (<clip>.markers.csv | http url): "
                                           "the A/V of a window of a --runs run")
    args = ap.parse_args(argv)
    if args.decode_ticks:
        rows = decode_ticks(args.decode_ticks, args.workers, runs=tuple(args.runs)) if args.runs else \
            decode_ticks(args.decode_ticks, args.workers)
        for r in rows:
            print("\t".join([str(r[0]), f"{r[1]:.3f}"] + ["" if v is None else str(v) for v in r[2:]]))
        return EXIT_PASS
    if not (args.vod and args.markers and args.out and args.recording and args.windows):
        ap.error("--vod, --recording, --markers, --windows and --out are required")
    try:
        result = measure(args)
    except Exception as e:  # fail closed: any tool error is UNKNOWN, written like any verdict
        print(f"youtube_leg_verdict: tool error: {type(e).__name__}: {e}", file=sys.stderr)
        result = {"schema": SCHEMA, "overall": "UNKNOWN", "criteria": {}, "windows": [], "publishes": [],
                  "reasons": [f"tool error: {type(e).__name__}: {e}"]}
    path = write_result(args.out, result)
    print(f"youtube-leg verdict: {result['overall']} -> {path}")
    for r in result["reasons"]:
        print(f"  - {r}")
    return {"PASS": EXIT_PASS, "FAIL": EXIT_FAIL}.get(result["overall"], EXIT_UNKNOWN)


def entry(argv=None):
    """main() with every crash (a full disk at write time, ...) mapped to UNKNOWN, never FAIL's 1."""
    install_cleanup()
    try:
        return main(argv)
    except SystemExit:
        raise
    except BaseException:  # noqa: B036 -- a crash anywhere is fail-closed UNKNOWN
        traceback.print_exc()
        return EXIT_UNKNOWN


if __name__ == "__main__":
    sys.exit(entry())
