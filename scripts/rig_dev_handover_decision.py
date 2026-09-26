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
# #1319: a problem the SUPERVISOR (not the owner) must fix -- e.g. a production-critical dev1
# watchdog timer left disabled/inactive/never-run. The owner MUST see it, but is NEVER blamed for
# it (its Slovak line says `nezapnutý watchdog: …`, not `zabudol si: …`). A not-clean state -> exit 1.
SUPERVISOR = "SUPERVISOR"
UNKNOWN = "UNKNOWN"

GLYPH = {OK: "✅", FORGOT: "❌", FIXED: "\U0001f527", SUPERVISOR: "\U0001f6e0",
         UNKNOWN: "❔"}  # ✅ ❌ 🔧 🛠 ❔

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


# --- item 16: watchdogs (#1319) -----------------------------------------------------------------
# The OK-recency bar: a production-critical dev1 watchdog timer must have fired within this many
# seconds. Env-overridable (RDH_WATCHDOG_MAX_AGE_S) -- the fleet's watchdog timers all run on
# <= 5-min cadences, so 15 min is a safe "hasn't fired recently" ceiling.
WD_MAX_AGE_S = 900

# One raw per-timer line the bash probe (scripts/rig-dev-handover-check.sh probe_watchdogs) emits:
#   watchdog <timer> scope=<core|imag> unit=<present|absent> enabled=<yes|no> active=<yes|no> age_s=<N|na>
_WD_LINE_RE = re.compile(
    r"^watchdog\s+(\S+)\s+scope=(\S+)\s+unit=(\S+)\s+enabled=(\S+)\s+active=(\S+)\s+age_s=(\S+)")


def _wd_reason(unit, enabled, active, age_s, max_age_s):
    """Classify ONE timer's raw fields: 'ok' | 'off-*' (a supervisor problem) | 'missing'."""
    if unit != "present":
        return "missing"
    if enabled != "yes":
        return "off-disabled"
    if active != "yes":
        return "off-inactive"
    if age_s == "na":
        return "off-neverrun"
    try:
        if int(age_s) > max_age_s:
            return "off-stale"
    except (TypeError, ValueError):
        return "off-neverrun"
    return "ok"


def classify_watchdogs(text, imag_retired=False, max_age_s=WD_MAX_AGE_S):
    """Decide the `watchdogs` item from the probe's raw per-timer lines.

    A disabled/inactive/never-run/stale timer is a SUPERVISOR problem (never blamed on the owner);
    a timer with no unit file reads UNKNOWN with the names; nothing readable reads UNKNOWN.
    `imag_retired` drops scope=imag timers (issue 1316) so a retired imag never forces a problem.
    Returns {status, message, off, missing, ok}."""
    off, missing, ok = [], [], []
    scanned = 0
    for line in (text or "").splitlines():
        m = _WD_LINE_RE.match(line.strip())
        if not m:
            continue
        name, scope, unit, enabled, active, age_s = m.groups()
        if imag_retired and scope == "imag":
            continue
        scanned += 1
        reason = _wd_reason(unit, enabled, active, age_s, max_age_s)
        if reason == "ok":
            ok.append(name)
        elif reason == "missing":
            missing.append(name)
        else:
            off.append(name)

    if off:
        msg = "nezapnutý watchdog: " + ", ".join(off)
        if missing:
            msg += "; chýba unit súbor pre: " + ", ".join(missing)
        status = SUPERVISOR
    elif missing:
        msg = ("chýba unit súbor pre watchdog: " + ", ".join(missing)
               + " — over inštaláciu na dev1 (neoverené)")
        status = UNKNOWN
    elif scanned == 0:
        msg = "žiadny watchdog timer sa nepodarilo prečítať (systemctl --user na dev1 nedostupné)"
        status = UNKNOWN
    else:
        msg = ("všetkých %d watchdog timerov beží (enabled+active, spustené za posledných 15 min)"
               % len(ok))
        status = OK
    return {"status": status, "message": msg, "off": off, "missing": missing, "ok": ok}


# --- item 17: exposure (issue 1371) --------------------------------------------------------------
# The ONE line `camera_test_settings.py snapshot-state` prints about the test camera's production
# ISO/shutter snapshot (taken by the E2E before its first exposure set, restored + moved aside by
# `rig-mode.sh event`):
#   exposure state=none | pending box=.. taken=.. <key>=<value>... | restored restored=.. ... | invalid path=.. reason=..
_EXPOSURE_LINE_RE = re.compile(r"^exposure\s+state=(none|pending|restored|invalid)\b(.*)$")


def classify_exposure(text, mode_text=""):
    """Decide the `exposure` item from the snapshot-state line.
      none / restored            -> OK
      pending, rig in TEST       -> OK  (a snapshot of this development period, waiting for its EVENT
                                         switch -- the E2E takes it; never a false alarm)
      pending, rig in EVENT      -> SUPERVISOR (the handover moment: the EVENT switch never restored
                                         it, e.g. it aborted before the restore step, so no marker)
      pending, rig mode unknown  -> UNKNOWN
      pending + restore_failed=  -> SUPERVISOR (an EVENT switch tried and did NOT restore it, so
                                         production ran on the TEST exposure)
      invalid                    -> SUPERVISOR (the owner's values are stuck in an unreadable file)
      no readable line           -> UNKNOWN
    SUPERVISOR is never the owner's fault. Returns {status, message}."""
    m = None
    for line in (text or "").splitlines():
        m = _EXPOSURE_LINE_RE.match(line.strip()) or m
    if m is None:
        return {"status": UNKNOWN,
                "message": "stav produkčnej expozície testovacej kamery sa nepodarilo prečítať "
                           "(~/.camera-box na dev1)"}
    state, detail = m.group(1), m.group(2).strip()
    if state == "none":
        return {"status": OK,
                "message": "žiadna produkčná expozícia nečaká na vrátenie (test kameru nemenil)"}
    if state == "restored":
        return {"status": OK,
                "message": "produkčná expozícia testovacej kamery bola naposledy vrátená (%s)" % detail}
    mode = bare_token(mode_text)
    if state == "pending" and "restore_failed=" not in detail and mode == "TEST":
        return {"status": OK,
                "message": "produkčná expozícia testovacej kamery čaká na vrátenie pri najbližšom "
                           "`scripts/rig-mode.sh event` (%s)" % detail}
    if state == "pending" and "restore_failed=" not in detail and mode != "EVENT":
        return {"status": UNKNOWN,
                "message": "čaká snímka produkčnej expozície (%s), ale rig režim sa nepodarilo "
                           "prečítať — neoverené, či ju EVENT mal vrátiť" % detail}
    if state == "pending":
        return {"status": SUPERVISOR,
                "message": "produkčná expozícia testovacej kamery sa pri EVENT NEVRÁTILA — "
                           "produkcia bežala na testovacej expozícii; snímka stále čaká (%s). Zisti "
                           "prečo (kamera nebola na USB / nesedel read-back / bežal prenos), potom "
                           "`scripts/rig-mode.sh event` s kamerou na USB, keď rig nevysiela, ju vráti"
                           % detail}
    return {"status": SUPERVISOR,
            "message": "snímka produkčnej expozície testovacej kamery je nečitateľná (%s) — "
                       "hodnoty vlastníka treba z nej obnoviť ručne" % detail}


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

    def decide(self, captures_data, imag_retired=False):
        """`captures_data`: {name: (text, rc)} for THIS item's captures (a missing name reads as
        ("", RC_MISSING)). Combine the per-capture statuses (FORGOT > OK > UNKNOWN) and produce the
        Slovak line. `imag_retired` (issue 1316) is used only by the `watchdogs` kind."""
        if self.kind == "exposure":
            text, _rc = captures_data.get(self.captures[0], ("", RC_MISSING))
            mode_text, _mrc = captures_data.get(self.captures[1], ("", RC_MISSING))
            r = classify_exposure(text, mode_text)
            return {"key": self.key, "label": self.label, "status": r["status"],
                    "message": r["message"]}
        if self.kind == "watchdogs":
            text, _rc = captures_data.get(self.captures[0], ("", RC_MISSING))
            r = classify_watchdogs(text, imag_retired=imag_retired)
            entry = {"key": self.key, "label": self.label, "status": r["status"],
                     "message": r["message"]}
            if r["status"] == SUPERVISOR:
                entry["names"] = r["off"]  # the timers the supervisor must (re-)enable, for the summary
            return entry
        statuses = []
        for name in self.captures:
            # issue 1316: a retired imag-nb (returned to the owner) drops its OWN capture (e.g.
            # `pins_imag`) so the item is judged on the remaining boxes only — probing the dark box
            # would otherwise read UNKNOWN/timeout and drag the whole item to UNKNOWN. The other
            # imag-scoped gate (dantesync version) EXCLUDES imag-nb via its rig-fleet.txt ack.
            # PRECISE match on the box-suffix convention (`<item>_imag`) — never a bare `in` that
            # could drop an unrelated future capture like `imaging`/`mapping_imag`.
            if imag_retired and (name == "imag" or name.endswith("_imag")):
                continue
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
    # #1309: per-cambox bkshading-relay state so "shading is off on cam1" is REPORTED, never
    # discovered. verdict-kind like `mic`: SHADING-ON = a box actively shading with camera online
    # (good); SHADING-DEAD = relay enabled but not running = a crashed relay that should be up
    # (forgot -- the 2026-09-15 "shading crashol" class). SHADING-OFF (disabled = the TEST-mode
    # default), SHADING-NO-CAMERA and SHADING-UNREACHABLE are neutral -> UNKNOWN, never a false
    # forgot (in development the relay is legitimately disabled per bkshading.md, so an all-off
    # fleet reads UNKNOWN + the per-box detail in the capture, not a page). The `mode` item already
    # catches a whole rig left in EVENT.
    Item("shading", "bkshading (clona) relay na camboxoch", ["shading"], "verdict",
         ok_msg="shading relay beží a kamera je online aspoň na jednom camboxe",
         forgot_msg="bkshading relay je zapnutý ale spadol na niektorom camboxe (SHADING-DEAD) — "
                    "reštartuj / preprovizuj relay (issue 1309)",
         unknown_msg="shading relay je vypnutý / bez kamery / nedostupný na camboxoch — v TEST "
                     "režime je to správne; pozri capture pre stav per box (issue 1309)",
         good={"SHADING-ON"}, forgot={"SHADING-DEAD"}),
    Item("avlatency", "meracia zvuková cesta: latencia oproti baseline", ["avlatency"], "verdict",
         ok_msg="meracia zvuková cesta má latenciu v norme oproti baseline (rozdiel do 90 ms)",
         forgot_msg="meracia zvuková cesta (mbc/Ableton reťazec) má posunutú latenciu oproti baseline "
                    "(nad 90 ms) — over DVS/Dante/Ableton mbc cestu; baseline sa prepisuje po zelenom "
                    "E2E cez `scripts/measurement-chain-latency.sh --baseline`",
         unknown_msg="latenciu meracej cesty sa nepodarilo zmerať (cam2 dole / marker log chýba / "
                     "meracia cesta ticho pod −60 dB / málo onsetov / žiadna baseline / painter "
                     "emit_ts nie je wall-clock / stream OBS nedostupné) — over v TEST režime",
         good={"ALIGNED"}, forgot={"DRIFTED"}),
    # #1319: the dev1 `--user` production-critical alert-watchdog TIMERS must be enabled + active +
    # fired recently. Their absence is what let av-step/avsync-lineup go uninstalled unnoticed. A
    # disabled/inactive/never-run/stale timer -> SUPERVISOR (the supervisor's to fix, never the
    # owner's fault); a timer with no unit file -> UNKNOWN. The roster is the ONE source of truth
    # scripts/lib/watchdog-roster.sh; the "watchdogs" kind uses classify_watchdogs (above), not the
    # verdict/exit reducers, because it must carry the offending timer NAMES into the message/summary.
    Item("watchdogs", "dev1 watchdog timery", ["watchdogs"], "watchdogs",
         ok_msg="všetky production-critical watchdog timery na dev1 bežia",
         forgot_msg="",  # never emitted for this kind (no owner-forgot path)
         unknown_msg="stav watchdog timerov sa nepodarilo prečítať"),
    # issue 1371: the test camera's production ISO/shutter snapshot. The "exposure" kind uses
    # classify_exposure (above): its message carries the snapshot detail (box, time, values).
    # It also reads the `mode` capture: a pending snapshot is normal in TEST, but in EVENT (the
    # handover moment) it means the EVENT switch never restored it.
    Item("exposure", "expozícia testovacej kamery (ISO/uzávierka)", ["exposure", "mode"], "exposure",
         ok_msg="", forgot_msg="", unknown_msg=""),  # messages come from classify_exposure
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
    supervisor = []  # #1319: timer names the supervisor must (re-)enable; owner is not blamed
    supervisor_other = []  # issue 1371: a non-watchdog supervisor problem (the item label)
    for e in entries:
        lines.append("%s %s: %s" % (GLYPH[e["status"]], e["label"], e["message"]))
        if e["status"] == FORGOT:
            forgot.append(e["label"])
        elif e["status"] == SUPERVISOR and e.get("key") != "watchdogs" and not e.get("names"):
            supervisor_other.append(e["label"])
        elif e["status"] == SUPERVISOR:
            supervisor.extend(e.get("names") or [e["label"]])
        elif e["status"] == UNKNOWN:
            unknown.append(e["label"])

    if forgot:
        summary = "zabudol si: " + ", ".join(forgot)
        if supervisor:
            summary += " (supervisor musí zapnúť: " + ", ".join(supervisor) + ")"
        if supervisor_other:
            summary += " (supervisor musí vyriešiť: " + ", ".join(supervisor_other) + ")"
        if unknown:
            summary += " (neoverené: " + ", ".join(unknown) + ")"
        exit_code = 1
    elif supervisor or supervisor_other:
        parts = []
        if supervisor:
            parts.append("supervisor musí zapnúť watchdogy: " + ", ".join(supervisor))
        if supervisor_other:
            parts.append("supervisor musí vyriešiť: " + ", ".join(supervisor_other))
        summary = "; ".join(parts)
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


def evaluate(work_dir, items=None, imag_retired=False):
    """Read every item's captures from work_dir and decide. Returns (entries, lines, summary,
    exit_code). `imag_retired` (issue 1316) drops imag-scoped watchdog timers."""
    items = ITEMS if items is None else items
    entries = []
    for item in items:
        cap = {name: _read_capture(work_dir, name) for name in item.captures}
        entries.append(item.decide(cap, imag_retired=imag_retired))
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

    # #1316: once imag-nb is retired, drop imag-scoped watchdog timers via RDH_IMAG_RETIRED.
    imag_retired = os.environ.get("RDH_IMAG_RETIRED", "").strip().lower() not in ("", "0", "false", "no")
    entries, lines, summary, exit_code = evaluate(ns.work_dir, imag_retired=imag_retired)

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
