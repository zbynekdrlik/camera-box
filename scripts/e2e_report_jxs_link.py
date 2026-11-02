"""issue 1387 slice 2 (design 6558387590) -- how the E2E report renders the BLOCKING JPEG XS link
continuity gate (the `jxs_link_continuity` block scripts/lib/jxs-link-continuity.sh merges into the
verdict JSON, decided by scripts/jxs_link_continuity.py).

Used by scripts/e2e_discord_report.py (kept apart: that file is past its size budget). Pure.

  * `blocks(verdict)`         -- the block gates (gates_overall_pass) and is not PASS: the run is RED
                                 even when the merge recording-verdict passed (FAIL and UNKNOWN alike);
  * `summary_label(verdict)`  -- the ONE Slovak summary line of a red block, the failing links named,
                                 a concealed slice named LOST ("zamaskované = STRATENÉ");
  * `section(verdict)`        -- the full-report section, one line per link (None without a block).
"""

_KEY = "jxs_link_continuity"
_INPUT_KEYS = ("lost_slices", "concealed", "undecodable", "dropped_unread", "foreign", "reopens")
_OUTPUT_KEYS = ("dropped_busy", "not_taken", "refused", "send_errors", "encode_errors")
_REPORT_KEYS = ("fec_repaired", "decoder_reopens", "stamp_dup")


def _block(verdict):
    b = (verdict or {}).get(_KEY)
    return b if isinstance(b, dict) else None


def blocks(verdict):
    b = _block(verdict)
    return bool(b) and b.get("gates_overall_pass") is True and b.get("pass") is not True


def _who(link):
    kind = "výstup" if link.get("kind") == "output" else "vstup"
    return f"{link.get('box')} {kind} '{link.get('link')}'"


def _failure_text(link):
    parts = []
    for f in link.get("failures") or []:
        parts.append(f.replace("(a concealed slice is a LOST slice)", "(zamaskované = STRATENÉ)"))
    return ", ".join(parts)


def summary_label(verdict):
    if not blocks(verdict):
        return None
    b = _block(verdict)
    links = b.get("links") or []
    if b.get("verdict") == "FAIL":
        bad = [f"{_who(l)}: {_failure_text(l)}" for l in links if l.get("verdict") == "FAIL"]
        return f"Strata na JPEG XS linke (okno nahrávky): ZLYHALA — {'; '.join(bad)}"
    reasons = b.get("unknown_reasons") or ["dôvod neuvedený"]
    return f"JPEG XS linky (okno nahrávky): NEOVERENÉ = ZLYHANIE — {reasons[0]}"


def _glyph(v):
    return "✅" if v == "PASS" else ("❌" if v == "FAIL" else "❌ neoverená")


def _link_line(link):
    keys = _OUTPUT_KEYS if link.get("kind") == "output" else _INPUT_KEYS
    c = link.get("counters") or {}
    r = link.get("report") or {}
    iv = link.get("interval") or {}
    text = (f"{_who(link)}: {link.get('windows', 0)} okien ({iv.get('from')} .. {iv.get('to')}), "
            f"{link.get('frames', 0)} snímok")
    text += "".join(f", {k} {c[k]}" for k in keys if k in c)
    extra = [f"{k} {r[k]}" for k in _REPORT_KEYS if r.get(k)]
    if extra:
        text += f" · informatívne: {', '.join(extra)}"
    return f"  • {text} — {_glyph(link.get('verdict'))}"


def section(verdict):
    b = _block(verdict)
    if not b:
        return None
    v = b.get("verdict")
    glyph = "✅ PASS" if v == "PASS" else ("❌ FAIL" if v == "FAIL" else "❌ NEOVERENÁ (= FAIL)")
    lines = [f"**📡 Kontinuita JPEG XS liniek (okno nahrávky): {glyph}**"]
    lines.extend(_link_line(l) for l in b.get("links") or [])
    if b.get("failures"):
        lines.append("Zlyhalo: " + "; ".join(f.replace("(a concealed slice is a LOST slice)",
                                                       "(zamaskované = STRATENÉ)") for f in b["failures"]))
    for u in b.get("unknown_reasons") or []:
        lines.append(f"Neoverené: {u}")
    return "\n".join(lines)
