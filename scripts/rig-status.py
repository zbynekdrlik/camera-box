#!/usr/bin/env python3
"""rig-status -- a dev1-hosted status PAGE over scripts/rig-health-audit.py output (#787).

The user's mandate (2026-07-16, after a full overnight rig power-off): rig health must be
provable at a glance -- "aby aj z logov pripadne z nejakej buducej status stranky sa dalo
jasne vidiet ze vsetky nody su uplne zdrave". scripts/rig-health-audit.py already produces the
one authoritative `[PASS/WARN/FAIL] node key=value... <<problems>>` sweep line per node (exit
0/1/2). This tool is the layer ABOVE it: a periodic run, a rolling JSONL history, a rendered
web page, and a deduped Discord alarm on FAIL.

ARCHITECTURE -- the page is a RENDERER, never a second prober. rig-health-audit.py is the ONE
data source: `update` shells out to it and parses its STDOUT; this tool never ssh/WS-es a rig
node itself (the same discipline the audit itself uses when it shells out to cadence-health.sh
instead of a second fps divisor). The parser is GENERIC -- it tokenises `key=value` / `key[...]`
facets with no per-facet knowledge, so a NEW feeder facet renders as a chip automatically:
the per-camera cadence tier (#1089) today, a build-sha / genlock-parity tier whenever the #789
bod-3 feeder enrichment lands -- with zero change to this page.

Subcommands:
  update   run rig-health-audit.py -> append a history row -> render index.html + status.json
           into the serve dir -> on FAIL fire a deduped Discord alert (throttle reuses the
           tested scripts/lib/obs-watchdog-decision.sh kernel).
  render   re-render the page from a captured audit text (a file or '-' for stdin); no ssh/WS.
           Handy for regenerating the page or for offline inspection.

DEPLOYMENT (dev1, SHIPS DISABLED -- sibling convention, e.g. bundle-state #732 / cam-disk-guard):
  systemd/rig-status-update.timer  -> rig-status-update.service (oneshot `rig-status.py update`)
  systemd/rig-status-page.service  -> `python3 -m http.server` over the serve dir
  Enable opt-in on dev1:
    systemctl --user enable --now rig-status-update.timer rig-status-page.service
  The page is then reachable on dev1's private interfaces (never localhost, never public):
    http://10.77.9.103:8790/    (rig LAN)      http://100.104.8.125:8790/    (tailscale)

Serve dir (--dir) defaults to ~/.camera-box/rig-status/ and holds ONLY the published index.html +
status.json (what http.server exposes). Private state (history.jsonl + alert.state) lives in a
separate --state-dir (~/.camera-box/rig-status-state/) so the web root never leaks internal files.
"""
from __future__ import annotations

import argparse
import html
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(HERE)
AUDIT = os.path.join(HERE, "rig-health-audit.py")
THROTTLE_LIB = os.path.join(HERE, "lib", "obs-watchdog-decision.sh")
DEFAULT_DIR = os.path.expanduser("~/.camera-box/rig-status")
NOTIFY = os.environ.get("AIRULESET_NOTIFY", os.path.expanduser("~/devel/airuleset/airuleset.py"))

DEFAULT_STATE_DIR = os.path.expanduser("~/.camera-box/rig-status-state")  # private: history + alert.state
HISTORY_MAX = 500                                                    # rolling JSONL cap on disk
HISTORY_SHOWN = 20                                                   # rows rendered on the page
ALERT_THROTTLE_PASSES = int(os.environ.get("RIG_STATUS_ALERT_THROTTLE_PASSES", "12"))  # ~1h @5min
# The audit sweeps 7 cams + imag + strih + stream sequentially; worst realistic (all reachable but
# slow) is ~225 s, so 300 s leaves margin. A hang past this is caught as a DEGRADED prober, never a
# crash (see _run_audit) -- and the systemd unit's TimeoutStartSec is sized above this.
AUDIT_TIMEOUT_S = int(os.environ.get("RIG_STATUS_AUDIT_TIMEOUT_S", "300"))

_NODE_RE = re.compile(r"^\[(PASS|WARN|FAIL)\]\s+(\S+)\s+(.*)$")
_PROBLEMS_RE = re.compile(r"\s*<<(.*)>>\s*$")
_FACET_BRACKET_RE = re.compile(r"^([A-Za-z_]\w*)\[(.*)\]$")          # arrivals[...] / cadence[...]
_FACET_KV_RE = re.compile(r"^([A-Za-z_]\w*)=(.*)$")                  # key=value

_BADGE_ORDER = {"FAIL": 0, "WARN": 1, "PASS": 2}


# --------------------------------------------------------------------------- pure: parse + derive
def parse_audit(text):
    """Turn rig-health-audit.py STDOUT into structured per-node records. Each record:
      {verdict, node, facets:[{key,value} | {flag}], problems:str|None, raw:str}
    The `<<...>>` problems block (which may contain internal spaces) is peeled off FIRST and kept
    verbatim, so it never mis-tokenises into facet chips. Bracket groups stay whole. Summary /
    blank / stderr lines that don't match the `[VERDICT] node detail` shape are ignored."""
    records = []
    for line in (text or "").splitlines():
        m = _NODE_RE.match(line)
        if not m:
            continue
        verdict, node, rest = m.group(1), m.group(2), m.group(3)
        problems = None
        pm = _PROBLEMS_RE.search(rest)
        if pm:
            problems = pm.group(1).strip()
            rest = rest[: pm.start()]
        facets = []
        for tok in rest.split():
            bm = _FACET_BRACKET_RE.match(tok)
            kv = _FACET_KV_RE.match(tok)
            if bm:
                facets.append({"key": bm.group(1), "value": bm.group(2)})
            elif kv:
                facets.append({"key": kv.group(1), "value": kv.group(2)})
            else:
                facets.append({"flag": tok})
        records.append({"verdict": verdict, "node": node, "facets": facets,
                        "problems": problems, "raw": line.rstrip()})
    return records


def summarize(records):
    """PASS/WARN/FAIL counts + the overall verdict (FAIL if any FAIL, else WARN if any WARN)."""
    counts = {"pass": 0, "warn": 0, "fail": 0}
    for r in records:
        counts[r["verdict"].lower()] += 1
    overall = "FAIL" if counts["fail"] else ("WARN" if counts["warn"] else "PASS")
    return {"pass": counts["pass"], "warn": counts["warn"], "fail": counts["fail"],
            "overall": overall}


def alert_signature(records):
    """The deduped Discord-on-FAIL fingerprint: the sorted set of FAIL node names, comma-joined.
    Empty string when nothing is FAIL (WARN never pages -- the audit's own severity model)."""
    return ",".join(sorted(r["node"] for r in records if r["verdict"] == "FAIL"))


def overall_state(records, exit_code=None):
    """The AUTHORITATIVE page verdict. A crashed / empty / timed-out audit (0 node lines, or an
    exit code outside the audit's own 0/1/2 contract) is ERROR -- never PASS. This is the guard
    against the false-green a bare summarize([]) would produce: the whole point of the page is that
    a green banner PROVES health, so "no data" must scream, not affirm all-healthy."""
    if not records:
        return "ERROR"
    if exit_code is not None and exit_code not in (0, 1, 2):
        return "ERROR"
    return summarize(records)["overall"]


def alert_condition(records, exit_code=None):
    """What is worth paging Discord about: a DOWN prober (no data / crash exit) OR any FAIL node.
    Returns a stable signature string (deduped by the throttle), or '' when everything is healthy
    (WARN never pages). A down prober pages too -- a silent status page over a dead audit is the
    exact 'tiché unknown' the rig-degradation-alert rule forbids."""
    if not records or (exit_code is not None and exit_code not in (0, 1, 2)):
        return f"prober-down:exit{exit_code}"
    fails = alert_signature(records)
    return f"fail:{fails}" if fails else ""


def history_entry(text, exit_code, ts):
    """One JSONL history row from a captured audit run: ts, exit code, counts, and a compact
    per-node (verdict + problems) list -- enough to re-render a run's shape without its full text."""
    recs = parse_audit(text)
    s = summarize(recs)
    return {"ts": ts, "exit": exit_code,
            "counts": {"pass": s["pass"], "warn": s["warn"], "fail": s["fail"]},
            "nodes": [{"node": r["node"], "verdict": r["verdict"], "problems": r["problems"]}
                      for r in recs]}


# --------------------------------------------------------------------------- pure: render
def render_json(records, version, generated_at, history=None, exit_code=None):
    """Machine-readable page payload (status.json)."""
    s = summarize(records)
    payload = {
        "version": version,
        "generated_at": generated_at,
        "overall": overall_state(records, exit_code),
        "exit_code": exit_code,
        "counts": {"pass": s["pass"], "warn": s["warn"], "fail": s["fail"]},
        "nodes": records,
        "history": (history or [])[-HISTORY_SHOWN:],
    }
    return json.dumps(payload, indent=2, ensure_ascii=False)


_STYLE = """<style>
:root{color-scheme:light dark;--bg:#f6f7f9;--fg:#1a1c1e;--card:#fff;--line:#d7dade;
--pass-fg:#0f6b28;--pass-bg:#e6f4ea;--warn-fg:#8a5a00;--warn-bg:#fff4e0;--fail-fg:#b3261e;--fail-bg:#fce8e6;--muted:#5a6067}
@media (prefers-color-scheme:dark){:root{--bg:#16181b;--fg:#e6e8ea;--card:#22262b;--line:#343a41;
--pass-bg:#0f2a17;--warn-bg:#2c2208;--fail-bg:#2c1210;--muted:#9aa2ab}}
*{box-sizing:border-box}
body{margin:0;padding:1.4rem;background:var(--bg);color:var(--fg);
font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;line-height:1.4}
h1{margin:0;font-size:1.35rem}h2{font-size:1.05rem;margin:1.6rem 0 .5rem}
.meta{color:var(--muted);font-size:.85rem;margin-top:.25rem}
.ver{font-weight:600;color:var(--fg)}
.banner{margin:1rem 0;padding:.7rem 1rem;border-radius:.5rem;font-weight:700;letter-spacing:.02em}
.b-PASS{background:var(--pass-bg);color:var(--pass-fg)}
.b-WARN{background:var(--warn-bg);color:var(--warn-fg)}
.b-FAIL{background:var(--fail-bg);color:var(--fail-fg)}
.b-ERROR{background:var(--fail-bg);color:var(--fail-fg)}
table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line);
border-radius:.5rem;overflow:hidden;font-size:.9rem}
th,td{text-align:left;padding:.5rem .7rem;border-bottom:1px solid var(--line);vertical-align:top}
th{background:transparent;color:var(--muted);font-weight:600;font-size:.78rem;text-transform:uppercase;letter-spacing:.03em}
tr:last-child td{border-bottom:none}
.badge{font-weight:700;white-space:nowrap;width:1%}
.node{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-weight:600;width:1%;white-space:nowrap}
.chip{display:inline-block;margin:.1rem .25rem .1rem 0;padding:.08rem .45rem;border-radius:.35rem;
background:rgba(127,127,127,.14);font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.82rem}
.chip b{color:var(--muted);font-weight:600}
.chip.flag{background:rgba(127,127,127,.22)}
.problems{margin-top:.35rem;padding:.3rem .55rem;border-radius:.35rem;background:var(--fail-bg);
color:var(--fail-fg);font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.82rem;font-weight:600}
tr.v-WARN .problems{background:var(--warn-bg);color:var(--warn-fg)}
footer{margin-top:1.5rem;color:var(--muted);font-size:.78rem}
</style>"""


def _esc(x):
    return html.escape(str(x), quote=True)


def _node_row(r):
    vb = r["verdict"]
    chips = []
    for f in r["facets"]:
        if "flag" in f:
            chips.append(f'<span class="chip flag">{_esc(f["flag"])}</span>')
        else:
            chips.append(f'<span class="chip"><b>{_esc(f["key"])}</b> {_esc(f["value"])}</span>')
    probs = f'<div class="problems">{_esc(r["problems"])}</div>' if r["problems"] else ""
    return (f'<tr class="v-{vb}"><td class="badge b-{vb}">{vb}</td>'
            f'<td class="node">{_esc(r["node"])}</td>'
            f'<td class="facets">{"".join(chips)}{probs}</td></tr>')


def _history_table(history):
    rows = []
    for h in (history or [])[-HISTORY_SHOWN:][::-1]:
        c = h.get("counts", {})
        ov = "FAIL" if c.get("fail") else ("WARN" if c.get("warn") else "PASS")
        rows.append(
            f'<tr class="v-{ov}"><td>{_esc(h.get("ts", ""))}</td>'
            f'<td class="badge b-{ov}">{ov}</td>'
            f'<td>{c.get("pass", 0)} PASS / {c.get("warn", 0)} WARN / {c.get("fail", 0)} FAIL</td></tr>')
    if not rows:
        return ""
    return ('<h2>Posledné behy</h2><table class="hist"><thead><tr><th>čas (UTC)</th>'
            '<th>verdikt</th><th>počty</th></tr></thead><tbody>' + "".join(rows) + "</tbody></table>")


def render_html(records, version, generated_at, history=None, exit_code=None):
    """The status page. version-on-dashboard: the version is DOM-readable (a data-version
    attribute AND visible text) alongside the page generation timestamp. `exit_code` (the audit's
    exit) turns an empty / crashed sweep into an explicit ERROR banner, never a false green."""
    s = summarize(records)
    overall = overall_state(records, exit_code)
    ordered = sorted(records, key=lambda r: (_BADGE_ORDER.get(r["verdict"], 9), r["node"]))
    banner_text = {
        "PASS": "VŠETKY NODY ZDRAVÉ",
        "WARN": "PROBLÉMY NIŽŠIE — POZRI TABUĽKU",
        "FAIL": "PROBLÉMY NIŽŠIE — POZRI TABUĽKU",
        "ERROR": "AUDIT ZLYHAL — ŽIADNE ZDRAVOTNÉ DÁTA (prober down, pozri dev1 log)",
    }.get(overall, "PROBLÉMY NIŽŠIE — POZRI TABUĽKU")
    head = (
        '<!doctype html><html lang="sk"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width,initial-scale=1">'
        '<meta http-equiv="refresh" content="60">'
        f'<title>Rig Health — {overall}</title>{_STYLE}</head>')
    header = (
        '<header><h1>Rig Health</h1>'
        f'<div class="meta">verzia '
        f'<span class="ver" data-version="{_esc(version)}">v{_esc(version)}</span>'
        f' · vygenerované <time datetime="{_esc(generated_at)}">{_esc(generated_at)}</time>'
        f' · <b>{s["pass"]}</b> PASS / <b>{s["warn"]}</b> WARN / <b>{s["fail"]}</b> FAIL</div></header>')
    banner = f'<div class="banner b-{overall}" data-overall="{overall}">{banner_text} — {overall}</div>'
    body_rows = ("".join(_node_row(r) for r in ordered) if ordered
                 else '<tr class="v-FAIL"><td class="badge b-ERROR">ERROR</td>'
                      '<td class="node">—</td><td class="facets">žiadne dáta z '
                      'rig-health-audit.py (prober zlyhal alebo timeout — pozri dev1 log)</td></tr>')
    table = ('<table class="nodes"><thead><tr><th>verdikt</th><th>node</th><th>signály</th></tr>'
             '</thead><tbody>' + body_rows + "</tbody></table>")
    footer = ('<footer>zdroj: rig-health-audit.py (jediný prober) · read-only · dev1 · '
              'obnovuje sa každých 60 s · #787</footer>')
    return (head + '<body>' + header + banner + table + _history_table(history) + footer
            + '</body></html>')


# --------------------------------------------------------------------------- I/O (update path)
def _read_version():
    path = os.path.join(REPO_ROOT, "Cargo.toml")
    if not os.path.exists(path):
        return "unknown"
    with open(path) as fh:
        for line in fh:
            m = re.match(r'^version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    return "unknown"


def _now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _run_audit():
    """Run rig-health-audit.py, return (stdout, exit_code). A hang past AUDIT_TIMEOUT_S or a spawn
    failure NEVER crashes the updater -- it degrades to a sentinel exit (124 timeout / 127 spawn)
    with whatever partial output was captured, so the page renders an explicit ERROR, not nothing."""
    try:
        proc = subprocess.run([sys.executable, AUDIT], capture_output=True, text=True,
                              timeout=AUDIT_TIMEOUT_S)
        return proc.stdout, proc.returncode
    except subprocess.TimeoutExpired as exc:
        partial = exc.output or ""
        if isinstance(partial, bytes):
            partial = partial.decode("utf-8", "replace")
        return partial, 124
    except OSError:
        return "", 127


def _atomic_write(path, text):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write(text)
    os.replace(tmp, path)


def _append_history(entry, path):
    lines = []
    if os.path.exists(path):
        with open(path, encoding="utf-8") as fh:
            lines = [ln for ln in fh.read().splitlines() if ln.strip()]
    lines.append(json.dumps(entry, ensure_ascii=False))
    lines = lines[-HISTORY_MAX:]
    _atomic_write(path, "\n".join(lines) + "\n")


def _load_history(path, log=None):
    out = []
    skipped = 0
    if not os.path.exists(path):
        return out
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                skipped += 1       # a truncated/corrupt row; skip it, never crash -- but surface it
    if skipped and log:
        log(f"history: skipped {skipped} corrupt JSONL row(s) in {path}")
    return out


def _read_state(path):
    state = {}
    if not os.path.exists(path):
        return state
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if "=" in line:
                k, v = line.rstrip("\n").split("=", 1)
                state[k] = v
    return state


def _write_state(path, state):
    _atomic_write(path, "".join(f"{k}={v}\n" for k, v in state.items()))


def _throttle(current_sig, prior_sig, prior_passes, throttle_n):
    """Reuse the tested bash kernel scripts/lib/obs-watchdog-decision.sh -- no second throttle
    implementation. Returns {alert_now, new_sig, new_passes}; any failure to run the kernel
    degrades to alert_now=1 (fail LOUD, never silently drop a FAIL alert)."""
    script = 'set -eu\n. "$1"\nobs_watchdog_alert_throttle "$2" "$3" "$4" "$5"\n'
    try:
        proc = subprocess.run(
            ["bash", "-c", script, "bash", THROTTLE_LIB,
             current_sig, prior_sig, str(prior_passes), str(throttle_n)],
            capture_output=True, text=True, timeout=10)
    except (subprocess.TimeoutExpired, OSError):
        return {"alert_now": "1", "new_sig": current_sig, "new_passes": "1"}
    if proc.returncode != 0:
        return {"alert_now": "1", "new_sig": current_sig, "new_passes": "1"}
    out = {}
    for line in proc.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    out.setdefault("alert_now", "1")     # kernel always prints it; a missing key = fail LOUD, never drop
    return out


def _maybe_alert(records, exit_code, state_dir, notify, dry_run, log):
    sig = alert_condition(records, exit_code)
    state_path = os.path.join(state_dir, "alert.state")
    prior = _read_state(state_path)
    if not sig:
        # healthy -> clear the fingerprint so the next problem re-pings immediately. NEVER on a dry
        # run: a --dry-run must leave persistent throttle state untouched, or it silently consumes
        # the fingerprint and suppresses the next REAL alert for a throttle window.
        if not dry_run:
            _write_state(state_path, {"alert_sig": "", "alert_passes": "0"})
        return False
    t = _throttle(sig, prior.get("alert_sig", ""), prior.get("alert_passes", "0"),
                  ALERT_THROTTLE_PASSES)
    if dry_run:
        would = "alert" if t.get("alert_now") == "1" else "suppress"
        log(f"[dry-run] WOULD {would} ({sig}); throttle state UNCHANGED")
        return False
    _write_state(state_path, {"alert_sig": t.get("new_sig", ""),
                              "alert_passes": t.get("new_passes", "0")})
    if t.get("alert_now") != "1":
        log(f"alert suppressed by throttle ({sig})")
        return False
    if sig.startswith("prober-down"):
        body = (f"\U0001F6A8 #787 rig-status: rig-health-audit PROBER FAILED ({sig}) -- camera-box. "
                f"Status stranka nema data -- pozri dev1 log.")
    else:
        body = (f"\U0001F6A8 #787 rig-status: rig node(s) FAIL ({sig}) -- camera-box. "
                f"Pozri status stranku.")
    try:
        subprocess.run([sys.executable, notify, "notify", "--body", body,
                        "--dedup-key", f"rig-status-{sig}"],
                       capture_output=True, timeout=30)
        log(f"ALERT: fired Discord notification ({sig})")
    except (subprocess.TimeoutExpired, OSError) as exc:
        log(f"ALERT: airuleset notify failed (non-fatal): {exc}")
    return True


# --------------------------------------------------------------------------- subcommands
def cmd_update(args):
    def log(msg):
        print(f"{_now_iso()} [rig-status] {msg}", file=sys.stderr)

    serve_dir = args.dir            # PUBLISHED artifacts only: index.html + status.json
    state_dir = args.state_dir      # PRIVATE: history.jsonl + alert.state (never web-served)
    os.makedirs(serve_dir, exist_ok=True)
    os.makedirs(state_dir, exist_ok=True)
    text, exit_code = _run_audit()
    ts = _now_iso()
    hist_path = os.path.join(state_dir, "history.jsonl")
    _append_history(history_entry(text, exit_code, ts), hist_path)
    records = parse_audit(text)
    version = _read_version()
    history = _load_history(hist_path, log)
    _atomic_write(os.path.join(serve_dir, "index.html"),
                  render_html(records, version, ts, history, exit_code))
    _atomic_write(os.path.join(serve_dir, "status.json"),
                  render_json(records, version, ts, history, exit_code))
    _maybe_alert(records, exit_code, state_dir, args.notify, args.dry_run, log)
    s = summarize(records)
    log(f"exit={exit_code} overall={overall_state(records, exit_code)} "
        f"{s['pass']}P/{s['warn']}W/{s['fail']}F -> {serve_dir}/index.html")
    return 0


def cmd_render(args):
    if args.input == "-":
        text = sys.stdin.read()
    else:
        with open(args.input, encoding="utf-8") as fh:
            text = fh.read()
    records = parse_audit(text)
    version = args.version or _read_version()
    out = render_html(records, version, _now_iso())
    if args.out:
        _atomic_write(args.out, out)
    else:
        sys.stdout.write(out)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description="Render the rig-health-audit sweep as a status page (#787).")
    sub = ap.add_subparsers(dest="cmd", required=True)

    up = sub.add_parser("update", help="run the audit, append history, render the page, alert on FAIL")
    up.add_argument("--dir", default=DEFAULT_DIR,
                    help="SERVE dir (published index.html + status.json; this is what http.server exposes)")
    up.add_argument("--state-dir", default=DEFAULT_STATE_DIR,
                    help="PRIVATE state dir (history.jsonl + alert.state; never web-served)")
    up.add_argument("--notify", default=NOTIFY, help="airuleset.py path for the Discord alert")
    up.add_argument("--dry-run", action="store_true", help="render + decide, but never fire the Discord alert")
    up.set_defaults(func=cmd_update)

    rn = sub.add_parser("render", help="re-render the page from captured audit text (no ssh/WS)")
    rn.add_argument("--input", "-i", default="-", help="audit text file, or '-' for stdin")
    rn.add_argument("--out", "-o", default="", help="write HTML here (default: stdout)")
    rn.add_argument("--version", default="", help="version label (default: read Cargo.toml)")
    rn.set_defaults(func=cmd_render)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
