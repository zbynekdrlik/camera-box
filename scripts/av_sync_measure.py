#!/usr/bin/env python3
"""#801 — AI A/V-sync meter: SyncNet offset of the stream-OBS program output, zero management.

Wraps syncnet_python (pretrained SyncNet + S3FD): measures the audio-visual offset of a clip
of the PROGRAM output and maps it onto the one knob the operator actually turns — the
'NDI 2ME PGM' genlock latency. Confidence gating makes it self-managing: windows without a
usable face/lip correlation (band wide shots, graphics) report as unmeasurable and are simply
skipped — nobody has to tell it "now there is speech".

Sign convention (VALIDATED 2026-07-19 on synthetic shifts, exact to the frame):
  SyncNet offset +N frames  = audio LEADS video by N*40 ms (video late)   -> LOWER the latency knob
  SyncNet offset -N frames  = audio LAGS  video by N*40 ms (video early)  -> RAISE the latency knob
  => knob_delta_ms = -offset_frames * 40

Usage:
  av_sync_measure.py --media clip.mp4 [--repo DIR] [--webhook URL] [--threshold-ms 60]
  av_sync_measure.py --grab srt://127.0.0.1:9998 --secs 20 [...]        # one-shot from live tap
  av_sync_measure.py --grab ... --loop 300 [...]                        # daemon: measure every 300 s
  av_sync_measure.py --grab ... --loop 420 --outer-loop --ws-host 10.77.9.204 [...]
      # #806 outer-loop watchdog: every confident window feeds OuterLoopGuard; a correction event
      # applies the new bias over obs-websocket (SetAsrcOuterBiasPpm) and Discord-reports it.

Alert delivery (#1207): by DEFAULT (no --webhook) the |offset|>=threshold + outer-loop alerts go
through `airuleset.py notify` with a stable per-kind --dedup-key (fleet-standard since #1206, so a
repeated identical state edits the existing card instead of re-pinging). Passing `--webhook URL`
keeps the raw-webhook path (manual opt-in) with a simple per-kind throttle instead.

Exit codes: 0 measured, 2 unmeasurable window (low confidence), 1 error,
            3 NO-SIGNAL (#814: --require-fresh rejected a stale/failed grab -- no verdict emitted).
"""

import argparse
import json
import os
import pickle
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

# #806: outer-loop guard + its obs-websocket control channel. Only exercised by --outer-loop
# (measure-only usage never calls these, so no connection is ever attempted); imported at module
# level (not lazily) so tests can monkeypatch _conn/_rpc, same convention as av_sync_calibrate.py.
from av_sync_outer_loop_guard import OuterLoopGuard
from obs_phase2 import _conn, _rpc

CONF_MIN = 4.0  # below this SyncNet's own confidence, the window is unusable (no face / no lips)
FRAME_MS = 40  # syncnet pipeline is fixed 25 fps

# #806: the program-audio source the outer-loop guard corrects. NOT an NDI/DistroAV source (see
# issue #803's design comment) -- reached over the source-name-addressed, type-agnostic
# SetAsrcOuterBiasPpm/GetAsrcOuterBiasPpm obs-websocket requests instead.
DEFAULT_OUTER_LOOP_SOURCE = "mbc"


def default_outer_loop_state_path() -> Path:
    """Where the #806 watchdog persists its guard's current bias_ppm across restarts (the window
    itself is intentionally NOT persisted, per OuterLoopGuard.from_bias_ppm's own doc comment).

    Same %PROGRAMDATA%/camera-box convention as av_sync_calibrate.py's default_last_json_path() --
    a DIFFERENT filename (this is a distinct piece of state), same fallback for off-rig testing.
    """
    programdata = os.environ.get("PROGRAMDATA")
    if programdata:
        return Path(programdata) / "camera-box" / "asrc-outer-loop-state.json"
    return Path.home() / ".camera-box" / "asrc-outer-loop-state.json"


def load_outer_loop_guard(path: Path) -> OuterLoopGuard:
    """Load a persisted bias_ppm (if the file exists and parses) into a fresh guard; a
    missing/corrupt file starts a guard at bias_ppm=0 -- default-safe, never guesses a bias."""
    try:
        data = json.loads(path.read_text())
        return OuterLoopGuard.from_bias_ppm(float(data["bias_ppm"]))
    except (FileNotFoundError, json.JSONDecodeError, KeyError, ValueError, TypeError):
        return OuterLoopGuard()


def save_outer_loop_state(path: Path, guard: OuterLoopGuard) -> None:
    """Persist ONLY guard.bias_ppm, atomically (write-tmp + replace, mirrors
    av_sync_calibrate.py's write_last_json() so a reader never observes a partial file)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps({"bias_ppm": guard.bias_ppm, "ts": time.time()}, indent=2))
    tmp.replace(path)


def apply_outer_bias(ws, source: str, current_ppm: float, new_ppm: float) -> float:
    """Apply `new_ppm` via SetAsrcOuterBiasPpm on `source`, verify via GetAsrcOuterBiasPpm
    read-back (the SAME #358 verify+rollback pattern av_sync_calibrate.py's apply_latency()
    already established for the genlock-latency knob). On a read-back mismatch, ROLLS BACK to
    `current_ppm` and FAILS LOUD -- the source is never left half-set."""
    print(f"[av-sync] SET '{source}' asrc_outer_bias_ppm: {current_ppm} -> {new_ppm}")
    _rpc(ws, "SetAsrcOuterBiasPpm", {"inputName": source, "biasPpm": new_ppm})
    actual = _rpc(ws, "GetAsrcOuterBiasPpm", {"inputName": source}).get("biasPpm")
    if actual is not None and abs(actual - new_ppm) < 1e-6:
        print(f"[av-sync] VERIFIED '{source}' asrc_outer_bias_ppm={actual}")
        return actual

    sys.stderr.write(
        f"[av-sync] read-back mismatch on '{source}': set {new_ppm}, got {actual!r} -- "
        f"rolling back to {current_ppm}\n"
    )
    _rpc(ws, "SetAsrcOuterBiasPpm", {"inputName": source, "biasPpm": current_ppm})
    rollback_actual = _rpc(ws, "GetAsrcOuterBiasPpm", {"inputName": source}).get("biasPpm")
    if rollback_actual is None or abs(rollback_actual - current_ppm) >= 1e-6:
        sys.stderr.write(
            f"[av-sync] WARN rollback ALSO mismatched on '{source}': expected {current_ppm}, "
            f"got {rollback_actual!r} -- manual check required!\n"
        )
    raise SystemExit(
        f"[av-sync] FAILED to apply asrc_outer_bias_ppm={new_ppm} on '{source}' "
        f"(read-back={actual!r}); rolled back to {current_ppm} "
        f"(rollback read-back={rollback_actual!r}) -- source never left half-set"
    )


# #806: an in-PROCESS cache of live OuterLoopGuard objects, keyed by state path. `--loop` mode
# calls one_measurement()/run_outer_loop() fresh every ~7 min from the SAME long-running process
# (main()'s while-True), and the guard's own WINDOW_N-sample sliding window (deliberately NOT
# persisted to disk, per OuterLoopGuard.from_bias_ppm's own doc comment -- only bias_ppm survives
# a genuine process RESTART) would never accumulate across iterations if each call re-loaded a
# brand-new guard from disk. This cache is what lets the window actually stay "sustained" across
# the ~21 min (WINDOW_N * ~7 min) the guard needs to see before it ever acts, while a genuine
# process restart still starts fresh from the persisted bias (first access per key loads from
# disk; nothing here survives across a real restart, which is the correct behavior).
_OUTER_LOOP_GUARDS: dict = {}


def _get_outer_loop_guard(state_path: Path) -> OuterLoopGuard:
    key = str(state_path)
    if key not in _OUTER_LOOP_GUARDS:
        _OUTER_LOOP_GUARDS[key] = load_outer_loop_guard(state_path)
    return _OUTER_LOOP_GUARDS[key]


def run_outer_loop(args, offset_ms: float) -> None:
    """#806: feed one confident measurement's offset_ms into the live, in-process OuterLoopGuard
    (see `_get_outer_loop_guard`'s own doc comment for why this must NOT reload from disk every
    call); on a correction event, apply the new bias over obs-websocket and Discord-report it.
    Never raises on a measurement that does not trigger a correction (the common case)."""
    state_path = Path(args.outer_loop_state) if args.outer_loop_state else default_outer_loop_state_path()
    guard = _get_outer_loop_guard(state_path)
    event = guard.observe(offset_ms)
    if event is None:
        return

    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    text = (
        f"[{stamp}] outer-loop correction on '{args.outer_loop_source}': "
        f"avg_residual={event.avg_residual_ms:+.1f}ms bias {event.previous_bias_ppm:+.2f}ppm -> "
        f"{event.new_bias_ppm:+.2f}ppm"
    )
    print(text)

    ws = _conn(args.ws_host, args.ws_password)
    try:
        apply_outer_bias(ws, args.outer_loop_source, event.previous_bias_ppm, event.new_bias_ppm)
    finally:
        ws.close()

    save_outer_loop_state(state_path, guard)
    deliver_alert(args, "asrc", f"🎚️ ASRC outer-loop: {text}")


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def tap_preflight(grab_url: "str | None") -> "tuple[bool, str]":
    """#802: reader-side SRT-tap preflight. The A/V-sync tap is redesigned as an SRT LISTENER on
    the OBS side (`scripts/srt_tap.py` -- a listener bind never fails on a missing peer, so it can
    never crash OBS); this reader grabs FROM it as the CALLER. Returns (ok, reason): a PROVABLY
    dead tap short-circuits to `(False, 'NO-SIGNAL: ...')` so the caller can exit 3 (#814 family)
    instead of a doomed ffmpeg connect. Fails OPEN (proceed) when there is no --grab URL, the URL
    is not srt://, or srt_tap is unavailable -- a live-but-quiet SRT listener is never
    false-rejected."""
    if not grab_url:
        return True, "ok (no --grab)"
    try:
        from srt_tap import reader_should_grab
    except ImportError as exc:
        return True, f"ok (srt_tap unavailable; tap preflight skipped: {exc})"
    return reader_should_grab(grab_url)


def grab_clip(url: str, secs: int, out: Path) -> None:
    r = run(["ffmpeg", "-v", "error", "-y", "-i", url, "-t", str(secs),
             "-vf", "scale=960:-2,fps=25", "-c:v", "libx264", "-preset", "veryfast", "-crf", "26",
             "-c:a", "aac", "-ar", "16000", "-ac", "1", str(out)])
    if r.returncode != 0 or not out.exists():
        sys.exit(f"ERROR: grab failed: {r.stderr.strip()[:300]}")


def _mean_shift_curve(dist_track) -> "list[float]":
    """#917 -- reduce ONE track's raw per-frame distance array (shape (nframes, win_size), exactly
    what the vendored `run_syncnet.py` (stock, unmodified upstream joonson/syncnet_python) already
    dumps to `pickle.dump(dists, fil)` at the end of its OWN run) to the per-shift MEAN distance
    curve `SyncNetInstance.evaluate()` computes internally
    (`mdist = torch.mean(torch.stack(dists,1),1)`) but does not return or print. Pure list/float
    math -- no numpy/torch import needed in THIS module (unpickling a numpy array still works;
    numpy is already a declared dependency everywhere this script actually runs -- the live
    syncnet_python venv and the repo's own CI pytest job).

    Verified LIVE (2026-08-01, real stream-box syncnet_python install) to reproduce SyncNet's own
    `mdist` bit-for-bit: averaging `dists_npy` (the pickle's per-track array) over the frame axis
    and locating its argmin reproduced the EXACT SAME integer "AV offset" SyncNet itself printed,
    for all 3 real face tracks in a real 35s soundcheck clip.
    """
    frames = [[float(x) for x in row] for row in dist_track]
    if not frames:
        return []
    n = len(frames)
    win = len(frames[0])
    return [sum(frame[i] for frame in frames) / n for i in range(win)]


def dist_curve_for_track(dist_track, offset_frames: int) -> "list[float] | None":
    """#917 -- the 3-point SyncNet mean-distance window `[argmin-1, argmin, argmin+1]` that
    `av_sync_calibrate.py`'s `parabolic_subframe_offset()`/`window_offset_ms()` (#805) consumes
    via the optional `dist_curve` field, for sub-frame refinement of the plain frame-quantized
    offset. Returns `None` (== today's exact frame-quantized behavior, zero regression) when:
      - `dist_track` is falsy/empty/too short to have 3 shift bins,
      - the argmin derived from the curve does NOT match SyncNet's own reported `offset_frames`
        (a pickle/track-count mismatch -- never guess a curve for the wrong track), or
      - the argmin sits at either EDGE of the shift window (no neighbor on one side to fit a
        parabola through).
    """
    curve = _mean_shift_curve(dist_track)
    if len(curve) < 3:
        return None
    minidx = min(range(len(curve)), key=lambda i: curve[i])
    vshift = (len(curve) - 1) // 2
    derived_offset = vshift - minidx
    if derived_offset != offset_frames:
        return None
    if minidx == 0 or minidx == len(curve) - 1:
        return None
    return [curve[minidx - 1], curve[minidx], curve[minidx + 1]]


def _load_dist_tracks(workdir: Path, ref: str) -> "list | None":
    """#917 -- load the per-track raw per-shift distance arrays the vendored, UNMODIFIED
    `run_syncnet.py` already dumps unconditionally to `<data_dir>/pywork/<ref>/activesd.pckl`
    after every run (one array per face track, in the SAME order `run_syncnet.py` evaluates them
    -- the same order its "AV offset"/"Confidence" log lines are printed in). Returns `None`
    (never raises) on ANY failure to read/parse it -- a missing or malformed pickle must degrade
    extraction to today's plain frame-quantized offset, never break a measurement.
    """
    path = workdir / "pywork" / ref / "activesd.pckl"
    try:
        with open(path, "rb") as f:
            data = pickle.load(f)
    except (OSError, pickle.UnpicklingError, EOFError, ImportError, AttributeError, ValueError):
        return None
    if not isinstance(data, list):
        return None
    return data


def measure(repo: Path, media: Path, workdir: Path):
    """Run syncnet_python's two stages; return list of (offset_frames, confidence, dist_curve)
    per track. `dist_curve` (#917) is the 3 SyncNet per-shift mean-distance values around the
    reported offset bin -- see `dist_curve_for_track` -- or `None` when unavailable (exactly
    today's plain frame-quantized behavior). Consumed by `av_sync_calibrate.py`'s
    `window_offset_ms()` (#805) for sub-frame refinement of the baseline calibration.
    """
    py = sys.executable
    ref = "m"
    for stage in ("run_pipeline.py", "run_syncnet.py"):
        r = run([py, str(repo / stage), "--videofile", str(media), "--reference", ref,
                 "--data_dir", str(workdir)], cwd=str(repo))
        if stage == "run_pipeline.py" and r.returncode != 0:
            sys.exit(f"ERROR: {stage} failed: {(r.stderr or r.stdout).strip()[-300:]}")
        out = (r.stdout or "") + (r.stderr or "")
    # SyncNetInstance logs per track: "AV offset:  N" then "Confidence: C"
    offsets = [int(m) for m in re.findall(r"AV offset:\s*(-?\d+)", out)]
    confs = [float(m) for m in re.findall(r"Confidence:\s*([\d.]+)", out)]

    dist_tracks = _load_dist_tracks(workdir, ref)
    if dist_tracks is not None and len(dist_tracks) == len(offsets):
        curves = [
            dist_curve_for_track(track, off) for track, off in zip(dist_tracks, offsets)
        ]
    else:
        # Missing pickle, OR its track count disagrees with the regex-parsed log lines -- never
        # guess which curve belongs to which track; fall back to plain frame-quantized for all.
        curves = [None] * len(offsets)

    return list(zip(offsets, confs, curves))


def log_calibration_window(path: str, record: dict) -> None:
    """#805 -- append one JSON line per measured window (usable AND unmeasurable) for later
    baseline aggregation via `av_sync_calibrate.py --calibrate`. JSONL so a long soundcheck
    `--loop` run can append safely without re-reading/re-writing the whole file each round."""
    with open(path, "a") as f:
        f.write(json.dumps(record) + "\n")


def notify_discord(webhook: str, text: str) -> None:
    data = json.dumps({"content": text}).encode()
    req = urllib.request.Request(webhook, data=data, headers={"Content-Type": "application/json"})
    try:
        urllib.request.urlopen(req, timeout=15).read()
    except OSError as exc:
        print(f"WARN: discord webhook failed: {exc}")


# #1207 — alert DELIVERY layer. av_sync_measure was the ONE alert emitter in this repo that still
# POSTed to a RAW Discord webhook (notify_discord() above → urllib) with no dedup and entirely
# outside airuleset.py notify, so the #1206 --dedup-key sweep never covered it. The DEFAULT (no
# --webhook) now routes through airuleset notify with a STABLE per-incident --dedup-key — the
# fleet-standard path since #1206: a repeated identical state EDITS the existing airuleset card
# instead of re-pinging, and the analyze-not-ping doctrine applies for free. See
# .claude/rules/watchdog-notify-dedup.md. Detection/measurement is UNCHANGED — only WHERE the
# alert is delivered. The `AIRULESET_NOTIFY` env override mirrors the bash watchdogs' own default.
AIRULESET_NOTIFY = os.environ.get(
    "AIRULESET_NOTIFY", str(Path.home() / "devel" / "airuleset" / "airuleset.py")
)

# The EXPLICIT `--webhook URL` override keeps today's raw-webhook path (manual opt-in) but gains a
# simple in-process per-KIND cooldown so a sustained state in --loop mode does not re-POST every
# round — the raw webhook has no dedup of its own, which is the whole reason for #1207. 20 min
# mirrors the fleet alert-watchdogs' own re-fire cadence (airuleset #1206).
WEBHOOK_THROTTLE_S = 1200
_WEBHOOK_LAST_SENT: dict = {}  # kind -> time.monotonic() of the last raw-webhook send for that kind


def notify_airuleset(text: str, dedup_key: str) -> None:
    """#1207 — DEFAULT alert delivery: send `text` through airuleset notify with a STABLE
    per-incident `--dedup-key` (fleet-standard since #1206). Best-effort: a missing/failing
    airuleset prints a WARN and never aborts a measurement (same tolerance as notify_discord's own
    OSError guard). Written as a literal `subprocess.run([...])` so the #1206 dedup-key sweep
    (tests/python/test_notify_dedup_key_sweep_1206.py) auto-discovers + enforces this call too."""
    try:
        subprocess.run([sys.executable, AIRULESET_NOTIFY, "notify", "--body", text,
                        "--dedup-key", dedup_key], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as exc:
        print(f"WARN: airuleset notify failed: {exc}")


def deliver_alert(args, kind: str, text: str) -> None:
    """#1207 — the single alert-delivery seam both call-sites route through. `kind` is one of
    "asrc" / "verdict". DEFAULT (no --webhook): route through airuleset notify with a stable
    `av-sync-measure-<kind>` dedup key. EXPLICIT `--webhook URL` override: keep the raw-webhook
    path (manual opt-in), suppressed per-kind within WEBHOOK_THROTTLE_S so a sustained state does
    not spam. Delivery layer only — the caller's detection/threshold logic is unchanged."""
    if args.webhook:
        now = time.monotonic()
        last = _WEBHOOK_LAST_SENT.get(kind)
        if last is not None and (now - last) < WEBHOOK_THROTTLE_S:
            return  # per-kind throttle: same kind fired < WEBHOOK_THROTTLE_S ago
        _WEBHOOK_LAST_SENT[kind] = now
        notify_discord(args.webhook, text)
    else:
        notify_airuleset(text, f"av-sync-measure-{kind}")


def _calibration_record(stamp: str, offset_frames: int, confidence: float, usable: bool,
                         dist_curve) -> dict:
    """#917 -- build one `--calibration-log` JSONL record. `dist_curve` is included ONLY when
    present (not `None`) so a run with no real curve produces the EXACT SAME record shape as
    before #917 -- `av_sync_calibrate.py`'s `window_offset_ms()` already treats a missing
    `dist_curve` key as "no sub-frame data" via `record.get("dist_curve")`."""
    rec = {"ts": stamp, "offset_frames": offset_frames, "confidence": confidence, "usable": usable}
    if dist_curve is not None:
        rec["dist_curve"] = dist_curve
    return rec


def one_measurement(args, repo: Path) -> int:
    with tempfile.TemporaryDirectory(prefix="avsync-") as td:
        workdir = Path(td)
        if args.grab:
            media = workdir / "grab.mp4"
            grab_clip(args.grab, args.secs, media)
        else:
            media = Path(args.media).resolve()
            if not media.exists():
                sys.exit(f"ERROR: no such file: {media}")
        tracks = measure(repo, media, workdir)

    usable = [t for t in tracks if t[1] >= CONF_MIN]
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    if not usable:
        best = max(tracks, key=lambda t: t[1], default=(0, 0.0, None))
        print(f"[{stamp}] UNMEASURABLE window (best confidence {best[1]:.1f} < {CONF_MIN}"
              f" — no usable face/lips; band/graphics segments are expected to skip)")
        if getattr(args, "calibration_log", None):
            log_calibration_window(
                args.calibration_log,
                _calibration_record(stamp, best[0], best[1], False, best[2] if len(best) > 2 else None),
            )
        return 2

    offset_frames, conf, dist_curve = max(usable, key=lambda t: t[1])
    if getattr(args, "calibration_log", None):
        log_calibration_window(
            args.calibration_log,
            _calibration_record(stamp, offset_frames, conf, True, dist_curve),
        )
    offset_ms = offset_frames * FRAME_MS
    knob = -offset_ms
    if offset_ms > 0:
        verdict = f"audio predbieha video o ~{offset_ms} ms -> ZNIZ '2ME PGM' latency o {abs(knob)}"
    elif offset_ms < 0:
        verdict = f"video predbieha audio o ~{abs(offset_ms)} ms -> ZVYS '2ME PGM' latency o {abs(knob)}"
    else:
        verdict = "A/V sync OK (offset 0 ms)"
    line = f"[{stamp}] AV offset {offset_frames:+d} fr ({offset_ms:+d} ms) conf {conf:.1f} :: {verdict}"
    print(line)
    if abs(offset_ms) >= args.threshold_ms:
        deliver_alert(args, "verdict", f"🎯 AV-sync watchdog: {verdict} (conf {conf:.1f})")
    if getattr(args, "outer_loop", False):
        # #806: feed this CONFIDENT measurement into the outer-loop guard. Independent of the
        # --threshold-ms alert above -- the guard has its own 40ms sustained-average threshold and
        # only acts every ~WINDOW_N windows, so this runs on every usable window regardless.
        run_outer_loop(args, float(offset_ms))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--media", help="measure an existing clip file")
    src.add_argument("--grab", help="grab N secs from this ffmpeg-readable URL (srt://, rtmp://...)")
    ap.add_argument("--secs", type=int, default=20)
    ap.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent / "syncnet_python"),
                    help="path to syncnet_python checkout (with data/ + s3fd weights)")
    ap.add_argument(
        "--webhook",
        help="#1207: EXPLICIT raw-webhook override for the |offset| >= threshold + outer-loop "
             "alerts. DEFAULT (omit this) routes those alerts through `airuleset.py notify` with a "
             "stable per-kind --dedup-key (fleet-standard since #1206); passing a URL keeps the raw "
             "webhook (manual opt-in) with a simple per-kind throttle instead.",
    )
    ap.add_argument("--threshold-ms", type=int, default=60)
    ap.add_argument(
        "--require-fresh", action="store_true",
        help="#814: assert the --media clip is a CURRENT grab (rc==0 + size + mtime age + "
             "duration, via the shared avsync_freshness gate) BEFORE measuring; a stale/failed "
             "grab prints 'NO-SIGNAL: <reason>' and exits 3 without ever emitting a verdict",
    )
    ap.add_argument(
        "--grab-rc", type=int, default=0,
        help="#814: the ffmpeg exit code of the grab that produced --media (used with "
             "--require-fresh); non-zero => NO-SIGNAL regardless of the clip left on disk",
    )
    ap.add_argument("--loop", type=int, metavar="SECS",
                    help="daemon mode: repeat every SECS (grab mode only)")
    ap.add_argument(
        "--tap-preflight", action="store_true",
        help="#802: before grabbing, run the reader-side SRT-tap preflight (srt_tap); if the tap "
             "is PROVABLY not up, print 'NO-SIGNAL: ...' and exit 3 instead of a doomed connect. "
             "The tap is a listener on the OBS side (crash-safe redesign); this reader is the caller.",
    )
    ap.add_argument(
        "--calibration-log", default=None,
        help="append each window's raw measurement as JSONL for later baseline aggregation "
             "via `av_sync_calibrate.py --calibrate` (#805)",
    )
    ap.add_argument(
        "--outer-loop", action="store_true",
        help="#806: feed every confident window into the outer-loop guard; on a correction "
             "event, apply the new bias over obs-websocket and Discord-report it (--loop mode)",
    )
    ap.add_argument(
        "--outer-loop-state", default=None,
        help="override the guard state path (default: %%PROGRAMDATA%%/camera-box/"
             "asrc-outer-loop-state.json)",
    )
    ap.add_argument(
        "--outer-loop-source", default=DEFAULT_OUTER_LOOP_SOURCE,
        help=f"obs-websocket input name the outer-loop bias is applied to (default: "
             f"{DEFAULT_OUTER_LOOP_SOURCE!r})",
    )
    ap.add_argument("--ws-host", default=None, help="obs-websocket host (required with --outer-loop)")
    ap.add_argument("--ws-password", default="", help="obs-websocket password")
    args = ap.parse_args()

    if args.require_fresh:
        # #814: never emit a verdict on a stale/failed grab. Checked BEFORE the syncnet/ffmpeg
        # presence checks below so a dead relay (grab rc!=0, possibly no file at all) short-circuits
        # to NO-SIGNAL without needing either. Uses the SAME pure gate the ps1 + installer use.
        try:
            from avsync_freshness import clip_facts, freshness_verdict
        except ImportError as exc:
            print(f"NO-SIGNAL: freshness gate unavailable ({exc})")  # fail CLOSED, never measure
            return 3
        size, age, dur = (-1, -1.0, -1.0)
        if args.media:
            size, age, dur = clip_facts(args.media)
        allowed, reason = freshness_verdict(args.grab_rc, size, age, dur)
        if not allowed:
            print(f"NO-SIGNAL: {reason}")
            return 3

    if args.tap_preflight:
        # #802: fail fast + clean if the redesigned SRT-listener tap is provably down, rather than
        # a doomed ffmpeg connect. Fails OPEN for a live/quiet listener (never a false NO-SIGNAL).
        ok, reason = tap_preflight(args.grab)
        if not ok:
            print(reason)  # already "NO-SIGNAL: ..."
            return 3

    repo = Path(args.repo).resolve()
    if not (repo / "run_syncnet.py").exists():
        sys.exit(f"ERROR: syncnet_python repo not found at {repo}")
    if not shutil.which("ffmpeg"):
        sys.exit("ERROR: ffmpeg not on PATH")
    if args.outer_loop and not args.ws_host:
        sys.exit("ERROR: --outer-loop requires --ws-host")

    if args.loop:
        if not args.grab:
            sys.exit("ERROR: --loop needs --grab")
        while True:
            t0 = time.time()
            try:
                one_measurement(args, repo)
            except SystemExit as exc:
                print(f"WARN: round failed: {exc}")
            time.sleep(max(10, args.loop - (time.time() - t0)))
    return one_measurement(args, repo)


if __name__ == "__main__":
    sys.exit(main())
