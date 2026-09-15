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
     bare number where the data doesn't support one. #714: a camera whose OWN per-window pooling
     was sample-starved (structurally, per-camera windows are too short to accumulate enough real
     QPSK marker matches — see av_window::derive_camera_av_sync's own doc comment) but whose
     offset could be soundly ESTIMATED (verdict=="derived", from cam2's own measured offset +
     this camera's own #286 delivery-latency delta) is reported as "ODVODENÉ <value>", always
     visually distinct from a real "measured" number — satisfying the #714 acceptance bar (a value
     or a reasoned bound for EVERY camera, never a silent "cam2 only").
  6. Celkový verdikt (PASS/RED) + ktoré brány blokujú, named technically (never silently "red").
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys

CAMERA_ORDER = ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7"]

# #757 (2026-07-15, binding user directive): imag's fixed floor -- mirrors
# imag_latency_enforce.IMAG_FIXED_LATENCY_MS (kept as its own literal, not a cross-module
# import, so this stays a dependency-free PURE formatter).
IMAG_FIXED_LATENCY_MS = 3
_CAM_KEY_RE = re.compile(r"^cam[1-7]$")

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


def _pin_or_na(v):
    return f"{v:.0f}ms" if isinstance(v, (int, float)) else "N/A"


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
            agg[cam] = {
                "pass": True,
                "copies": 0,
                "gaps": 0,
                "undecodable": 0,
                "frames": 0,
                # issue 1144 -- a per-cam flag: did any of this cam's segments carry a switch-in
                # transient (a raw content FAIL that the imag content gate excuses / attributes to
                # cold-cut)? The raw `pass` glyph stays honest; the imag rendering annotates it so an
                # excused ❌ on the detailed view is explained (report-only, matches overall_pass).
                "switch_in_transient": False,
            }
        a = agg[cam]
        a["pass"] = a["pass"] and bool(seg.get("pass"))
        a["copies"] += seg.get("copies", 0) or 0
        a["gaps"] += seg.get("gaps", 0) or 0
        a["undecodable"] += seg.get("undecodable", 0) or 0
        a["frames"] += seg.get("frames", 0) or 0
        a["switch_in_transient"] = a["switch_in_transient"] or bool(
            seg.get("switch_in_transient")
        )
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
            sit_note = (
                " (switch-in transient → cold-cut, report-only)"
                if a.get("switch_in_transient")
                else ""
            )
            lines.append(
                f"  {_pass_glyph(a['pass'])} {cam}: {a['frames']} snímok, "
                f"{a['copies']} kópií, {a['gaps']} medzier, {a['undecodable']} nečitateľných"
                f"{sit_note}"
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


def _av_reason(node, av_audio_silent=None):
    """Honest, specific reason string for a non-'measured' A/V verdict — never a bare UNKNOWN.

    `av_audio_silent` is the block-level #748 discriminator: when it is explicitly False the
    measurement audio was PRESENT (the demod saw preamble energy), so a `candidates == 0` camera
    means the marker never decoded — not a silent track. Keeps the per-camera line consistent with
    the block summary instead of contradicting it with "tichá stopa"."""
    verdict = node.get("verdict")
    candidates = node.get("candidates", 0) or 0
    cluster_samples = node.get("cluster_samples", 0) or 0
    if verdict == "measured":
        return None
    if candidates == 0:
        if av_audio_silent is False:
            return "značka nedekódovaná (zvuk je prítomný)"
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
        elif node.get("verdict") == "derived":
            # #714: a camera whose OWN per-window pooling was sample-starved, but whose offset
            # could be soundly ESTIMATED from cam2's own measured offset + this camera's own
            # #286 delivery-latency delta — always labeled distinctly from a real measurement.
            offset = node.get("derived_offset_ms")
            spread = node.get("derived_delivery_spread_ms")
            lines.append(
                f"  {_pass_glyph(node.get('gate_pass'))} {cam}: ODVODENÉ {_fmt_ms(offset)} "
                f"(z cam2 + vlastný doručovací rozdiel, rozptyl ±{_fmt_ms(spread)})"
            )
        elif node.get("verdict") == "excluded":
            # #855: an operator-acknowledged offline box (CAMBOX_OFFLINE_ACK / rig-fleet.txt) —
            # reported EXCLUDED, never as a judged UNKNOWN, so it never reads as "this box failed".
            reason = node.get("exclude_reason") or "operátorom potvrdené offline"
            lines.append(f"  ⏸️ {cam}: VYNECHANÉ (potvrdené offline — {reason})")
        else:
            reason = _av_reason(node, block.get("av_audio_silent"))
            lines.append(f"  ⚪ {cam}: UNKNOWN — {reason}")

    if all_silent and cams:
        # #748: never leave the operator with a bare "unknown" — say WHICH chain link to check,
        # and blame the RIGHT one. The verdict's `av_audio_silent` discriminator (fed by the QPSK
        # demod's whole-recording preamble-onset count, av_window::classify_av_audio_state)
        # separates a genuinely silent mbc chain from present-but-undecoded audio:
        #   None/absent/True -> the safe, loud default: the measurement audio is (or may be) SILENT
        #                       -> check the mbc mute + Dante routing.
        #   False            -> the demod SAW preamble energy: audio is PRESENT, the marker just
        #                       never clustered -> the problem is the QPSK marker / emit (cam2
        #                       painter) side, NOT an mbc mute.
        if block.get("av_audio_silent") is False:
            lines.append(
                "  ↳ A/V: UNKNOWN — žiadna kamera nedekódovala značku (merací zvuk je PRÍTOMNÝ, nie tichý)"
            )
            lines.append(
                "  ⚠️ A/V ZNAČKA sa NEDEKÓDOVALA, hoci merací zvuk je prítomný — problém je v QPSK značke / emit strane (cam2 painter), NIE mute mbc (#748)"
            )
        else:
            lines.append(
                "  ↳ A/V: UNKNOWN — tichá stopa (žiadna kamera nezachytila zvukovú značku)"
            )
            lines.append(
                "  ⚠️ MERACÍ ZVUK TICHÝ — skontroluj mbc Ableton kanál (mute) + Dante routing do stream OBS (#748)"
            )

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


def _section_presentation_cadence(verdict):
    """#726 -- presentation-cadence EVENNESS, REPORTED only (not yet gate-enforced; the threshold
    is not calibrated against a known-healthy run -- see src/presentation_cadence.rs). Sourced
    from all_cambox_continuity.segments[] (STRIH's OWN per-cambox sweep, not .imag.segments) --
    populated only for cam2's own window(s), the only place with a continuously-decodable painted
    tick. This is the "smooth 30 = uniform 2-tick spacing; 15-like = paired spacing" number the
    2026-07-12 live-event stutter needed and the pre-existing gates were blind to.

    Deliberately NOT numbered (1-5) so it never shifts the existing headline sections' numbering.
    """
    lines = ["**Plynulosť obrazu na strih (informatívne, #726)**"]
    segments = _g(verdict, "all_cambox_continuity", "segments", default=[]) or []
    cadence_segments = [s for s in segments if isinstance(s, dict) and s.get("presentation_cadence")]
    if not cadence_segments:
        lines.append("  N/A — nemerané v tomto behu (chýba okno s namaľovaným tikom, napr. cam2)")
        return "\n".join(lines)
    for seg in cadence_segments:
        pc = seg["presentation_cadence"]
        score = pc.get("evenness_score")
        dup = pc.get("duplicate_steps", 0)
        paired = pc.get("paired_events", 0)
        total = pc.get("sample_deltas", 0)
        cam = seg.get("cambox", "?")
        pct = f"{score * 100:.0f}%" if score is not None else "N/A"
        lines.append(
            f"  {cam}: rovnomernosť {pct} ({dup} zdvojených z {total} snímok, {paired} "
            f"spárovaných udalostí 'drž a dobehni' — signatúra '15fps' pri 30fps plátne)"
        )
    return "\n".join(lines)


def _section_latency_pins(verdict, meta):
    """#756 Member 3 -- the user's REPEATED, previously-unmet request: per-camera CONFIGURED
    genlock latency pins (live-read over WS at run time, never hardcoded) next to this run's
    OWN measured delivery p50, plus the computed RECOMMENDED pin set for the next iteration.
    The fused E2E's own program scene-switching already measures every source's delivery -- this
    turns that into the per-source latency calibrator the user has asked for repeatedly
    ("vysledok syncu a nastavenych latencii pre jednotlive cam sourcy... sucastou velkeho e2e
    testu").

    `meta["pins"]` (optional -- this whole section is skipped, never fabricated, when absent) is
    a dict gathered by scripts/latency_pins_snapshot.py (a separate, impure, WS-driven script --
    this function stays a PURE formatter, no network/subprocess I/O, matching compose_report's
    own contract):
        {
          "strih": {"cam1": {"main_ms": 3, "mv_ms": 3}, ...},   # live GetInputSettings reads
          "imag":  {"cam1": {"main_ms": 3, "mv_ms": 3}, ...},   # (both optional per-box)
          "stream_hold_active_ms": 952,                          # live 'NDI 2ME PGM' pin
          "av_sync_last": {"applied_latency_ms": 952, "offset_ms": -7.24, "source": "NDI 2ME PGM",
                            "calibrated_at": "..."},              # ~/.camera-box/av-sync-last.json
          "recommended_pins_ms": {"cam1": 3, "cam2": 13, ...}     # phase-sync-gate output,
                                                                    # computed from THIS run's
                                                                    # own delivery p50 table
        }
    A camera/box missing from `pins` is reported N/A for that cell, never silently omitted from
    the table -- the user asked to flag mismatches loudly, not hide gaps.

    **#757 (2026-07-15, binding user directive): imag is FIXED-3ms-ALWAYS, never per-camera
    equalized** -- per-camera pin equalization is a STRIH-only concept (see
    scripts/imag_latency_enforce.py). So imag no longer gets a per-camera cell in the loop below;
    instead it gets ONE summary line after the per-camera table: "všetky 3 (fixné, IMAG=min
    latencia)" when every imag pin this run actually reads back as 3ms, or a LOUD per-camera
    drift warning otherwise (imag_latency_enforce.py should have already self-healed this before
    the report ran -- seeing drift HERE means that step itself failed or didn't run).
    """
    pins = meta.get("pins")
    if not pins:
        return None  # never fabricated -- this run didn't gather a pins snapshot

    cams = _cameras_present(verdict) or list(CAMERA_ORDER)
    delivery = _g(verdict, "all_cambox_delivery_latency", default={})
    strih_pins = pins.get("strih") or {}
    imag_pins = pins.get("imag") or {}
    recommended = pins.get("recommended_pins_ms") or {}

    lines = ["**Nastavené latencie per kamera (živé z WS, nie napevno) + odporúčanie**"]
    any_mismatch = False
    for cam in cams:
        s = strih_pins.get(cam) or {}
        s_main, s_mv = s.get("main_ms"), s.get("mv_ms")
        p50 = _g(delivery, cam, "p50_ms")
        rec = recommended.get(cam)

        mismatch = False
        if s_main is not None and s_mv is not None and s_main != s_mv:
            mismatch = True
        any_mismatch = any_mismatch or mismatch

        strih_str = f"strih={_pin_or_na(s_main)}" if s_main is not None else "strih=N/A"
        if s_mv is not None and s_mv != s_main:
            strih_str += f"(MV={_pin_or_na(s_mv)}!)"

        flag = " ⚠️ PARITA main≠MV" if mismatch else ""
        rec_str = f", odporúčané={_pin_or_na(rec)}" if rec is not None else ""
        lines.append(
            f"  {'⚠️' if mismatch else '•'} {cam}: {strih_str}, "
            f"p50 tento beh={_pin_or_na(p50)}{rec_str}{flag}"
        )

    if imag_pins:
        drifted = [
            cam
            for cam in cams
            if (imag_pins.get(cam) or {}).get("main_ms") not in (None, IMAG_FIXED_LATENCY_MS)
            or (imag_pins.get(cam) or {}).get("mv_ms") not in (None, IMAG_FIXED_LATENCY_MS)
        ]
        if drifted:
            lines.append(
                f"  ⚠️ imag NEODCHÝLENÉ od pevných {IMAG_FIXED_LATENCY_MS}ms na: "
                + ", ".join(
                    f"{cam}=main:{_pin_or_na((imag_pins.get(cam) or {}).get('main_ms'))}"
                    f"/mv:{_pin_or_na((imag_pins.get(cam) or {}).get('mv_ms'))}"
                    for cam in drifted
                )
                + " (imag_latency_enforce.py malo toto opraviť pred behom)"
            )
        else:
            lines.append(
                f"  imag: všetky {IMAG_FIXED_LATENCY_MS} (fixné, IMAG=min latencia)"
            )

    stream_active = pins.get("stream_hold_active_ms")
    av_last = pins.get("av_sync_last") or {}
    applied = av_last.get("applied_latency_ms")
    lines.append(
        f"  Stream 'NDI 2ME PGM' hold: živé z WS={_pin_or_na(stream_active)}, "
        f"posledný zdroj pravdy (av-sync-last.json)={_pin_or_na(applied)}"
        + (
            " ⚠️ NEZHODA — box beží s iným holdom, než je zapísaný ako naposledy kalibrovaný"
            if (
                stream_active is not None
                and applied is not None
                and stream_active != applied
            )
            else ""
        )
    )
    if any_mismatch:
        lines.append(
            "  ⚠️ main≠MV klon nesie inú latenciu ako hlavný zdroj tej istej kamery — "
            "monitorovací obraz (multiview) potom sedí inak ako program (parity violation)"
        )
    return "\n".join(lines)


def _section_mv_skew(verdict, meta):
    """#761 -- per-camera MV-clone-vs-main presentation skew (scene 'MV Cam N' vs 'Cam N'), from
    scripts/mv_skew_snapshot.py's order-alternated, t_send-compensated painter-QR measurement.

    `meta["mv_skew"]` (optional -- this whole section is skipped, never fabricated, when absent) is
    that gatherer's JSON: {"cameras": {"camN": {"median_ms", "n_samples", "stdev_ms", "alarming",
    ...}}, "frame_ms": 16.67, "error"?: "..."}. A camera with no decodable QR is an honest N/A
    (never a fabricated 0). Both strih and imag are shared-source (scene 'MV Cam N' draws the SAME
    input as 'Cam N') so the expected skew is ~0 -- this is a REGRESSION GUARD: |median| > 1 frame
    means the multiview cell the operator sees presents at a different time than the program."""
    mv = meta.get("mv_skew")
    if not mv:
        return None  # never fabricated -- this run didn't gather an MV-skew snapshot

    frame_ms = mv.get("frame_ms") or (1000.0 / 60.0)
    lines = ["**MV klon vs. program — prezentačný skew (#761, živé WS screenshoty)**"]
    if mv.get("error"):
        lines.append(f"  ⚠️ nemeralo sa: {mv['error']}")
        return "\n".join(lines)

    cams = mv.get("cameras") or {}
    if not cams:
        lines.append("  (žiadna aktívna kamera nemala scény 'Cam N' + 'MV Cam N' na meranie)")
        return "\n".join(lines)

    any_alarm = False
    for cam in sorted(cams):
        node = cams.get(cam) or {}
        median = node.get("median_ms")
        n = node.get("n_samples") or 0
        stdev = node.get("stdev_ms")
        if median is None:
            reason = node.get("note") or "žiadny dekódovateľný QR"
            lines.append(f"  • {cam}: N/A ({reason})")
            continue
        alarming = bool(node.get("alarming"))
        any_alarm = any_alarm or alarming
        spread = f", ±{stdev:.0f} ms" if isinstance(stdev, (int, float)) else ""
        glyph = "⚠️" if alarming else "•"
        line = f"  {glyph} {cam}: {median:+.1f} ms (n={n}{spread})"
        if alarming:
            direction = "neskôr" if median > 0 else "skôr"
            line += f" — strihač vidí {cam} o {abs(median):.0f} ms {direction} než program"
        lines.append(line)

    lines.append(
        f"  Prah poplachu: |skew| > 1 snímka ({frame_ms:.1f} ms @60fps). Shared-source usporiadanie "
        "('MV Cam N' = ten istý vstup ako 'Cam N') => očakávaj ~0; toto je regresný strážca."
    )
    if any_alarm:
        lines.append(
            "  ⚠️ nenulový skew = multiview bunka NEsedí časovo s programom (možný samostatný "
            "dekód klonu) — over usporiadanie scén na imag (#761)."
        )
    return "\n".join(lines)


def _section_residual_events(verdict):
    """#707 EVENT-FORENSICS -- the per-event residual copy/gap breakdown (src/residual_events.rs),
    surfaced per the user's binding #707 decision ("every residual deviation must have its own
    documented reason"). Reads the flat `all_cambox_continuity.residual_events` list (each event
    optionally carries a `reason` once a human/tool has investigated it -- absent means still
    OPEN, never silently assumed benign). Falls back to walking `segments[].residual_events` for
    an older verdict JSON that predates the flattened top-level list. Returns None (no line) when
    this run carried NO --switch-schedule sweep at all (`all_cambox_continuity` absent, or present
    with no `segments`/`residual_events` key at all) -- never a spurious "0/0" for a run where the
    metric plain doesn't apply. A genuinely CLEAN sweep (the block ran, found zero events) DOES
    report "0 s dôkazmi / 0 otvorených" -- that is a real, useful "swept clean" signal, distinct
    from "never swept".
    """
    events = _g(verdict, "all_cambox_continuity", "residual_events", default=None)
    if events is None:
        segments = _g(verdict, "all_cambox_continuity", "segments", default=None)
        if segments is None:
            return None  # no ALL_CAMBOX sweep at all in this run
        events = []
        for seg in segments:
            if isinstance(seg, dict):
                events.extend(seg.get("residual_events") or [])
    with_reason = sum(1 for e in events if isinstance(e, dict) and e.get("reason"))
    open_count = len(events) - with_reason
    return (
        "**Odchýlky s dôvodmi (#707 forenzný rozbor)**\n"
        f"  Odchýlky s dôvodmi: {with_reason} s dôkazmi / {open_count} otvorených"
    )


# ===========================================================================
# #1127 — the REDESIGNED, phone-readable Discord SUMMARY (owner directive 2026-08-19).
#
# The owner's complaint: the per-run Discord report is a multi-page wall — "ani test pass neviem
# najst" — and a PASS run shows ❌ on REPORT-ONLY metrics (run 1104689227: overall_pass=true, yet
# ❌ on the imag leg + the 84.7ms delivery-side spread), so the verdict is impossible to find.
#
# The new summary is verdict-FIRST, tiny, and NEVER renders a report-only metric as ❌:
#   PASS  -> exactly 3 lines: verdict, one zero-loss summary, one link to the CI run/artifact.
#   FAIL  -> verdict + ONLY the failing BLOCKING gates (one line each, plain Slovak + #1117
#            ownership), then at most ONE collapsed "ℹ️ sledované (negatuje verdikt)" line.
#
# BLOCKING-vs-REPORT-ONLY is DERIVED from the verdict JSON's own gate semantics, mirroring how
# src/bin/recording-verdict.rs folds `all_pass`: a LIVE-toggleable seam blocks only while its own
# node carries `gates_overall_pass=true`; a report-only seam ships `gates_overall_pass=false` and
# is never a blocking failure. The full detail moves OUT of Discord — it stays the plain-text /
# CI-log rendering (`compose_report`, unchanged) and lives in the uploaded verdict JSON artifact.
# ===========================================================================

_OWN_CLAUDE = "Rieši Claude."


def _fmt_duration(secs):
    """`None` -> None (verdict line omits duration); else a human "45s" / "5m 25s"."""
    if secs is None:
        return None
    try:
        secs = int(secs)
    except (TypeError, ValueError):
        return None
    if secs < 0:
        return None
    if secs < 60:
        return f"{secs}s"
    return f"{secs // 60}m {secs % 60}s"


def _camera_plural(n):
    """Slovak count-noun for cameras: 1 kamera, 2-4 kamery, 0/5+ kamier."""
    if n == 1:
        return "kamera"
    if 2 <= n <= 4:
        return "kamery"
    return "kamier"


def _upper_join(cams):
    return ", ".join(str(c).upper() for c in cams)


def _order_nodes(keys):
    """Stable, readable order for full_chain.loss node keys: cameras first (CAMERA_ORDER), then the
    strih/stream aggregate nodes, then anything else."""
    order = {c: i for i, c in enumerate(CAMERA_ORDER)}
    order["strih"], order["stream"] = 90, 91
    return sorted(keys, key=lambda k: order.get(k, 80))


def _stream_drop_total(verdict, cams):
    """Total real frames lost on the camera->stream path: per-camera digital-burn real_drops plus
    any V4L2 capture-card drops. On a PASS this is 0 (headline zero-loss held)."""
    loss = _g(verdict, "full_chain", "loss", default={}) or {}
    total = 0
    for cam in cams:
        rd = _g(loss, cam, "real_drops")
        if isinstance(rd, (int, float)):
            total += rd
    for key, node in loss.items():
        if key.startswith("cam2_") and isinstance(node, dict):
            v4 = node.get("v4l2_dropped")
            if isinstance(v4, (int, float)):
                total += v4
    return int(total)


def _blocking_failures(verdict):
    """Ordered list of `(label, ownership)` for every BLOCKING gate that FAILED in this verdict.

    'Blocking' == folds into recording-verdict.rs's `all_pass` and is LIVE today. A LIVE-toggleable
    seam is honored via its own `gates_overall_pass` field (so if a seam is ever flipped to
    report-only, this report auto-follows). The report-only seams (imag leg PER-FRAME CONTENT,
    cold_cut, lipsync — dup_cadence LIVE since issue 1166; frozen_leg/self_heal LIVE since issue
    905 item 2; the optical undecodable floor LIVE since issue 905 item 3) are NEVER returned here
    — see `_report_only_tripped`. Ownership follows the #1117
    convention: agent-recoverable ->
    "Rieši Claude."; a genuine physical fault (capture card, silent mbc chain) -> a "Treba fyzicky
    skontrolovať …" human step.

    KEEP IN SYNC with recording-verdict.rs's `all_pass &= …` folds (grep `all_pass` there): this
    classifier hand-mirrors that fold list (unavoidable at Tier-0 — python cannot import the Rust).
    A blocking gate added there but missed here degrades to the compose_summary safety-net line (a
    FAIL is never hidden, just unnamed), so add its branch when a fold changes."""
    out = []
    loss = _g(verdict, "full_chain", "loss", default={}) or {}

    # 1) Zero-loss into stream (headline). Every full_chain.loss node with zero_loss=False EXCEPT the
    #    imag leg (report-only -> _report_only_tripped) and the cam2_* V4L2 capture nodes (physical,
    #    item 2). Covers per-camera digital burn (cam1..7) AND the strih/stream aggregate delivery
    #    nodes (recording-verdict.rs :3795 `all_pass &= is_zero_within_allowance && span_ok`).
    zl_fail = [
        key for key, node in loss.items()
        if isinstance(node, dict) and key != "imag" and not key.startswith("cam2_")
        and node.get("zero_loss") is False
    ]
    if zl_fail:
        out.append((
            f"Strata snímok na ceste do stream OBS (zero-loss): ZLYHALA — {_upper_join(_order_nodes(zl_fail))}",
            _OWN_CLAUDE,
        ))

    # 2) V4L2 capture-card drops on the camera leg (`full_chain.loss.cam2_*`) — a physical fault.
    #    Label from the cam2_<label> KEY: the node's verbose `source` sentence ("… V4L2 sequence-gap
    #    capture-drop (camera leg) — not a painter-tick compare") is exactly the wall #1127 kills.
    cap_fail = [
        key[len("cam2_"):] for key, node in loss.items()
        if key.startswith("cam2_") and isinstance(node, dict) and node.get("zero_loss") is False
    ]
    if cap_fail:
        out.append((
            f"Snímková strata na zachytení z kamery (V4L2 capture-karta): ZLYHALA — {_upper_join(cap_fail)}",
            "Treba fyzicky skontrolovať (kamera / kábel / capture-karta).",
        ))

    # 3) burn_hold — per-hop repeat / max-hold. LIVE seam (recording-verdict.rs :3868), JSON at
    #    full_chain.loss.<node>.hold (within_bound + gates_overall_pass).
    hold_fail = [
        key for key, node in loss.items()
        if isinstance(node, dict) and isinstance(node.get("hold"), dict)
        and node["hold"].get("within_bound") is False
        and node["hold"].get("gates_overall_pass") is True
    ]
    if hold_fail:
        out.append((
            f"Opakovanie/držanie snímky (burn max-hold) na {_upper_join(_order_nodes(hold_fail))}: ZLYHALO",
            _OWN_CLAUDE,
        ))

    # 4) Per-cambox STREAM continuity (copies/gaps/undecodable per window) — BLOCKING unconditionally
    #    (`all_pass &= seg.overall_pass`, aggregated into all_cambox_continuity.overall_pass).
    if _g(verdict, "all_cambox_continuity", "overall_pass") is False:
        tol = _g(verdict, "all_cambox_continuity", "copies_gaps_tolerance", default=0) or 0
        segs = _g(verdict, "all_cambox_continuity", "segments", default=[]) or []
        over = []
        for s in segs:
            if not isinstance(s, dict):
                continue
            # #1251: each window is judged against ITS OWN applied tolerance (a per-cambox override
            # like CAM2 -> 25 while its grabber HW is sick, issue 1249). Fall back to the run-wide
            # default for verdicts predating the per-segment field, so old runs classify unchanged.
            seg_tol = s.get("copies_gaps_tolerance", tol)
            if seg_tol is None:
                seg_tol = tol
            if (s.get("copies", 0) or 0) > seg_tol or (s.get("gaps", 0) or 0) > seg_tol:
                cb = str(s.get("cambox", "")).strip()
                if cb and cb not in over:
                    over.append(cb)
        # issue 905 item 3: the re-gated optical undecodable floor (per-window AND run-wide) gates
        # again. Name a floor red — run-wide OR a single window over the per-window floor — so it is
        # attributable, not folded into the generic fallback. A copies/gaps red and a floor red can
        # BOTH be true in one run, so append BOTH details rather than picking one (review 🔵2).
        floor_gates = _g(verdict, "all_cambox_continuity",
                         "undecodable_floor_gates_overall_pass") is True
        run_wide_over = _g(verdict, "all_cambox_continuity",
                           "run_wide_undecodable_within_floor") is False
        # per-window floor: read the serialized value, fall back to 4 for pre-#905 verdicts.
        pw_floor = _g(verdict, "all_cambox_continuity", "per_window_undecodable_floor", default=4)
        if pw_floor is None:
            pw_floor = 4
        per_window_over = any(
            isinstance(s, dict) and (s.get("frames", 0) or 0) > 0
            and (s.get("undecodable", 0) or 0) > pw_floor
            for s in segs
        )
        parts = []
        if over:
            parts.append(f"{', '.join(over)} (strata/duplicita snímok nad toleranciou)")
        if floor_gates and (run_wide_over or per_window_over):
            parts.append("optická čitateľnosť — nečitateľné snímky nad floor")
        if parts:
            detail = " — " + "; ".join(parts)
        else:
            # A segment can fail for a non-copies/gaps reason (empty schedule / frame_count==0,
            # recording-verdict.rs :4638) — don't claim the wrong cause.
            detail = " (chyba kontinuity — pozri CI log)"
        out.append((f"Plynulosť/kontinuita v stream OBS: ZLYHALA{detail}", _OWN_CLAUDE))

    # 5) A/V synchronizácia (stream OBS) — LIVE seam (`av_window::gates_overall_pass()==true`).
    av = _g(verdict, "all_cambox_av_sync", default={}) or {}
    if av.get("gate_pass") is False and av.get("gates_overall_pass") is True:
        if av.get("av_audio_silent") is True:
            out.append((
                "A/V synchronizácia (stream OBS): ZLYHALA — merací zvuk je tichý",
                "Treba fyzicky skontrolovať (mbc mute / Dante routing do stream OBS).",
            ))
        else:
            out.append((
                "A/V synchronizácia (stream OBS): ZLYHALA — offset mimo tolerancie",
                _OWN_CLAUDE,
            ))

    # 6) Absolute cam->strih p99 latency bound — LIVE seam (e2e_latency_gate::gates_overall_pass).
    lg = _g(verdict, "latency", "cam_strih_gate", default={}) or {}
    if lg.get("pass") is False and lg.get("gates_overall_pass") is True:
        p99 = lg.get("p99_ms")
        bound = lg.get("bound_p99_ms")
        detail = f" (p99 {_fmt_ms(p99)} nad limitom {_fmt_ms(bound)})" if p99 is not None else ""
        out.append((f"Absolútna latencia kamera→strih: ZLYHALA{detail}", _OWN_CLAUDE))

    # 7) SOURCE cross-camera latency spread — BLOCKING unconditionally (`all_pass &= sv.pass`).
    if _g(verdict, "all_cambox_latency", "spread_gate_pass") is False:
        spread = _g(verdict, "all_cambox_latency", "cross_camera_spread_ms")
        detail = f" ({_fmt_ms(spread)})" if spread is not None else ""
        out.append((f"Rozptyl latencie medzi kamerami (zdroj): ZLYHAL{detail}", _OWN_CLAUDE))

    # 8) Cadence-judder gate — LIVE seam (presentation_cadence::gates_overall_pass).
    cj = _g(verdict, "all_cambox_continuity", "cadence_judder_gate", default={}) or {}
    if cj.get("pass") is False and cj.get("gates_overall_pass") is True:
        out.append(("Rovnomernosť obrazu (judder 15↔30 fps): ZLYHALA", _OWN_CLAUDE))

    # 9) Cadence-UNIFORMITY floor — #1142 NEW LIVE seam
    #    (presentation_cadence::uniformity_gates_overall_pass). Only fires on a #1142-shape verdict
    #    that carries the block with gates_overall_pass=true.
    cu = _g(verdict, "all_cambox_continuity", "cadence_uniformity_gate", default={}) or {}
    if cu.get("pass") is False and cu.get("gates_overall_pass") is True:
        worst = cu.get("worst_uniform_fraction")
        detail = f" (najhoršia rovnomernosť {worst:.2f})" if isinstance(worst, (int, float)) else ""
        out.append((f"Rovnomernosť obrazu (plynulý pohyb 60→30): ZLYHALA{detail}", _OWN_CLAUDE))

    # 10) DELIVERY-side cross-camera spread — #1142: now BLOCKING (delivery_spread_gate::
    #     gates_overall_pass). The SOURCE-side spread (item 7) was always blocking; #1142 makes the
    #     DELIVERY side block too. Only fires on a #1142-shape verdict (gates_overall_pass=true).
    dl = _g(verdict, "all_cambox_delivery_latency", default={}) or {}
    if dl.get("spread_gate_pass") is False and dl.get("gates_overall_pass") is True:
        spread = dl.get("cross_camera_spread_ms")
        detail = f" ({_fmt_ms(spread)})" if spread is not None else ""
        out.append((f"Rozptyl doručovacej latencie medzi kamerami (strih): ZLYHAL{detail}", _OWN_CLAUDE))

    # 11) IMAG PRESENCE/VERIFICATION — #1142: now BLOCKING (imag_leg_gate::gates_overall_pass). A run
    #     that silently skipped the imag leg (or dropped it via the schema-degrade) REDs, UNLESS imag
    #     is operator-offline-acked (the ONE sanctioned skip). The PER-FRAME CONTENT terms stay
    #     report-only (→ _report_only_tripped). Only fires on a #1142-shape verdict.
    fc = _g(verdict, "full_chain", default={}) or {}
    if (fc.get("imag_leg_verified") is False
            and fc.get("imag_leg_verified_offline_acked") is not True
            and fc.get("imag_leg_verified_gates_overall_pass") is True):
        out.append((
            "IMAG vetva nebola overená (nevznikol imag partial — beh nie je úplný): ZLYHALA",
            _OWN_CLAUDE,
        ))
    if (_g(verdict, "full_chain", "loss", "imag", "imag_presence_pass") is False
            and _g(verdict, "full_chain", "loss", "imag", "gates_overall_pass") is True):
        out.append((
            "IMAG vetva — prezenčná kontrola (dĺžka záznamu / optická čitateľnosť): ZLYHALA",
            _OWN_CLAUDE,
        ))

    # 12) Own digital burn absent for a SCHEDULED cam (issue 1247). REPORT-ONLY today
    #     (own_burn_absent::gates_overall_pass=false → _report_only_tripped); becomes BLOCKING only
    #     when the seam is flipped LIVE (gates_overall_pass=true). The `is True` guard makes this
    #     auto-follow the flip without double-counting with the report-only branch below.
    oba = _g(verdict, "full_chain", "own_burn_absent_gate", default=None)
    if (isinstance(oba, dict) and oba.get("pass") is False
            and oba.get("gates_overall_pass") is True):
        cams = _upper_join(oba.get("absent_cams") or [])
        out.append((
            f"Vlastný digitálny burn kamery CHÝBA (kamera bežala, ale bez vlastného burnu): "
            f"ZLYHALA — {cams}",
            _OWN_CLAUDE,
        ))

    # 13) Projection-tap scanout TEAR (issue 1196). LIVE since the known-torn calibration run
    #     (gates_overall_pass=true on the all_cambox_continuity.tear block). An Observed window
    #     whose tear_fraction exceeds the calibrated ceiling fails tear_gate_pass -> the imag
    #     projection is tearing on the CAM2 leg (present-vsync broke). The `gates_overall_pass is
    #     True` guard makes this auto-follow a future disarm (routes to nothing instead of
    #     double-counting), mirroring the delivery-spread / own_burn_absent flips.
    tear = _g(verdict, "all_cambox_continuity", "tear", default=None)
    if isinstance(tear, dict) and tear.get("gates_overall_pass") is True:
        torn = [
            w for w in (tear.get("windows") or [])
            if isinstance(w, dict) and w.get("tear_gate_pass") is False
        ]
        if torn:
            boxes = _upper_join(sorted({w.get("cambox") for w in torn if w.get("cambox")}))
            out.append((
                f"Projekčný sken sa TRHÁ (scanout tear na projekčnej vetve): ZLYHAL — {boxes}"
                if boxes else "Projekčný sken sa TRHÁ (scanout tear): ZLYHAL",
                _OWN_CLAUDE,
            ))

    # 14) Duplication-masked 50->60 source cadence (issue 1088, signal fixed + promoted LIVE by
    #     issue 1166). A per-cambox window whose content near-duplicate rate is sustained +
    #     regular + window-spanning (a 50->60 pulldown masked as a clean 60fps NDI stream — the
    #     issue-794 received= rate tap is structurally blind to it). The `gates_overall_pass is
    #     True` guard mirrors the delivery-spread / own_burn_absent / tear pattern: an OLD verdict
    #     (pre-flip, gates_overall_pass=false on its own node) stays report-only below; only a
    #     post-flip verdict routes here.
    dmc = _g(verdict, "all_cambox_continuity", "duplication_masked_cadence", default=None)
    if (isinstance(dmc, dict) and (dmc.get("masked_windows") or 0) > 0
            and dmc.get("gates_overall_pass") is True):
        out.append((
            "Duplikačne maskovaná kadencia zdroja (50->60 pulldown skrytý duplikátmi snímok): "
            "ZLYHALA",
            _OWN_CLAUDE,
        ))

    # 15) frozen_leg / self_heal_reset — RESTORED to blocking by issue 905 item 2 (were report-only
    #     under issue 914 pending cam1's ShadowCast grabber, issue 909). Each node ships its own
    #     `gates_overall_pass`. Mirror the recording-verdict.rs SelfHealAttributionReport gate
    #     EXACTLY: `frozen` (hard-frozen windows) gates overall_pass, but `stale_replay` does NOT
    #     (`any_frozen()` reads only `frozen`); self-heal gates on `attributed` OR
    #     `unattributed_events` (`any_self_heal()` reads both), never `attributed` alone. The
    #     `is True` guard auto-follows a future re-decouple without double-counting the report-only
    #     branch (which stays for stale_replay + a pre-flip frozen/self-heal).
    fl = _g(verdict, "frozen_leg", default={}) or {}
    if fl.get("frozen") and fl.get("gates_overall_pass") is True:
        out.append((
            "Zamrznutá kamera (vetva zamrzla, žiadny self-heal reset): ZLYHALA",
            _OWN_CLAUDE,
        ))
    sh = _g(verdict, "self_heal_reset", default={}) or {}
    if ((sh.get("attributed") or sh.get("unattributed_events"))
            and sh.get("gates_overall_pass") is True):
        out.append((
            "Self-heal reset počas merania (integrita behu, nie chyba kamery): ZLYHAL",
            _OWN_CLAUDE,
        ))

    return out


def _report_only_tripped(verdict):
    """Short Slovak names of REPORT-ONLY metrics that 'tripped' in this verdict — for the single
    optional 'ℹ️ sledované (neovplyvňuje verdikt)' line on a FAIL. These NEVER gate overall_pass
    (each seam ships gates_overall_pass=false) and are NEVER rendered as a ❌ failure — #1127 pt.4.
    Exactly the report-only seams the issue names."""
    names = []
    # #1142 — the imag leg is now SPLIT: PRESENCE/VERIFICATION blocks (→ _blocking_failures),
    # PER-FRAME CONTENT (burn contiguity + optical beat + per-segment continuity) stays report-only.
    # A #1142-shape verdict carries `imag_content_pass`; a pre-#1142 verdict does not, and its whole
    # imag leg was report-only (fall back to zero_loss then).
    imag_content = _g(verdict, "full_chain", "loss", "imag", "imag_content_pass")
    imag_seg_content = _g(verdict, "all_cambox_continuity", "imag", "overall_pass")
    if imag_content is not None:
        if imag_content is False or imag_seg_content is False:
            names.append("IMAG vetva (obsah/plynulosť)")
    elif imag_seg_content is False or _g(verdict, "full_chain", "loss", "imag", "zero_loss") is False:
        names.append("IMAG vetva")
    # #1142 — the delivery-side spread is now BLOCKING (its seam ships gates_overall_pass=true), so
    # it moves to _blocking_failures. It stays report-only ONLY on a pre-#1142 verdict that carries
    # no `gates_overall_pass` on the delivery block (the field auto-follows the seam flip).
    if (_g(verdict, "all_cambox_delivery_latency", "spread_gate_pass") is False
            and _g(verdict, "all_cambox_delivery_latency", "gates_overall_pass") is not True):
        names.append("rozptyl doručenia (strih)")
    if _g(verdict, "all_cambox_continuity", "cold_cut_onset", "any_genuine_cold_cut_miss") is True:
        names.append("cold-cut")
    # issue 905 item 2 — frozen_leg/self_heal_reset RESTORED to blocking (→ _blocking_failures item
    # 15). `frozen` (hard-frozen) and self-heal (attributed OR unattributed_events) now gate when
    # the node ships gates_overall_pass=true, so they stay report-only ONLY on a pre-flip verdict
    # (guarded `is not True`, the delivery-spread pattern). stale_replay NEVER gates overall_pass
    # (it is not in any_frozen()), so it stays report-only regardless of the flip.
    fl = _g(verdict, "frozen_leg", default={}) or {}
    # Split the two sub-signals (issue 905 item 2): `frozen` (hard-frozen) is report-only ONLY on a
    # pre-flip verdict -- post-flip it is a BLOCKING failure (item 15) and must NOT also read as a
    # report-only "zamrznutá" line directly under its own FAIL bullet. `stale_replay` never gates,
    # so it stays report-only with its own distinct label regardless of the flag.
    if fl.get("frozen") and fl.get("gates_overall_pass") is not True:
        names.append("zamrznutá vetva")
    if fl.get("stale_replay"):
        names.append("stale vetva (replay)")
    sh = _g(verdict, "self_heal_reset", default={}) or {}
    if ((sh.get("attributed") or sh.get("unattributed_events"))
            and sh.get("gates_overall_pass") is not True):
        names.append("self-heal reset")
    # issue 1166 — LIVE since the promote (its seam ships gates_overall_pass=true), so it moves to
    # _blocking_failures (item 14). The `is not True` guard mirrors the delivery-spread /
    # own_burn_absent pattern: only a PRE-flip verdict (no gates_overall_pass=true on its own node)
    # stays report-only here.
    if ((_g(verdict, "all_cambox_continuity", "duplication_masked_cadence", "masked_windows",
            default=0) or 0) > 0
            and _g(verdict, "all_cambox_continuity", "duplication_masked_cadence",
                   "gates_overall_pass") is not True):
        names.append("duplikačná kadencia")
    # issue 905 item 3 — the optical undecodable floor is LIVE (its seam ships
    # undecodable_floor_gates_overall_pass=true), so an over-floor run moves to _blocking_failures
    # (block 4). The `is not True` guard mirrors the dup_cadence / own_burn_absent pattern: only a
    # PRE-flip verdict (floor still report-only) stays report-only here, never double-counted.
    if (_g(verdict, "all_cambox_continuity", "run_wide_undecodable_within_floor") is False
            and _g(verdict, "all_cambox_continuity",
                   "undecodable_floor_gates_overall_pass") is not True):
        names.append("optická čitateľnosť (floor)")
    # lipsync cross-check (issue 1032) — report-only; JSON node lives under all_cambox_av_sync or
    # top-level depending on run shape (absent on ~all runs today). Guarded so absent -> no-op.
    lip = (_g(verdict, "all_cambox_av_sync", "lipsync_cross_check", default=None)
           or _g(verdict, "lipsync_cross_check", default=None))
    if isinstance(lip, dict) and lip.get("gate_pass") is False:
        names.append("lipsync")
    # issue 1247 — own digital burn absent for a scheduled cam. REPORT-ONLY today (its seam ships
    # gates_overall_pass=false). The `is not True` guard mirrors the delivery-spread pattern: if the
    # seam is ever flipped LIVE it routes to _blocking_failures (item 12) instead of double-counting.
    oba = _g(verdict, "full_chain", "own_burn_absent_gate", default=None)
    if (isinstance(oba, dict) and oba.get("pass") is False
            and oba.get("gates_overall_pass") is not True):
        cams = _upper_join(oba.get("absent_cams") or [])
        names.append(f"chýbajúci vlastný burn kamery ({cams})" if cams
                     else "chýbajúci vlastný burn kamery")
    # issue 1196 review-hardening — REPORT-ONLY: the projection-tap tear gate's aux signal is not
    # operable (aux decoding collapsed), so the LIVE gate cannot fire and is silently blind. Surface
    # it so the blind spot is visible; it never gates (a genuinely aux-free run is not a failure).
    tear = _g(verdict, "all_cambox_continuity", "tear", default=None)
    if isinstance(tear, dict) and tear.get("signal_operable") is False:
        names.append("tear-gate slepá škvrna (aux nedekóduje)")
    return names


def _verdict_line(verdict, meta):
    passed = bool(verdict.get("overall_pass"))
    head = "✅ E2E TEST PREŠIEL" if passed else "❌ E2E TEST ZLYHAL"
    run_id = meta.get("run_id") or "?"
    tail = f" — beh {run_id}"
    dur = _fmt_duration(meta.get("duration_secs"))
    if dur:
        tail += f" · {dur}"
    return head + tail


def _link_line(meta):
    url = meta.get("run_url")
    if url:
        return f"🔗 Plný detail: {url}"
    return f"🔗 Plný detail: verdikt JSON v artefaktoch CI behu {meta.get('run_id') or '?'}"


def compose_summary(verdict: dict, meta: dict | None = None) -> str:
    """#1127 — PURE: verdict JSON dict + small meta dict -> the short, phone-readable Slovak
    summary the Discord path (`--json-chunks`) sends.

    PASS -> 3 lines (verdict, zero-loss summary, link). FAIL -> verdict + only the failing BLOCKING
    gates (with #1117 ownership) + at most one collapsed report-only ℹ️ line + link. A report-only
    seam is NEVER rendered as a ❌. A FAIL with no recognized blocking gate still emits a generic
    blocking line pointing at the CI log — a FAIL is never silently hidden."""
    meta = meta or {}
    lines = [_verdict_line(verdict, meta)]
    if bool(verdict.get("overall_pass")):
        cams = _cameras_present(verdict)
        n = len(cams)
        drops = _stream_drop_total(verdict, cams)
        # On a PASS `drops` is normally 0; it can be a small nonzero under the #904 real-drops
        # allowance (still within tolerance, so the run PASSED) — say so rather than pair ✅ with a
        # bare loss count.
        loss_txt = "0 stratených snímok" if drops == 0 else f"{drops} stratených snímok (v rámci tolerancie)"
        lines.append(f"📷 {n} {_camera_plural(n)} · {loss_txt} (celá cesta kamera → stream)")
        lines.append(_link_line(meta))
        return "\n".join(lines)

    failures = _blocking_failures(verdict)
    if not failures:
        # Safety net: overall_pass is false but no enumerated blocking gate matched (e.g. a
        # burn_hold-only fold, or a shape this summary doesn't itemize). Never look PASS-ish.
        failures = [(
            "Test zlyhal — konkrétnu blokujúcu bránu sa nepodarilo rozpoznať, pozri CI log",
            _OWN_CLAUDE,
        )]
    for label, owner in failures:
        lines.append(f"• {label} — {owner}")
    tripped = _report_only_tripped(verdict)
    if tripped:
        lines.append(f"ℹ️ sledované (neovplyvňuje verdikt): {', '.join(tripped)}")
    lines.append(_link_line(meta))
    return "\n".join(lines)


def _ci_run_url():
    """The GitHub Actions run URL from standard env vars, or None outside CI. Read in the impure
    CLI wrapper only — `compose_summary` stays pure (it renders `meta['run_url']`)."""
    server = os.environ.get("GITHUB_SERVER_URL")
    repo = os.environ.get("GITHUB_REPOSITORY")
    run = os.environ.get("GITHUB_RUN_ID")
    if server and repo and run:
        return f"{server}/{repo}/actions/runs/{run}"
    return None


def compose_report(verdict: dict, meta: dict | None = None) -> str:
    """Pure: verdict JSON dict + small meta dict -> Slovak markdown report text.

    `meta` keys (all optional): run_id, event ("CI PR gate" | "manuálny beh (recording-e2e.sh)" |
    any free label), duration_secs, gate_exit (the merge recording-verdict process exit code),
    pins (the #756 Member 3 live-pins snapshot dict -- see _section_latency_pins's own docstring
    for its shape; entirely optional, the section is skipped when absent).
    """
    meta = meta or {}
    sections = [
        _section_header(verdict, meta),
        _section_zero_loss(verdict),
        _section_latency(verdict),
        _section_video_sync(verdict),
        _section_av_sync(verdict),
        _section_overall(verdict, meta),
        _section_presentation_cadence(verdict),
    ]
    pins_section = _section_latency_pins(verdict, meta)
    if pins_section is not None:
        sections.append(pins_section)
    mv_skew_section = _section_mv_skew(verdict, meta)
    if mv_skew_section is not None:
        sections.append(mv_skew_section)
    residual_section = _section_residual_events(verdict)
    if residual_section is not None:
        sections.append(residual_section)
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
        "--pins-json",
        default=None,
        help="#756 Member 3: path to the JSON scripts/latency_pins_snapshot.py wrote (live "
        "genlock latency pins + recommended pins) -- optional; the pins report section is "
        "skipped entirely when not supplied",
    )
    ap.add_argument(
        "--mv-skew-json",
        default=None,
        help="#761: path to the JSON scripts/mv_skew_snapshot.py wrote (per-camera MV-clone-vs-main "
        "presentation skew) -- optional; the MV-skew report section is skipped entirely when not "
        "supplied",
    )
    ap.add_argument(
        "--run-url",
        default=None,
        help="#1127: the CI run / artifact URL for the summary's link line (Discord path). When "
        "omitted it is derived from GITHUB_SERVER_URL/GITHUB_REPOSITORY/GITHUB_RUN_ID; outside CI "
        "the link line falls back to naming the verdict-JSON artifact.",
    )
    ap.add_argument(
        "--json-chunks",
        action="store_true",
        help="print a JSON array of Discord-sized message chunks of the SHORT #1127 summary "
        "(verdict-first, PASS=3 lines, FAIL=only failing blocking gates). Without this flag the "
        "FULL detailed report is printed instead (the plain-text / CI-log rendering).",
    )
    args = ap.parse_args(argv)

    with open(args.json, encoding="utf-8") as f:
        verdict = json.load(f)

    pins = None
    if args.pins_json:
        with open(args.pins_json, encoding="utf-8") as f:
            pins = json.load(f)

    mv_skew = None
    if args.mv_skew_json:
        with open(args.mv_skew_json, encoding="utf-8") as f:
            mv_skew = json.load(f)

    meta = {
        "run_id": args.run_id,
        "event": args.event,
        "duration_secs": args.duration_secs,
        "gate_exit": args.gate_exit,
        "pins": pins,
        "mv_skew": mv_skew,
        "run_url": args.run_url or _ci_run_url(),
    }
    if args.json_chunks:
        # #1127: Discord carries only the short, verdict-first summary.
        print(json.dumps(chunk_for_discord(compose_summary(verdict, meta))))
    else:
        # Plain mode = the FULL detail (the CI-log / manual-inspection rendering). The Discord
        # body no longer carries this wall (#1127); the detail lives here + in the verdict JSON.
        print(compose_report(verdict, meta))
    return 0


if __name__ == "__main__":
    sys.exit(main())
