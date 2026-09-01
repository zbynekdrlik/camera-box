#!/usr/bin/env python3
"""#722 -- EVENT-mode CONTRACT: PURE decision + aggregation for rig-mode.sh event's 8-item
machine-checkable assert phase.

Trigger (2026-07-12 live incident, #721): rig-mode event + a manual supervisor checklist BOTH
said "clean" while a QR was live on air; the user caught it by eye minutes before broadcast.
"EVENT mode" must never again depend on someone remembering what to check, and no single earlier
step's own exit code is trusted blindly here -- every item gets its own independent proof.

Architecture: this module is PURE (no ssh, no OBS WebSocket, no subprocess) -- every function
takes ALREADY-GATHERED facts and returns a verdict. Gathering happens elsewhere: fleet SSH
checks in scripts/rig-mode.sh (paint processes, service health, stray units, artifacts), OBS-WS
reads via existing/small CLIs (scripts/obs_burn_filter.py check, scripts/obs_phase2.py
record/stream-status/program-scene/latency-check, scripts/set-ndi-mapping.py --verify-only,
scripts/qr_screenshot_check.py for the pixel proof) invoked as subprocesses from bash. Keeping
the decision layer pure makes it trivially unit-testable with fixtures
(tests/python/test_event_assert.py) and keeps the "was this rig actually clean" judgment in ONE
place, never re-derived ad hoc.

CLI entrypoint (`decide`): reads a facts JSON (assembled by rig-mode.sh's event_mode_ledger_...
/ event assert wiring), computes the 8-item verdict + aggregate, prints the Slovak summary, and
exits 0 (PASS) / 1 (FAIL). Also writes a machine-readable result JSON (consumed by the #724
Discord confirmation composer) when --result-out is given.
"""

import argparse
import json
import sys

# The 8 contract items, in a FIXED order -- both the Slovak summary and the aggregate()
# failed-items list always follow this order, never dict/insertion order (a missing/absent item
# must never silently vanish from the printed summary).
ITEM_ORDER = [
    "paint_processes",
    "pixel_proof",
    "burns_off",
    "no_recordings",
    "services_healthy",
    "latency_calibrated",
    "ndi_mapping",
    "artifacts_cleared",
]

ITEM_LABELS_SK = {
    "paint_processes": "ziadne maliarske (QR) procesy na kamerach",
    "pixel_proof": "obraz neobsahuje ziadny QR kod (pixelovy dokaz)",
    "burns_off": "vypalovanie QR (burn) je vypnute na vsetkych vystupoch",
    "no_recordings": "nic sa nenahrava ani nestreamuje mimo bezneho vysielania",
    "services_healthy": "vsetky kamerove sluzby bezia (6/6), ziadne testovacie jednotky",
    "latency_calibrated": "latencia zodpoveda kalibrovanej hodnote",
    "ndi_mapping": "mapovanie kamier je spravne (#399)",
    "artifacts_cleared": "testovacie artefakty su vymazane",
}


# ---------------------------------------------------------------------------
# Item 1 -- no paint processes fleet-wide.
# ---------------------------------------------------------------------------


def paint_processes_ok(fleet_counts: dict) -> bool:
    """fleet_counts: {box_name: int (pgrep match count) | None}. PASS iff every box reports a
    numeric 0. A box's own value being None/non-numeric (a per-box gather failure) fails
    CLOSED for that box, never raises a TypeError (#1225)."""
    if not fleet_counts:
        return False
    for v in fleet_counts.values():
        if v is None:
            return False
        if int(v) != 0:
            return False
    return True


# ---------------------------------------------------------------------------
# Item 2 -- pixel proof: a QR decode pass over each camera scene's screenshot found NOTHING.
# ---------------------------------------------------------------------------


def pixel_proof_ok(qr_findings: dict) -> bool:
    """qr_findings: {scene_name: [decoded_qr_text, ...] | None}. PASS iff every scene's value is
    a list AND that list is empty. Fails CLOSED on an empty dict -- that means the gather step
    produced nothing at all (e.g. every scene's screenshot/decode failed), never "nothing was
    checked, so nothing was found". A scene whose OWN value is None
    (scripts/qr_screenshot_check.py's explicit "this one scene could not be checked" signal)
    ALSO fails CLOSED as an unreadable facet, never raises (#1225 live incident: a
    screenshot/decode RPC failure on ONE scene crashed the whole aggregate assert with
    `TypeError: object of type 'NoneType' has no len()` instead of an honest FAIL)."""
    if not qr_findings:
        return False
    return all(isinstance(v, list) and len(v) == 0 for v in qr_findings.values())


def pixel_proof_detail(qr_findings: dict) -> str:
    """Slovak-facing detail string for the pixel_proof item's summary line, when there is
    something worth naming: any scene(s) that could not be read at all come first (#1225 -- an
    UNKNOWN/unreadable facet, fail-closed, never a crash -- named "facet unreadable" so an
    operator can tell "a QR was live" apart from "we couldn't even check"); scene(s) where a QR
    *was* found come second. Returns "" when there is nothing extra to add (e.g. every scene was
    cleanly checked and found empty)."""
    if not qr_findings:
        return ""
    unreadable = sorted(s for s, v in qr_findings.items() if not isinstance(v, list))
    found = sorted(s for s, v in qr_findings.items() if isinstance(v, list) and len(v) > 0)
    parts = []
    if unreadable:
        parts.append(f"facet unreadable: {', '.join(unreadable)}")
    if found:
        parts.append(f"QR najdeny: {', '.join(found)}")
    return "; ".join(parts)


# ---------------------------------------------------------------------------
# Item 3 -- burns off on every measurement-burn target.
# ---------------------------------------------------------------------------


def burns_off_ok(burn_states: dict) -> bool:
    """burn_states: {target_label: bool (burn_on)}. PASS iff every target is False."""
    if not burn_states:
        return False
    return all(v is False for v in burn_states.values())


# ---------------------------------------------------------------------------
# Item 4 -- no active recordings/streams anywhere the harness could have started one.
# ---------------------------------------------------------------------------


def no_recordings_ok(rec_states: dict) -> bool:
    """rec_states: {"<box>:record"|"<box>:stream": bool (active)} | None. PASS iff every value
    is False. An empty dict is NOT a failure here (a box legitimately not covered, e.g. imag has
    no recording output) -- callers only include the targets that actually apply. The WHOLE
    facet being None (couldn't gather at all) fails CLOSED instead of raising on `.values()`
    (#1225) -- distinct from the legitimate "no applicable targets" empty-dict case."""
    if rec_states is None:
        return False
    return all(v is False for v in rec_states.values())


# ---------------------------------------------------------------------------
# Item 5 -- services healthy 6/6, no stray test systemd units anywhere.
# ---------------------------------------------------------------------------


def services_healthy_ok(service_active: dict, stray_units: dict) -> bool:
    """service_active: {box: bool}; stray_units: {box: [unit_name, ...] | None} | None. PASS
    iff every box is active AND no box has any stray unit. A box's stray_units value being None
    (a per-box gather failure), or the whole stray_units facet being None, fails CLOSED rather
    than raising on `len()`/`.values()` (#1225)."""
    if not service_active:
        return False
    if not all(v is True for v in service_active.values()):
        return False
    if stray_units is None:
        return False
    return all(isinstance(v, list) and len(v) == 0 for v in stray_units.values())


# ---------------------------------------------------------------------------
# Item 6 -- stream PGM latency == the calibrated value (av-sync-last.json).
# ---------------------------------------------------------------------------


def latency_calibrated_ok(current_ms, calibrated_ms) -> bool:
    """PASS iff current_ms == calibrated_ms (exact match -- this is a SET value, not a measured
    one, so no tolerance is appropriate). Fails CLOSED when calibrated_ms is unknown (None) --
    the #691 stomp-protection risk: never claim "calibrated" when we don't actually know the
    calibrated value to compare against."""
    if calibrated_ms is None or current_ms is None:
        return False
    return int(current_ms) == int(calibrated_ms)


# ---------------------------------------------------------------------------
# Item 7 -- NDI mapping (#399) correct.
# ---------------------------------------------------------------------------


def ndi_mapping_ok(mismatches: list) -> bool:
    """mismatches: [(input, actual_sender, wanted_sender), ...] | None from set-ndi-mapping.py
    --verify-only. PASS iff the list is empty. A None value (the facet could not be gathered)
    fails CLOSED, never raises on `len()` (#1225)."""
    if mismatches is None:
        return False
    return len(mismatches) == 0


# ---------------------------------------------------------------------------
# Item 8 -- test artifacts cleared (pidfiles, painter CSVs, tmp heartbeats).
# ---------------------------------------------------------------------------


def artifacts_cleared_ok(existing_paths: list) -> bool:
    """existing_paths: paths that STILL EXIST after cleanup, or None if unreadable. PASS iff the
    list is empty. A None value fails CLOSED, never raises on `len()` (#1225)."""
    if existing_paths is None:
        return False
    return len(existing_paths) == 0


# ---------------------------------------------------------------------------
# Aggregation.
# ---------------------------------------------------------------------------


def aggregate(item_results: dict):
    """item_results: {item_name: bool}. Returns (overall_pass, failed_items) where failed_items
    is in the FIXED ITEM_ORDER (never dict/insertion order). A MISSING item (not present in
    item_results at all) counts as FAILED -- never silently treated as passed."""
    failed = [name for name in ITEM_ORDER if not item_results.get(name, False)]
    return (len(failed) == 0, failed)


def format_summary_sk(overall_pass: bool, item_results: dict, details: dict = None) -> str:
    """A Slovak, phone/terminal-readable multi-line summary: header + one line per item with an
    OK/CHYBA mark, every item named regardless of outcome. `details` (optional):
    {item_name: extra_context_string} appended in parens to that item's line."""
    details = details or {}
    lines = []
    if overall_pass:
        lines.append("EVENT mod POTVRDENY -- rig je cisty pre zivy prenos:")
    else:
        lines.append("EVENT mod NEPRESIEL -- rig NIE JE cisty pre zivy prenos:")
    for name in ITEM_ORDER:
        ok = item_results.get(name, False)
        mark = "OK" if ok else "CHYBA"
        extra = f" ({details[name]})" if name in details else ""
        lines.append(f"  [{mark}] {ITEM_LABELS_SK[name]}{extra}")
    return "\n".join(lines)


def format_discord_message_sk(overall_pass: bool, item_results: dict, details: dict = None,
                               timestamp: str = "") -> str:
    """#724 -- the EVENT-mode Discord confirmation message: sent to the owner's Discord thread
    after EVERY assert-phase run (pass AND fail), so the user never again has to trust a bare
    terminal claim about the rig being broadcast-clean. PASS -> "EVENT mod POTVRDENY" (a
    confirmation); FAIL -> "EVENT mod NEPRESIEL" (a WARNING naming every failing item with a
    CHYBA mark). Reuses format_summary_sk's per-item lines -- one source of truth for what
    "clean" means, never a second, divergent description of the same 8 items. Always
    comfortably under Discord's 2000-char single-message hard cap (a short fixed 8-item
    checklist, even with details populated for every item)."""
    header_emoji = "✅" if overall_pass else "⚠️"  # checkmark / warning
    summary = format_summary_sk(overall_pass, item_results, details)
    lines = [f"{header_emoji} {summary}"]
    if timestamp:
        lines.append(f"cas: {timestamp}")
    return "\n".join(lines)


def compute_item_results(facts: dict) -> dict:
    """Run every item's decision function against an already-gathered facts dict (see the
    module docstring for the facts shape). Returns {item_name: bool}."""
    return {
        "paint_processes": paint_processes_ok(facts.get("fleet_paint_process_counts", {})),
        "pixel_proof": pixel_proof_ok(facts.get("qr_findings", {})),
        "burns_off": burns_off_ok(facts.get("burn_states", {})),
        "no_recordings": no_recordings_ok(facts.get("recording_states", {})),
        "services_healthy": services_healthy_ok(
            facts.get("fleet_service_active", {}), facts.get("fleet_stray_units", {})
        ),
        "latency_calibrated": latency_calibrated_ok(
            facts.get("latency_current_ms"), facts.get("latency_calibrated_ms")
        ),
        "ndi_mapping": ndi_mapping_ok(facts.get("ndi_mismatches", [])),
        "artifacts_cleared": artifacts_cleared_ok(facts.get("artifacts_existing", [])),
    }


def main(argv=None):
    import datetime

    ap = argparse.ArgumentParser(description="#722 EVENT-mode CONTRACT decision + aggregation")
    ap.add_argument("--facts", required=True, help="path to the gathered-facts JSON")
    ap.add_argument("--result-out", default="", help="optional: write the result JSON here")
    ap.add_argument(
        "--discord-out", default="",
        help="optional: write the #724 Discord confirmation message here (plain text)",
    )
    a = ap.parse_args(argv)

    with open(a.facts, encoding="utf-8") as f:
        facts = json.load(f)

    item_results = compute_item_results(facts)
    overall_pass, failed = aggregate(item_results)
    details = dict(facts.get("details") or {})
    # #1225: name WHICH scene was unreadable (or found a live QR) in the summary, rather than
    # just marking pixel_proof CHYBA with no further explanation -- an operator seeing "facet
    # unreadable: Cam 2" can tell "we couldn't check" apart from "a QR is actually on air".
    qr_detail = pixel_proof_detail(facts.get("qr_findings") or {})
    if qr_detail:
        details.setdefault("pixel_proof", qr_detail)
    summary = format_summary_sk(overall_pass, item_results, details)
    print(summary)

    timestamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    discord_message = format_discord_message_sk(overall_pass, item_results, details, timestamp)

    result = {
        "overall_pass": overall_pass,
        "item_results": item_results,
        "failed_items": failed,
        "summary_sk": summary,
        "discord_message_sk": discord_message,
    }
    if a.result_out:
        with open(a.result_out, "w", encoding="utf-8") as f:
            json.dump(result, f, indent=2)
    if a.discord_out:
        with open(a.discord_out, "w", encoding="utf-8") as f:
            f.write(discord_message)

    sys.exit(0 if overall_pass else 1)


if __name__ == "__main__":
    main()
