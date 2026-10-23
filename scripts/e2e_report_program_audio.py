"""issue 1439 (ROZHODNUTE 6359382787) -- how the E2E report renders the BLOCKING program-audio
continuity gate (the `program_audio_continuity` block scripts/lib/program-audio-continuity.sh merges
into the verdict JSON, decided by scripts/program_audio_continuity.py).

Used by scripts/e2e_discord_report.py (kept apart: that file is past its size budget). Pure.

  * `blocks(verdict)`         -- the block gates (gates_overall_pass) and is not PASS: the run is RED
                                 even when the merge recording-verdict passed (FAIL and UNKNOWN alike);
  * `summary_label(verdict)`  -- the ONE Slovak summary line of a red block, concealed slots named
                                 LOST ("zamaskované = STRATENÉ"), UNKNOWN named "NEOVERENÁ";
  * `section(verdict)`        -- the full-report section (None when the verdict has no block).
"""

_KEY = "program_audio_continuity"
_EVIDENCE_MAX = 10


def _block(verdict):
    b = (verdict or {}).get(_KEY)
    return b if isinstance(b, dict) else None


def blocks(verdict):
    b = _block(verdict)
    return bool(b) and b.get("gates_overall_pass") is True and b.get("pass") is not True


def _lost_text(b):
    lost = b.get("lost_slots")
    if not lost:
        return None
    conc, sil = b.get("lost_concealed"), b.get("lost_silence")
    if conc is None:
        return f"{lost} stratených slotov"
    return f"{lost} stratených slotov ({conc} zamaskované = STRATENÉ, {sil} ticho)"


def summary_label(verdict):
    if not blocks(verdict):
        return None
    b = _block(verdict)
    name = b.get("input") or "?"
    if b.get("verdict") != "FAIL":
        reasons = b.get("unknown_reasons") or ["dôvod neuvedený"]
        return f"Kontinuita zvuku programu ('{name}'): NEOVERENÁ = ZLYHANIE — {reasons[0]}"
    parts = []
    lost = _lost_text(b)
    if lost:
        parts.append(lost)
    parts.extend(f for f in (b.get("failures") or []) if not f.startswith("lost +"))
    return f"Strata zvuku programu ('{name}', okno nahrávky): ZLYHALA — {', '.join(parts)}"


def section(verdict):
    b = _block(verdict)
    if not b:
        return None
    name = b.get("input") or "?"
    v = b.get("verdict")
    glyph = "✅ PASS" if v == "PASS" else ("❌ FAIL" if v == "FAIL" else "❌ NEOVERENÁ (= FAIL)")
    lines = [f"**🔊 Kontinuita zvuku programu ('{name}', okno nahrávky): {glyph}**"]
    iv = b.get("interval")
    if iv:
        lines.append(f"Stavové riadky obs-vban: {iv.get('from')} .. {iv.get('to')}")
    c = b.get("counters") or {}
    if c:
        lines.append("obs-vban prírastky: " + ", ".join(
            f"{k} +{x['delta']}" for k, x in c.items() if k not in ("received", "reordered")))
    lost = _lost_text(b)
    if lost:
        lines.append(f"Stratené: {lost} — zamaskovaný slot je stratený slot, nikdy OK")
    a = b.get("asrc") or {}
    if a:
        lines.append("ASRC prírastky: " + ", ".join(f"{k} +{x['delta']}" for k, x in a.items()))
    for key in ("loss_runs", "late_lines", "reanchor_lines", "guard_lines"):
        items = b.get(key) or []
        for item in items[:_EVIDENCE_MAX]:
            lines.append(f"  • {item}")
        if len(items) > _EVIDENCE_MAX:
            lines.append(f"  • ... ďalších {len(items) - _EVIDENCE_MAX}")
    t = b.get("telemetry") or {}
    if t:
        lines.append("MIXED_LATE prírastok (tento vstup): " + ", ".join(f"{k} +{x['delta']}" for k, x in t.items()))
    for k, x in (b.get("checks") or {}).items():
        if x != "ran":
            lines.append(f"{k}: {x}")
    if b.get("not_graded"):
        lines.append(f"neoverené (starší build): {', '.join(b['not_graded'])}")
    if b.get("failures"):
        lines.append("Zlyhalo: " + ", ".join(b["failures"]))
    for u in b.get("unknown_reasons") or []:
        lines.append(f"Neoverené: {u}")
    if b.get("mix_mixed_late_delta"):
        lines.append(f"(informatívne: MIXED_LATE celého mixu +{b['mix_mixed_late_delta']})")
    return "\n".join(lines)
