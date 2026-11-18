"""issue 1534 (owner ROZHODNUTE 18.11.2026) -- how the E2E report renders the BLOCKING whole-session video
gate (the `genlock_session_continuity` block scripts/lib/genlock-session-continuity.sh merges into the
verdict JSON, decided by scripts/genlock_session_continuity.py).

Used by scripts/e2e_discord_report.py (kept apart: that file is past its size budget). Pure.

  * `blocks(verdict)`         -- the block gates (gates_overall_pass) and is not PASS: the run is RED
                                 even when the merge recording-verdict passed (FAIL and UNKNOWN alike);
  * `summary_label(verdict)`  -- the ONE Slovak summary line of a red block, the failing inputs named;
  * `section(verdict)`        -- the full-report section, one line per input (None without a block).
"""

_KEY = "genlock_session_continuity"
_SHOWN = 6


def _block(verdict):
    b = (verdict or {}).get(_KEY)
    return b if isinstance(b, dict) else None


def blocks(verdict):
    b = _block(verdict)
    return bool(b) and b.get("gates_overall_pass") is True and b.get("pass") is not True


def _sk_failures(b):
    """The failing inputs in Slovak, from the block's structured fields (never its English lines)."""
    out = []
    for box, bx in (b.get("boxes") or {}).items():
        for i in bx.get("inputs") or []:
            if i.get("verdict") != "FAIL" or i.get("excluded"):
                continue
            parts = [f"{k} +{v['delta']}" for k, v in (i.get("counters") or {}).items() if v.get("delta")]
            j = i.get("jxs") or {}
            if j.get("lossy_windows"):
                sums = ", ".join(f"{k} +{v}" for k, v in (j.get("sums") or {}).items() if v)
                parts.append(f"{j['lossy_windows']} stratových JPEG XS okien ({sums or 'súčty neuvedené'}; "
                             f"zamaskované = STRATENÉ)")
            out.append(f"{box} '{i.get('input')}': {', '.join(parts)} po ustálení")
    return out


def summary_label(verdict):
    if not blocks(verdict):
        return None
    b = _block(verdict)
    if b.get("verdict") == "FAIL":
        fails = _sk_failures(b) or list(b.get("failures") or [])
        more = f" (+{len(fails) - _SHOWN} ďalších)" if len(fails) > _SHOWN else ""
        return f"Strata obrazu od štartu OBS (celá session, nie len okno): ZLYHALA — {'; '.join(fails[:_SHOWN])}{more}"
    reasons = b.get("unknown_reasons") or ["dôvod neuvedený"]
    more = f" (+{len(reasons) - 3} ďalších)" if len(reasons) > 3 else ""
    return f"Obraz od štartu OBS (celá session): NEOVERENÉ = ZLYHANIE — {'; '.join(reasons[:3])}{more}"


def _glyph(v):
    return "✅" if v == "PASS" else ("❌" if v == "FAIL" else "❌ neoverené")


def _input_line(box, i):
    parts = [f"{k} {v['settle']}→{v['closing']}" for k, v in (i.get("counters") or {}).items()]
    j = i.get("jxs")
    if j:
        parts.append(f"JPEG XS stratové okná {j.get('lossy_windows', 0)} z {j.get('windows', 0)}")
    d = (i.get("context") or {}).get("dropped_due")
    if d:
        parts.append(f"informatívne: dropped_due +{d}")
    if i.get("first_growth"):
        parts.append(f"prvý nárast {i['first_growth']}")
    if i.get("excluded"):
        parts.append("kamera kvitovaná offline: len informatívne")
    return f"  • {box} '{i.get('input')}': {', '.join(parts) or 'bez počítadiel'} — {_glyph(i.get('verdict'))}"


def section(verdict):
    b = _block(verdict)
    if not b:
        return None
    v = b.get("verdict")
    glyph = "✅ PASS" if v == "PASS" else ("❌ FAIL" if v == "FAIL" else "❌ NEOVERENÁ (= FAIL)")
    lines = [f"**🎞 Obraz od štartu OBS (celá session, ustálenie {b.get('settle_s', 60):g} s): {glyph}**"]
    for box, bx in (b.get("boxes") or {}).items():
        lines.append(f"{box} ({bx.get('log', '?')}): {_glyph(bx.get('verdict'))}")
        lines.extend(_input_line(box, i) for i in bx.get("inputs") or [])
    fails = _sk_failures(b)
    if fails:
        lines.append("Zlyhalo: " + "; ".join(fails))
    for u in b.get("unknown_reasons") or []:
        lines.append(f"Neoverené: {u}")
    return "\n".join(lines)
