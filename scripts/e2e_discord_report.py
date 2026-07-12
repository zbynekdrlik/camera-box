#!/usr/bin/env python3
"""#711 — compose a Slovak, phone-readable Discord report from a full-path E2E verdict JSON.

User directive (2026-07-12, verbatim intent, issue #711): after EVERY full-path E2E run — CI PR
gate AND a manual/supervisor-driven run — a Discord notification MUST go out summarizing what the
run did and found. Before this, the user had zero visibility into test executions and once
discovered a broken leg (a muted mic) only days later. A report on every run — including an honest
RED, including honestly-skipped legs — puts the result on the user's phone every time.

This module is a PURE, fixture-tested unit: ``compose_report(verdict, meta)`` takes the
``recording-verdict --merge-partials --json`` output (the ``verdict-${RUN_ID}.json`` file
scripts/recording-e2e.sh's E2E_EXECUTE_VERDICT=1 path writes) plus a small metadata dict and
returns Slovak markdown text. No network I/O, no file I/O beyond what the CLI wrapper does — every
code path is exercised by feeding it a JSON dict, matching the project's established pure-module
pattern (scripts/switch_schedule.py, scripts/av_sync_calibrate.py).

Delivery (network POST to Discord) deliberately lives OUTSIDE this module, in
scripts/lib/e2e-discord-report.sh — it shells out to `python3 -m e2e_discord_report --json-chunks`
to get the message text, then curls it to the Discord bot API using the SAME mechanism
ci.yml / full-path-e2e.yml's existing "Discord alert" steps already use (Bot token + channel id,
#notifications). See .claude/skills/ci "Discord CI Notifications (#25)". This script never touches
DISCORD_BOT_TOKEN/DISCORD_CHANNEL_ID — reuse, not a second sender.

Report content — six sections, matching issue #711's spec verbatim:
  1. Kedy + čo bežalo (event, run id, duration, ktoré kamery)
  2. Zero frame loss per kamera — cesta do STREAMU (`full_chain.loss.camN`, the #186 headline
     burn-id-contiguity gate) A cesta do IMAG (`all_cambox_continuity.imag.segments`, per-cambox
     painted-tick continuity; falls back to the single combined `full_chain.loss.imag` node when
     the run wasn't an ALL_CAMBOX sweep).
  3. Latencia — stabilita/jitter/spread + absolútne hodnoty, `all_cambox_latency` (the SOURCE-side
     cam2→camera-capture hop, 5 cams, cam2 excluded — see .claude/skills/e2e "all_cambox_latency
     measures SOURCE-side d_X"). This hop runs BEFORE both strih and imag ever see the frame, so
     its minimum is reported as an honest FLOOR on the imag-bound latency, never claimed as the
     full camera→imag number (imag's own receive/hold time is not yet a separately measured field
     — no-overstatement).
  4. Video sync NDI kamier v strih OBS — `all_cambox_delivery_latency` (receiver-side per-camera
     hold time at strih, all 6 cams incl. cam2, #286/#624), the block the issue's own parenthetical
     ("delivery-latency spread, per-camera holds") names directly.
  5. A/V sync v stream OBS — `all_cambox_av_sync`. Every UNKNOWN is reported WITH ITS REASON:
     zero candidates -> "tichá stopa" (silent audio track, the literal phrase issue #711 requires);
     candidates present but no reliable cluster -> "nedostatok konzistentných vzoriek". NEVER a
     bare number where the data doesn't support one.
  6. Celkový verdikt (PASS/RED) + ktoré brány blokujú, named technically (never silently "red").
"""
from __future__ import annotations

import argparse
import json
import re
import sys

CAMERA_ORDER = ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"]
_CAM_KEY_RE = re.compile(r"^cam[1-6]$")

# Optional, best-effort annotations only — the technical description above each line is always
# accurate on its own; a stale/closed ticket number here never changes what is reported, it just
# stops being a useful pointer. Update when the tracked issue for a gate changes.
KNOWN_BLOCKER_HINTS = {
    "continuity_stream": "pozri #707 (all_cambox_continuity)",
    "continuity_imag": "optická judder trieda #588/#604",
    "av_sync": "pozri #689 (tichá A/V stopa) / #641 tolerancia",
}


def _g(d, *path, default=None):
    """Safe nested dict/list get -- returns `default` on any missing key or wrong type."""
    cur = d
    for key in path:
        if not isinstance(cur, dict) or key not in cur:
            return default
        cur = cur[key]
    return cur if cur is not None else default


def _fmt_ms(x, nd=1):
    if x is None:
        return "N/A"
    try:
        return f"{float(x):.{nd}f}ms"
    except (TypeError, ValueError):
        return "N/A"


def _pass_glyph(ok):
    if ok is True:
        return "✅"
    if ok is False:
        return "❌"
    return "⚪"


def _cameras_present(verdict):
    """Union of camera keys seen across the loss/latency/av blocks, in CAMERA_ORDER."""
    seen = set()
    for block_path in (
        ("full_chain", "loss"),
        ("all_cambox_latency",),
        ("all_cambox_delivery_latency",),
        ("all_cambox_av_sync",),
    ):
        block = _g(verdict, *block_path, default={})
        if isinstance(block, dict):
            seen.update(k for k in block if _CAM_KEY_RE.match(k))
    return [c for c in CAMERA_ORDER if c in seen]


def _section_header(verdict, meta):
    run_id = meta.get("run_id") or "?"
    event = meta.get("event") or "E2E beh"
    duration = meta.get("duration_secs")
    cams = _cameras_present(verdict)
    cam_list = ", ".join(c.upper() for c in cams) if cams else "?"
    lines = [f"📊 **Full-path E2E — {event}** (run {run_id})"]
    if duration:
        lines.append(f"Plánovaná dĺžka: {duration}s · Kamery v behu: {cam_list}")
    else:
        lines.append(f"Kamery v behu: {cam_list}")
    return "\n".join(lines)


def _aggregate_segments(segments, cambox_key="cambox"):
    """Aggregate a `[{cambox, pass, copies, gaps, undecodable, frames}, ...]` segment list into
    per-camera totals (a cambox may appear in several cycled segments)."""
    agg = {}
    for seg in segments or []:
        cam = str(seg.get(cambox_key, "")).lower()
        if cam not in agg:
            agg[cam] = {"pass": True, "copies": 0, "gaps": 0, "undecodable": 0, "frames": 0}
        a = agg[cam]
        a["pass"] = a["pass"] and bool(seg.get("pass"))
        a["copies"] += seg.get("copies", 0) or 0
        a["gaps"] += seg.get("gaps", 0) or 0
        a["undecodable"] += seg.get("undecodable", 0) or 0
        a["frames"] += seg.get("frames", 0) or 0
    return agg


def _section_zero_loss(verdict):
    lines = ["**1️⃣ Strata snímok (zero-loss) — per kamera**"]
    cams = _cameras_present(verdict)
    loss = _g(verdict, "full_chain", "loss", default={})

    lines.append("Cesta do STREAMU (digitálny burn, headline gate):")
    if not cams:
        lines.append("  N/A — žiadna kamera nebola v tomto behu nameraná")
    for cam in cams:
        node = loss.get(cam) or {}
        ok = node.get("zero_loss")
        real_drops = node.get("real_drops", "?")
        unreadable = node.get("burn_unreadable", "?")
        present = node.get("present_count", "?")
        expected = node.get("expected_count", "?")
        lines.append(
            f"  {_pass_glyph(ok)} {cam}: {present}/{expected} snímok, "
            f"{real_drops} stratených, {unreadable} nečitateľných"
        )

    lines.append("Cesta do IMAG:")
    imag_segments = _g(verdict, "all_cambox_continuity", "imag", "segments")
    if imag_segments:
        agg = _aggregate_segments(imag_segments)
        for cam in cams:
            a = agg.get(cam)
            if a is None:
                continue
            lines.append(
                f"  {_pass_glyph(a['pass'])} {cam}: {a['frames']} snímok, "
                f"{a['copies']} kópií, {a['gaps']} medzier, {a['undecodable']} nečitateľných"
            )
        missing = [c for c in cams if c not in agg]
        for cam in missing:
            lines.append(f"  ⚪ {cam}: nebol v žiadnom segmente na IMAG (nemeraný)")
    else:
        imag_node = loss.get("imag")
        if imag_node is None:
            lines.append("  N/A — imag nebol súčasťou tohto behu")
        else:
            ok = imag_node.get("zero_loss")
            beat_pass = imag_node.get("imag_optical_beat_pass")
            lines.append(
                f"  {_pass_glyph(ok)} spoločný signál (nie per-kamera, beh nemal ALL_CAMBOX): "
                f"burn zero_loss={ok}, optická plynulosť pass={beat_pass}"
            )
    return "\n".join(lines)


def _latency_block_lines(block, cams):
    """Returns (lines, min_of_min_ms) for a per-camera latency block (all_cambox_latency /
    all_cambox_delivery_latency shape). `min_of_min_ms` is None when nothing was measured."""
    if not block:
        return ["  N/A — nemerané v tomto behu"], None
    lines = []
    mins = []
    for cam in cams:
        node = block.get(cam)
        if not node:
            continue
        mean = node.get("mean_ms")
        jitter = node.get("jitter_ms")
        mn = node.get("min_ms")
        mx = node.get("max_ms")
        if mn is not None:
            mins.append(mn)
        lines.append(
            f"  {cam}: priemer {_fmt_ms(mean)} (min {_fmt_ms(mn)} – max {_fmt_ms(mx)}, "
            f"kolísanie {_fmt_ms(jitter)})"
        )
    spread = block.get("cross_camera_spread_ms")
    spread_pass = block.get("spread_gate_pass")
    if spread is not None:
        lines.append(
            f"  Rozptyl medzi kamerami: {_fmt_ms(spread)} → {_pass_glyph(spread_pass)}"
        )
    if mins:
        lines.append(f"  Najnižšia nameraná hodnota v tomto hope: {_fmt_ms(min(mins))}")
    return lines, (min(mins) if mins else None)


def _section_latency(verdict):
    cams = _cameras_present(verdict)
    block = _g(verdict, "all_cambox_latency", default={})
    lines = ["**2️⃣ Latencia — stabilita (cam2→zachytenie kamery, PRED strih/imag)**"]
    body_lines, floor_min = _latency_block_lines(block, [c for c in cams if c != "cam2"])
    lines.extend(body_lines)
    if floor_min is not None:
        lines.append(
            f"  ↳ Minimálna nameraná latencia smerom k IMAG (dolná hranica, imag k tomu "
            f"pridáva vlastné spracovanie navyše): {_fmt_ms(floor_min)}"
        )
    else:
        lines.append("  ↳ Minimálna latencia smerom k IMAG: N/A — nemerané v tomto behu")
    return "\n".join(lines)


def _section_video_sync(verdict):
    cams = _cameras_present(verdict)
    block = _g(verdict, "all_cambox_delivery_latency", default={})
    lines = ["**3️⃣ Video sync NDI kamier v strih OBS (doručovacia latencia, per-kamera hold)**"]
    body_lines, _floor_min = _latency_block_lines(block, cams)
    lines.extend(body_lines)
    return "\n".join(lines)


def _av_reason(node):
    """Honest, specific reason string for a non-'measured' A/V verdict — never a bare UNKNOWN."""
    verdict = node.get("verdict")
    candidates = node.get("candidates", 0) or 0
    cluster_samples = node.get("cluster_samples", 0) or 0
    if verdict == "measured":
        return None
    if candidates == 0:
        return "tichá stopa"
    if cluster_samples == 0:
        return "nedostatok konzistentných vzoriek"
    return "nedostatok okien na spoľahlivé meranie"


def _section_av_sync(verdict):
    lines = ["**4️⃣ A/V synchronizácia (stream OBS)**"]
    block = _g(verdict, "all_cambox_av_sync", default={})
    cams = _cameras_present(verdict)
    if not block:
        lines.append("  N/A — A/V sync nebol súčasťou tohto behu (bez --switch-schedule)")
        return "\n".join(lines)

    all_silent = True
    for cam in cams:
        node = block.get(cam)
        if not node:
            continue
        if (node.get("candidates") or 0) > 0:
            all_silent = False
        if node.get("verdict") == "measured":
            offset = node.get("av_offset_ms")
            mad = node.get("mad_ms")
            matched = node.get("cluster_samples")
            lines.append(
                f"  {_pass_glyph(node.get('gate_pass'))} {cam}: offset {_fmt_ms(offset)}, "
                f"MAD {_fmt_ms(mad)}, zhody={matched}"
            )
        else:
            reason = _av_reason(node)
            lines.append(f"  ⚪ {cam}: UNKNOWN — {reason}")

    if all_silent and cams:
        lines.append("  ↳ A/V: UNKNOWN — tichá stopa (žiadna kamera nezachytila zvukovú značku)")

    tolerance = block.get("gate_tolerance_ms")
    gate_pass = block.get("gate_pass")
    if tolerance is not None:
        lines.append(f"  Tolerancia: ±{_fmt_ms(tolerance)} · Brána: {_pass_glyph(gate_pass)}")
    return "\n".join(lines)


def _section_overall(verdict, meta):
    overall = verdict.get("overall_pass")
    lines = [f"**5️⃣ Celkový verdikt: {'✅ PASS' if overall else '❌ RED'}**"]
    blockers = []

    cont_pass = _g(verdict, "all_cambox_continuity", "overall_pass")
    if cont_pass is False:
        blockers.append(
            f"Kontinuita medzi kamerami (stream): FAIL — {KNOWN_BLOCKER_HINTS['continuity_stream']}"
        )
    imag_cont_pass = _g(verdict, "all_cambox_continuity", "imag", "overall_pass")
    if imag_cont_pass is False:
        blockers.append(
            f"Kontinuita/plynulosť na IMAG: FAIL — {KNOWN_BLOCKER_HINTS['continuity_imag']}"
        )
    elif imag_cont_pass is None:
        legacy_imag_pass = _g(verdict, "full_chain", "loss", "imag", "imag_optical_beat_pass")
        if legacy_imag_pass is False:
            blockers.append(
                f"Plynulosť na IMAG (optický beat): FAIL — {KNOWN_BLOCKER_HINTS['continuity_imag']}"
            )
    av_gate = _g(verdict, "all_cambox_av_sync", "gate_pass")
    if av_gate is False:
        blockers.append(f"A/V synchronizácia: FAIL — {KNOWN_BLOCKER_HINTS['av_sync']}")
    chain_zero = _g(verdict, "full_chain", "zero_loss")
    if chain_zero is False:
        blockers.append("Reťazec do streamu (headline zero-loss): FAIL")

    if blockers:
        lines.append("Blokujúce brány:")
        lines.extend(f"  • {b}" for b in blockers)
    elif overall:
        lines.append("Všetky brány prešli — žiadny blokujúci nález.")
    else:
        lines.append("RED, ale žiadna konkrétna brána nebola rozpoznaná — pozri celý JSON.")

    gate_exit = meta.get("gate_exit")
    if gate_exit is not None:
        lines.append(f"(merge recording-verdict exit code: {gate_exit})")
    return "\n".join(lines)


def compose_report(verdict: dict, meta: dict | None = None) -> str:
    """Pure: verdict JSON dict + small meta dict -> Slovak markdown report text.

    `meta` keys (all optional): run_id, event ("CI PR gate" | "manuálny beh (recording-e2e.sh)" |
    any free label), duration_secs, gate_exit (the merge recording-verdict process exit code).
    """
    meta = meta or {}
    sections = [
        _section_header(verdict, meta),
        _section_zero_loss(verdict),
        _section_latency(verdict),
        _section_video_sync(verdict),
        _section_av_sync(verdict),
        _section_overall(verdict, meta),
    ]
    return "\n\n".join(sections)


def chunk_for_discord(text: str, limit: int = 1900) -> list[str]:
    """Pure: split `text` into Discord-message-sized chunks (<=`limit` chars each), breaking ONLY
    at paragraph boundaries (`\\n\\n`) so a section is never cut mid-sentence. A single paragraph
    longer than `limit` is kept whole (Discord will reject it, but that never silently truncates
    real content — better a loud failure than a corrupted number)."""
    paragraphs = text.split("\n\n")
    chunks: list[str] = []
    current = ""
    for para in paragraphs:
        candidate = f"{current}\n\n{para}" if current else para
        if len(candidate) <= limit:
            current = candidate
        else:
            if current:
                chunks.append(current)
            current = para
    if current:
        chunks.append(current)
    return chunks


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--json", required=True, help="path to the verdict-<run_id>.json file")
    ap.add_argument("--run-id", default=None)
    ap.add_argument("--event", default="E2E beh")
    ap.add_argument("--duration", type=int, default=None, dest="duration_secs")
    ap.add_argument("--gate-exit", type=int, default=None, dest="gate_exit")
    ap.add_argument(
        "--json-chunks",
        action="store_true",
        help="print a JSON array of Discord-sized message chunks instead of the raw text",
    )
    args = ap.parse_args(argv)

    with open(args.json, encoding="utf-8") as f:
        verdict = json.load(f)

    meta = {
        "run_id": args.run_id,
        "event": args.event,
        "duration_secs": args.duration_secs,
        "gate_exit": args.gate_exit,
    }
    text = compose_report(verdict, meta)
    if args.json_chunks:
        print(json.dumps(chunk_for_discord(text)))
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
