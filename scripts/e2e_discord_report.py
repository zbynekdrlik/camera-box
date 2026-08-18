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
        "--json-chunks",
        action="store_true",
        help="print a JSON array of Discord-sized message chunks instead of the raw text",
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
    }
    text = compose_report(verdict, meta)
    if args.json_chunks:
        print(json.dumps(chunk_for_discord(text)))
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
