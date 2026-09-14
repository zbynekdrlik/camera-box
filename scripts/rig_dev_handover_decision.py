#!/usr/bin/env python3
# scripts/rig_dev_handover_decision.py -- #1312: the PURE decision engine of the "development"
# handover check. The bash orchestrator (scripts/rig-dev-handover-check.sh) runs each EXISTING
# read-only probe (the dev1 alert-watchdog --dry-runs, obs_burn_filter sweep-check,
# set-ndi-mapping --verify-only, latency_pins_verify, dantesync-version-gate, rig-mode-state)
# bounded + drain-safe, captures each probe's merged stdout+stderr into <work-dir>/<name>.out and
# its exit code into <work-dir>/<name>.rc, then calls this module. This module PARSES those raw
# captures and maps every checklist item to one of OK / FORGOT-BY-OWNER / FIXED-BY-ME / UNKNOWN
# plus the Slovak line the supervisor pastes to the owner, and exits non-zero when anything is
# FORGOT or UNKNOWN.
#
# WHY pure (#1312): all logic -- the verdict-line parsing AND the decision table -- lives here so it
# is exhaustively pytest-testable (Tier-0, #557: no cargo) against captured dry-run fixtures, with
# no live box. The bash side is thin I/O only. This mirrors the repo's established
# `*_decision.py` shape (audio_lag_decision.py / genlock_lock_decision.py).
#
# The check REPORTS, it never mutates. FIXED-BY-ME is reserved for a future `--fix` mode (a
# followup, never this lane) -- this report-only lane only ever emits OK / FORGOT-BY-OWNER /
# UNKNOWN.

import argparse
import json
import os
import re
import sys

# --- status vocabulary ---------------------------------------------------------------------------
OK = "OK"
FORGOT = "FORGOT-BY-OWNER"
FIXED = "FIXED-BY-ME"  # reserved for a future --fix mode; never emitted by this report-only lane
UNKNOWN = "UNKNOWN"

GLYPH = {OK: "✅", FORGOT: "❌", FIXED: "\U0001f527", UNKNOWN: "❔"}  # ✅ ❌ 🔧 ❔

# A missing capture file / a probe that could not be run reads as this exit code sentinel.
RC_MISSING = 127


# --- low-level pure parsers ----------------------------------------------------------------------
_VERDICT_RE = re.compile(r"verdict=(\S+)")
_NET_RE = re.compile(r"->\s*(REACHABLE|UNREACHABLE)\b")


def verdict_tokens(text):
    """Every `verdict=<TOKEN>` token in the captured watchdog output, in order.

    Covers the alert-watchdog log family (measurement-audio, dantesync-clock, audio-lag [lag AND
    band arms], genlock-lock, obs-liveness, optical-chain) -- each logs one or more
    `NAME (ip): reachable=.. verdict=TOKEN ..` (or `... -> verdict=TOKEN`) lines to stderr, which
    the orchestrator captures via `2>&1`. A token is `\\S+` so a prefixed token like
    `alert:PAINTER-DEAD` or `log-only:PAINTER-DEAD-optical-ok` is captured whole."""
    return _VERDICT_RE.findall(text or "")


def net_tokens(text):
    """Every REACHABLE/UNREACHABLE token from network-reach-alert-watchdog's per-box line
    `$box ($ip): ping=.. ws:..=.. bundle:..=.. report_only=.. -> REACHABLE|UNREACHABLE` (it does
    NOT use the `verdict=` shape)."""
    return _NET_RE.findall(text or "")


def bare_token(text):
    """The last non-empty stripped line -- the shape rig-mode-state.sh's
    `rig_mode_from_painter_snapshot` emits (a bare TEST / EVENT / UNKNOWN). An empty capture
    (ssh to cam2 failed / box down) yields None -> UNKNOWN."""
    for line in reversed((text or "").splitlines()):
        s = line.strip()
        if s:
            return s
    return None


def reduce_tokens(tokens, good, forgot, good_prefixes=(), forgot_prefixes=()):
    """Combine per-box/per-arm tokens into ONE item status. FAIL-LOUD toward FORGOT, then OK, then
    UNKNOWN:
      - any token in `forgot` (or starting with a `forgot_prefixes` entry)  -> FORGOT
      - else any token in `good` (or starting with a `good_prefixes` entry) -> OK
      - else (no token, or only SKIP/UNKNOWN-class tokens)                  -> UNKNOWN
    A token that is neither good nor forgot (SKIP, UNKNOWN, a probe error) contributes nothing, so
    an all-unreachable / absent-facet item reads UNKNOWN, never a false OK or FORGOT."""
    seen_good = False
    seen_forgot = False
    for t in tokens:
        if t in forgot or any(t.startswith(p) for p in forgot_prefixes):
            seen_forgot = True
        elif t in good or any(t.startswith(p) for p in good_prefixes):
            seen_good = True
    if seen_forgot:
        return FORGOT
    if seen_good:
        return OK
    return UNKNOWN


def status_from_exit(rc, ok_codes, forgot_codes):
    """Map a captured exit code (from an exit-code-carrying probe: obs_burn_filter sweep-check,
    set-ndi-mapping --verify-only, latency_pins_verify, dantesync-version-gate) to a status. Any
    code outside both sets (2/enum-fail, a `timeout` 124/137, the RC_MISSING sentinel) -> UNKNOWN
    (fail-safe: an unreadable probe is never a false OK)."""
    try:
        rc = int(rc)
    except (TypeError, ValueError):
        return UNKNOWN
    if rc in ok_codes:
        return OK
    if rc in forgot_codes:
        return FORGOT
    return UNKNOWN


# --- item specifications ------------------------------------------------------------------------
class Item(object):
    """One checklist item. `captures` are the <name> keys the orchestrator wrote (.out/.rc). `kind`
    selects the parser. Slovak messages are chosen by the decided status."""

    def __init__(self, key, label, captures, kind, ok_msg, forgot_msg, unknown_msg,
                 good=None, forgot=None, good_prefixes=(), forgot_prefixes=(),
                 ok_codes=None, forgot_codes=None):
        self.key = key
        self.label = label
        self.captures = captures
        self.kind = kind
        self.ok_msg = ok_msg
        self.forgot_msg = forgot_msg
        self.unknown_msg = unknown_msg
        self.good = set(good or ())
        self.forgot = set(forgot or ())
        self.good_prefixes = tuple(good_prefixes)
        self.forgot_prefixes = tuple(forgot_prefixes)
        self.ok_codes = set(ok_codes or ())
        self.forgot_codes = set(forgot_codes or ())

    def _status_for_capture(self, text, rc):
        """Status contribution of ONE captured (text, rc) pair, per this item's kind."""
        if self.kind == "verdict":
            return reduce_tokens(verdict_tokens(text), self.good, self.forgot,
                                 self.good_prefixes, self.forgot_prefixes)
        if self.kind == "net":
            return reduce_tokens(net_tokens(text), {"REACHABLE"}, {"UNREACHABLE"})
        if self.kind == "bare":
            t = bare_token(text)
            if t in self.forgot:
                return FORGOT
            if t in self.good:
                return OK
            return UNKNOWN
        if self.kind == "exit":
            return status_from_exit(rc, self.ok_codes, self.forgot_codes)
        raise ValueError("unknown item kind %r" % self.kind)

    def decide(self, captures_data):
        """`captures_data`: {name: (text, rc)} for THIS item's captures (a missing name reads as
        ("", RC_MISSING)). Combine the per-capture statuses (FORGOT > OK > UNKNOWN) and produce the
        Slovak line."""
        statuses = []
        for name in self.captures:
            text, rc = captures_data.get(name, ("", RC_MISSING))
            statuses.append(self._status_for_capture(text, rc))
        status = combine_statuses(statuses)
        msg = {OK: self.ok_msg, FORGOT: self.forgot_msg, UNKNOWN: self.unknown_msg}[status]
        return {"key": self.key, "label": self.label, "status": status, "message": msg}


def combine_statuses(statuses):
    """Combine an item's per-capture statuses (e.g. burns on strih AND stream; pins on strih,
    stream AND imag). FORGOT dominates; then a SINGLE unverified box makes the whole item UNKNOWN
    (never a false OK): OK only when EVERY captured box is OK.
      - any FORGOT                 -> FORGOT
      - else any UNKNOWN           -> UNKNOWN  (an unread box is NOT masked by an OK sibling -- the
                                                honesty the checklist's `neoverené` reporting needs)
      - else (all OK)              -> OK
    Single-capture items are unaffected ([OK]->OK, [UNKNOWN]->UNKNOWN, [FORGOT]->FORGOT)."""
    if FORGOT in statuses:
        return FORGOT
    if not statuses or UNKNOWN in statuses:
        return UNKNOWN
    return OK


# The ordered checklist. Every item reuses an EXISTING probe -- no new probes.
ITEMS = [
    Item("mic", "merací mikrofón (mbc)", ["mic"], "verdict",
         ok_msg="merací mikrofón na kanáli 'mbc' hrá (peak nad prahom)",
         forgot_msg="merací mikrofón je stlmený/vypnutý — odmutuj kanál 'mbc' v Abletone",
         unknown_msg="stav meracieho mikrofónu sa nepodarilo prečítať (stream OBS alebo cam2 nedostupné)",
         good={"PRESENT"}, forgot={"SILENT"}),
    Item("mode", "rig režim (TEST/EVENT)", ["mode"], "bare",
         ok_msg="rig je v TEST režime (development)",
         forgot_msg="rig je v EVENT (produkčnom) režime — spusti `scripts/rig-mode.sh test` "
                    "(obnoví scény, Studio Mode, burny aj painter naraz), POTOM spusti túto "
                    "kontrolu znova (mic/painter sa overia až v TEST režime)",
         unknown_msg="rig režim sa nepodarilo prečítať (cam2 painter probe nedostupné)",
         good={"TEST"}, forgot={"EVENT"}),
    Item("painter", "cam2 painter + optická vetva", ["painter"], "verdict",
         ok_msg="cam2 painter beží a strih program nie je čierny",
         forgot_msg="cam2 painter je mŕtvy alebo strih program je čierny — spusti "
                    "`scripts/rig-mode.sh test` (obnoví painter)",
         unknown_msg="optickú vetvu sa nepodarilo overiť (cam2 alebo strih OBS nedostupné)",
         good={"healthy", "healthy-unverified"}, good_prefixes=("log-only:",),
         forgot_prefixes=("alert:",)),
    Item("burns", "burny na programe (strih+stream)", ["burns_strih", "burns_stream"], "exit",
         ok_msg="genlock_burn je zapnutý na programových vstupoch",
         forgot_msg="burny sú vypnuté na programe — spusti `scripts/rig-mode.sh test` (zapne burny)",
         unknown_msg="stav burnov sa nepodarilo prečítať (OBS nedostupné)",
         ok_codes={1}, forgot_codes={0}),  # sweep-check exit 1 = ≥1 burn ON = dev/TEST OK; 0 = none
    Item("mapping", "NDI mapping (strih)", ["mapping_strih"], "exit",
         ok_msg="NDI mapping na strihu sedí (každý vstup na svojej kamere)",
         forgot_msg="NDI mapping na strihu je rozhodený — spusti `scripts/set-ndi-mapping.py`",
         unknown_msg="NDI mapping sa nepodarilo overiť (strih OBS nedostupné)",
         ok_codes={0}, forgot_codes={1}),
    Item("pins", "latency piny (strih/stream/imag)", ["pins_strih", "pins_stream", "pins_imag"],
         "exit",
         ok_msg="latency piny sú na baseline",
         forgot_msg="latency piny sú mimo baseline — over `scripts/latency_pins_verify.py`",
         unknown_msg="latency piny sa nepodarilo prečítať (OBS nedostupné)",
         ok_codes={0}, forgot_codes={1}),
    Item("clock", "dantesync clock LOCK", ["clock"], "verdict",
         ok_msg="všetky uzly sú zamknuté na video-clock",
         forgot_msg="niektorý uzol stratil clock LOCK (NO_CLOCK/NO_DANTESYNC/MGMT_DEAD)",
         unknown_msg="clock stav sa nepodarilo prečítať (uzly nedostupné)",
         good={"OK"}, forgot={"NO_CLOCK", "NO_DANTESYNC", "MGMT_DEAD"}),
    Item("obs", "OBS liveness (strih+stream)", ["obs"], "verdict",
         ok_msg="oba broadcast OBS renderujú živo",
         forgot_msg="OBS je zaseknutý/mŕtvy na niektorom boxe (FPS-ZERO/WEDGED/WS-DEAD/...)",
         unknown_msg="OBS liveness sa nepodarilo prečítať (WS nedostupné)",
         good={"HEALTHY"},
         forgot={"FPS-ZERO", "WEDGED-RENDER-LAG", "WS-DEAD", "OBS-COUNT-WRONG",
                 "GPU-DEVICE-REMOVED"}),
    Item("net", "sieťová dostupnosť (strih+stream)", ["net"], "net",
         ok_msg="strih aj stream sú dostupné zo siete",
         forgot_msg="niektorý box je nedostupný zo siete",
         unknown_msg="dostupnosť sa nepodarilo vyhodnotiť"),
    Item("audiolag", "audio timeline lag (strih+stream)", ["audiolag"], "verdict",
         ok_msg="audio timeline nezaostáva",
         forgot_msg="audio timeline zaostáva alebo je zamrznuté (LAGGING/STALE/DRIFTING)",
         unknown_msg="audio lag sa nepodarilo prečítať",
         good={"HEALTHY"}, forgot={"LAGGING", "STALE", "DRIFTING"}),
    Item("genlock", "genlock LOCK (strih+stream)", ["genlock"], "verdict",
         ok_msg="genlock je zamknutý",
         forgot_msg="genlock nie je zamknutý (DEGRADED/UNLOCKED)",
         unknown_msg="genlock facet nie je dostupný (stock OBS alebo box nedostupný) — neoverené",
         good={"HEALTHY"}, forgot={"DEGRADED", "UNLOCKED"}),
    Item("dantesync", "dantesync verzia (pin)", ["dantesync"], "exit",
         ok_msg="všetky uzly sú na pripnutej dantesync verzii",
         forgot_msg="dantesync verzia je rozhodená — nie všetky uzly na pine",
         unknown_msg="dantesync verzie sa nepodarilo prečítať (uzly nedostupné)",
         ok_codes={0}, forgot_codes={20}),  # gate exit 0=pass, 20=drift, 11=unknown/incomplete
    Item("cambox", "camera-box build (jednotný)", ["cambox"], "exit",
         ok_msg="camera-box build je jednotný naprieč aktívnymi boxmi (na pine)",
         forgot_msg="camera-box build nie je jednotný — nasaď rovnakú verziu (scripts/deploy-fleet.sh)",
         unknown_msg="camera-box verzie sa nepodarilo prečítať (boxy nedostupné)",
         ok_codes={0}, forgot_codes={20}),  # camera-box-version-gate exit 0=pass, 20=drift, 11=unknown
    # #1312: the mbc measurement-audio chain's LATENCY vs a persisted baseline (a read-only paired
    # measurement: cam2 marker emit vs stream `mbc` burst onset, median vs baseline, |now-base|>90ms).
    # Verdict-kind like `mic`: the standalone scripts/measurement-chain-latency.sh probe computes the
    # verdict token; a monotonic-emit painter / no-baseline / SKIP / missing-probe all read UNKNOWN
    # (never a false forgot). NOT the pin-relative dock av_offset (which read +17 ms while the gate
    # read -140 ms on 14.9.) -- this is an independent absolute measurement.
    Item("avlatency", "meracia zvuková cesta: latencia oproti baseline", ["avlatency"], "verdict",
         ok_msg="meracia zvuková cesta má latenciu v norme oproti baseline (rozdiel do 90 ms)",
         forgot_msg="meracia zvuková cesta (mbc/Ableton reťazec) má posunutú latenciu oproti baseline "
                    "(nad 90 ms) — over DVS/Dante/Ableton mbc cestu; baseline sa prepisuje po zelenom "
                    "E2E cez `scripts/measurement-chain-latency.sh --baseline`",
         unknown_msg="latenciu meracej cesty sa nepodarilo zmerať (cam2 dole / marker log chýba / málo "
                     "onsetov / žiadna baseline / painter emit_ts nie je wall-clock / stream OBS "
                     "nedostupné) — over v TEST režime",
         good={"ALIGNED"}, forgot={"DRIFTED"}),
]


# --- checklist assembly --------------------------------------------------------------------------
def build_checklist(entries):
    """entries: list of decide() dicts. Returns (lines, summary, exit_code).
      lines    -- one `<glyph> <label>: <message>` per item (owner-readable checklist)
      summary  -- the one-line `zabudol si: …` the supervisor pastes verbatim
      exit_code -- 0 all-OK; 1 any FORGOT; else 2 (UNKNOWN present, nothing FORGOT)
    """
    lines = []
    forgot = []
    unknown = []
    for e in entries:
        lines.append("%s %s: %s" % (GLYPH[e["status"]], e["label"], e["message"]))
        if e["status"] == FORGOT:
            forgot.append(e["label"])
        elif e["status"] == UNKNOWN:
            unknown.append(e["label"])

    if forgot:
        summary = "zabudol si: " + ", ".join(forgot)
        if unknown:
            summary += " (neoverené: " + ", ".join(unknown) + ")"
        exit_code = 1
    elif unknown:
        summary = "nič si nezabudol, ale neoverené: " + ", ".join(unknown)
        exit_code = 2
    else:
        summary = "✅ všetko je v development stave, nič si nezabudol"
        exit_code = 0
    return lines, summary, exit_code


def _read_capture(work_dir, name):
    """Read <work-dir>/<name>.out (text) and <name>.rc (int). A missing .out -> ("", RC_MISSING)."""
    out_path = os.path.join(work_dir, name + ".out")
    rc_path = os.path.join(work_dir, name + ".rc")
    if not os.path.exists(out_path):
        return "", RC_MISSING
    try:
        with open(out_path, "r", encoding="utf-8", errors="replace") as fh:
            text = fh.read()
    except OSError:
        text = ""
    rc = RC_MISSING
    try:
        with open(rc_path, "r", encoding="utf-8", errors="replace") as fh:
            rc = int(fh.read().strip() or RC_MISSING)
    except (OSError, ValueError):
        rc = RC_MISSING
    return text, rc


def evaluate(work_dir, items=None):
    """Read every item's captures from work_dir and decide. Returns (entries, lines, summary,
    exit_code)."""
    items = ITEMS if items is None else items
    entries = []
    for item in items:
        cap = {name: _read_capture(work_dir, name) for name in item.captures}
        entries.append(item.decide(cap))
    lines, summary, exit_code = build_checklist(entries)
    return entries, lines, summary, exit_code


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="Decide the camera-box development-handover checklist from captured "
                    "read-only probe output. Exits non-zero when any item is FORGOT or UNKNOWN.")
    ap.add_argument("--work-dir", required=True,
                    help="directory holding <item>.out/<item>.rc probe captures")
    ap.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    ns = ap.parse_args(argv)

    entries, lines, summary, exit_code = evaluate(ns.work_dir)

    if ns.json:
        print(json.dumps({
            "items": entries,
            "summary": summary,
            "exit_code": exit_code,
        }, ensure_ascii=False, indent=2))
    else:
        print("=== camera-box development handover check ===")
        for line in lines:
            print(line)
        print("")
        print(summary)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
