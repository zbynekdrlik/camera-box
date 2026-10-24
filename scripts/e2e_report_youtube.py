#!/usr/bin/env python3
"""Issue 1404 -- how the E2E report renders the YouTube end point and the per-point table.

The full-path E2E folds a `youtube_leg` block into the verdict JSON (scripts/youtube_leg_e2e.py,
gates_overall_pass=true, so FAIL and UNKNOWN both block). Owner 24.10.2026: the report must say
where the measurement ENDED; until YouTube passes in a run it never claims "full path" or a
faultless rig. Used by scripts/e2e_discord_report.py (kept apart: that file is past its size budget).

  * `blocks(verdict)`         -- the block gates and is not PASS: the run is RED
  * `summary_label(verdict)`  -- the ONE Slovak summary line of a red block
  * `endpoint_text(verdict)`  -- the end-point words of the PASS camera line
  * `section(verdict)`        -- the full-report section (None without a block)
  * `points_markdown(verdict, run_url)` -- the per-point table for the GitHub job summary and the PR
    comment: cam -> strih -> stream recording -> YouTube, video / audio / A/V each

CLI: e2e_report_youtube.py --verdict <verdict.json> [--run-url URL] --out <points.md>
(a missing or unreadable verdict JSON still writes a page that says so; exit 0 either way).
"""
import argparse
import json
import sys

_KEY = "youtube_leg"
_WINDOWS_MAX = 20


def _block(verdict):
    b = (verdict or {}).get(_KEY)
    return b if isinstance(b, dict) else None


def blocks(verdict):
    b = _block(verdict)
    return bool(b) and b.get("gates_overall_pass") is True and b.get("pass") is not True


def passed(verdict):
    b = _block(verdict)
    return bool(b) and b.get("pass") is True and b.get("verdict") == "PASS"


def _first_reason(b):
    reasons = b.get("reasons") or []
    return reasons[0] if reasons else "dôvod neuvedený"


def summary_label(verdict):
    if not blocks(verdict):
        return None
    b = _block(verdict)
    if b.get("verdict") == "FAIL":
        return f"YouTube (koncový bod, VOD e2e rtmp): ZLYHAL — {_first_reason(b)}"
    return f"YouTube (koncový bod): NEOVERENÝ = ZLYHANIE — {_first_reason(b)}"


def endpoint_text(verdict):
    if passed(verdict):
        return "celá cesta kamera → YouTube"
    if _block(verdict):
        return "kamera → stream nahrávka; YouTube NEPREŠIEL"
    return "kamera → stream nahrávka; YouTube sa v tomto behu nemeral"


def _glyph(ok):
    return "✅" if ok is True else ("❌" if ok is False else "❔")


def section(verdict):
    b = _block(verdict)
    if not b:
        return None
    v = b.get("verdict")
    head = {"PASS": "✅ PASS", "FAIL": "❌ FAIL"}.get(v, "❌ NEOVERENÝ (= FAIL)")
    lines = [f"**📺 YouTube (koncový bod, VOD {b.get('vod_id') or '?'} z e2e rtmp): {head}**"]
    lines.append(f"Okná: {b.get('windows_measured', 0)} merané"
                 + (f", vynechané (krátke): {', '.join(b['windows_dropped'])}" if b.get("windows_dropped") else ""))
    for w in (b.get("windows") or [])[:_WINDOWS_MAX]:
        lines.append(f"  • {w.get('name')}: dup {w.get('dup')} / skip {w.get('skip')}, zvuk skoky "
                     f"{w.get('audio_lag_jumps')} nízka korelácia {w.get('audio_low_corr')} ticho "
                     f"{w.get('audio_silent')}, A/V VOD−nahrávka {_ms(w.get('av_delta_ms'))}"
                     + (f" — {'; '.join(w['errors'])}" if w.get("errors") else ""))
    for r in b.get("reasons") or []:
        lines.append(f"Dôvod: {r}")
    return "\n".join(lines)


def _ms(x):
    return "?" if not isinstance(x, (int, float)) else f"{x:+.1f} ms"


def _g(d, *path):
    for k in path:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


# ------------------------------------------------------------------ the per-point table

def _camera_rows(verdict):
    """[(name, ok, text)] for each camera window group of the stream recording."""
    segs = _g(verdict, "all_cambox_continuity", "segments") or []
    per = {}
    for s in segs:
        if not isinstance(s, dict):
            continue
        cam = str(s.get("cambox") or "?")
        p = per.setdefault(cam, {"windows": 0, "ok": True, "copies": 0, "gaps": 0})
        p["windows"] += 1
        p["copies"] += s.get("copies") or 0
        p["gaps"] += s.get("gaps") or 0
        if s.get("pass") is not True and not s.get("multi_source"):
            p["ok"] = False
    return [(cam, p["ok"], f"{p['windows']} okien, kópie {p['copies']}, medzery {p['gaps']}")
            for cam, p in sorted(per.items())]


def _av_text(verdict):
    av = verdict.get("all_cambox_av_sync") if isinstance(verdict.get("all_cambox_av_sync"), dict) else {}
    cams = []
    for k in sorted(av):
        n = av[k]
        if isinstance(n, dict) and n.get("verdict") == "measured":
            cams.append(f"{k} {_ms(n.get('av_offset_ms'))} (mad {_ms(n.get('mad_ms'))})")
    return av.get("gate_pass"), ", ".join(cams) or "nemerané"


def _node_loss(verdict, node):
    n = _g(verdict, "full_chain", "loss", node)
    if not isinstance(n, dict):
        return None, "nemerané"
    return n.get("zero_loss"), f"{n.get('present_count')}/{n.get('expected_count')} snímok, straty {n.get('real_drops')}"


def _yt_cells(b):
    if not b:
        return [(None, "nemeraný v tomto behu")] * 3
    ws = b.get("windows") or []
    if not ws:
        return [(False, _first_reason(b))] * 3
    dup = sum(w.get("dup") or 0 for w in ws)
    skip = sum(w.get("skip") or 0 for w in ws)
    errs = sum(1 for w in ws if w.get("errors"))
    st = b.get("criteria") if isinstance(b.get("criteria"), dict) else {}
    video_ok = _both(_crit(st.get("dupskip")), _crit(st.get("coverage")))
    audio_ok, av_ok = _crit(st.get("audio")), _crit(st.get("av"))
    deltas = [w.get("av_delta_ms") for w in ws if isinstance(w.get("av_delta_ms"), (int, float))]
    av_txt = f"VOD−nahrávka {min(deltas):+.1f}..{max(deltas):+.1f} ms" if deltas else "nemerané"
    return [(_crit(video_ok), f"dup {dup} / skip {skip} v {len(ws)} oknách" + (f", {errs} okien s chybou" if errs else "")),
            (_crit(audio_ok), "kontinuita bloku 0,25 s (skoky / korelácia / ticho)"),
            (_crit(av_ok), av_txt)]


def _crit(s):
    """The tool's per-criterion status: PASS = True; FAIL and UNKNOWN both = False (UNKNOWN blocks)."""
    if s in (True, "PASS"):
        return True
    if s in (False, "FAIL", "UNKNOWN"):
        return False
    return None


def _both(a, b):
    if a is False or b is False:
        return False
    return True if a is True and b is True else None


def points_markdown(verdict, run_url=None):
    """The per-point table (Markdown) for the job summary and the PR comment."""
    if not isinstance(verdict, dict):
        return ("## Full-path E2E — overenie po bodoch\n\n❌ Verdikt JSON neexistuje: beh skončil pred "
                "výsledkom (pozri CI log). **Nič nie je overené, ani stream nahrávka, ani YouTube.**\n")
    b = _block(verdict)
    yt_ok = passed(verdict)
    lines = ["## Full-path E2E — overenie po bodoch", ""]
    if yt_ok:
        lines.append("**Koncový bod: YouTube (VOD e2e rtmp) ✅ — overená celá cesta cam2 → … → YouTube.**")
    else:
        why = summary_label(verdict) if b else "YouTube sa v tomto behu nemeral"
        lines.append(f"**Koncový bod overenia: stream nahrávka. {why}. Toto NIE JE overenie celej cesty.**")
    lines += ["", "| Bod | Obraz (snímky, monotónne) | Zvuk (vzorky) | A/V |", "|---|---|---|---|"]
    for cam, ok, txt in _camera_rows(verdict):
        lines.append(f"| {cam} → strih → stream | {_glyph(ok)} {txt} | — (zvuk meria stream) | — |")
    s_ok, s_txt = _node_loss(verdict, "strih")
    lines.append(f"| strih program nahrávka | {_glyph(s_ok)} {s_txt} | — strih nemá merací zvuk | — strih nemá merací zvuk |")
    t_ok, t_txt = _node_loss(verdict, "stream")
    pa = verdict.get("program_audio_continuity") if isinstance(verdict.get("program_audio_continuity"), dict) else None
    pa_cell = (f"{_glyph(pa.get('pass') is True)} prijímač obs-vban {pa.get('verdict')} (počítadlá; vzorková kontrola "
               "nahrávky zatiaľ nie)") if pa else "❔ nemerané"
    av_ok, av_txt = _av_text(verdict)
    lines.append(f"| stream nahrávka | {_glyph(t_ok)} {t_txt} | {pa_cell} | {_glyph(av_ok)} {av_txt} |")
    (v_ok, v_txt), (a_ok, a_txt), (x_ok, x_txt) = _yt_cells(b)
    lines.append(f"| YouTube VOD {(b or {}).get('vod_id') or ''} | {_glyph(v_ok)} {v_txt} | {_glyph(a_ok)} {a_txt} "
                 f"| {_glyph(x_ok)} {x_txt} |")
    if b and b.get("reasons"):
        lines += ["", "YouTube: " + "; ".join(str(r) for r in b["reasons"][:5])]
    if run_url:
        lines += ["", f"Plný detail: {run_url}"]
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1404: the per-point E2E table (Markdown)")
    ap.add_argument("--verdict", required=True)
    ap.add_argument("--run-url")
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    try:
        with open(a.verdict) as f:
            verdict = json.load(f)
    except (OSError, ValueError):
        verdict = None
    with open(a.out, "w") as f:
        f.write(points_markdown(verdict, a.run_url))
    return 0


if __name__ == "__main__":
    sys.exit(main())
