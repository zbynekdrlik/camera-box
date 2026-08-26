"""#1206 — Discord phone-flood fix: every alert-watchdog `notify --body` call must carry a stable
`--dedup-key`, and no `notify --body` may deliver a RECOVERY/status (✅) message to the phone.

Root cause (airuleset #704/#705): camera-box's ~22 alert-watchdog scripts each call
`python3 "$NOTIFY" notify --body ...` with NO `--dedup-key`, so airuleset's own auto-dedup can only
collapse them into ~5-min windows — one stuck state re-pings the owner ~288×/day (this repo was 76%
of all delivered fleet pings). The fix is purely in the DELIVERY layer:

  * ALERT class  (🚨 active incident, ⚠️ degraded/tap-blind, 🛟/🧹 one-shot auto-action) →
    a STABLE `--dedup-key` so a repeated IDENTICAL state edits the existing card instead of
    re-pinging (airuleset holds the card within its 14-day marker TTL).
  * RECOVERY / STATUS class (the ✅ "back to normal / serving again / OK again" latch pings) →
    NOT a phone ping at all; routed to the machine channel (the `log "RECOVERY: ..."` journal line
    stays, the `notify` call is removed). Doctrine: analyze-not-ping (airuleset #704/#693).

These are Tier-0 static invariants (no cargo, no rig, no OBS) — they read the swept scripts' text
and assert the two rules above. The confirm/throttle/recovery DECISION logic
(scripts/lib/obs-watchdog-decision.sh + per-script `*_recovery_decision`) is out of scope and
UNCHANGED — only the notify delivery layer is swept.
"""
import pathlib
import re

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"


def _iter_notify_scripts():
    """Every file under scripts/ that emits at least one `notify --body` (bash) or a
    `subprocess.run([... "notify", ... "--body" ...])` (rig-status.py)."""
    found = []
    for p in sorted(_SCRIPTS.rglob("*")):
        if not p.is_file():
            continue
        if p.suffix not in (".sh", ".py"):
            continue
        # Discover on the JOINED logical lines (not raw text) so a call-site written across a
        # line-continuation — `"$NOTIFY" notify \` ⏎ `--body ...`, or with an arg between them —
        # is still found and subjected to the invariants below, never a silent false green (#1206).
        if _notify_body_logical_lines(p):
            found.append(p)
    return found


def _join_bash_continuations(text):
    """Collapse trailing-backslash line continuations into single logical lines, so a multi-line
    `python3 "$NOTIFY" notify --body \\ "msg" \\ --dedup-key "k" \\ >/dev/null ...` becomes ONE
    logical line the invariants can inspect as a unit."""
    logical = []
    buf = ""
    for raw in text.splitlines():
        stripped = raw.rstrip()
        if stripped.endswith("\\"):
            buf += stripped[:-1] + " "
        else:
            buf += stripped
            logical.append(buf)
            buf = ""
    if buf:
        logical.append(buf)
    return logical


def _notify_body_logical_lines(path):
    """Return the joined logical lines that are an actual `notify --body` invocation.

    For a .py file the emit is a subprocess list (`"notify", "--body", body`), not the literal
    `notify --body` substring — represent each such call by the whole `subprocess.run([...])` text
    so the same assertions apply."""
    txt = path.read_text(encoding="utf-8", errors="replace")
    if path.suffix == ".py":
        calls = []
        for m in re.finditer(r"subprocess\.run\(\[(.*?)\]", txt, flags=re.DOTALL):
            seg = m.group(1)
            if '"notify"' in seg and '"--body"' in seg:
                calls.append(" ".join(seg.split()))
        return calls
    # Match `notify ... --body` on the JOINED logical line (regex, not a contiguous-substring
    # check) so a future call-site that splits `notify \` from `--body`, or inserts an arg
    # between them, is still caught by both invariants below (#1206 review hardening).
    return [ln for ln in _join_bash_continuations(txt) if re.search(r"notify\b.*--body", ln)]


def test_sweep_covers_the_known_alert_watchdogs():
    """Guard against a broken discovery glob making the invariants vacuous."""
    names = {p.name for p in _iter_notify_scripts()}
    expected = {
        "obs-liveness-watchdog.sh", "network-reach-alert-watchdog.sh",
        "ndi-portmap-alert-watchdog.sh", "splitter-port-alert-watchdog.sh",
        "rig-status.py", "obs-burn-reconcile-watchdog.sh", "cam-disk-guard.sh",
    }
    missing = expected - names
    assert not missing, f"#1206 sweep did not discover expected scripts: {missing}"
    assert len(names) >= 20, f"#1206 expected ~22 notify-emitting scripts, found {len(names)}: {sorted(names)}"


def test_every_notify_body_call_carries_a_dedup_key():
    """Invariant A: every surviving `notify --body` emit in a swept script must include
    `--dedup-key` — otherwise airuleset can only 5-min-window-dedup it and one stuck state floods
    the owner's phone (#1206)."""
    offenders = []
    for p in _iter_notify_scripts():
        for ln in _notify_body_logical_lines(p):
            has_key = ("--dedup-key" in ln) if p.suffix == ".sh" else ('"--dedup-key"' in ln)
            if not has_key:
                offenders.append(f"{p.relative_to(_ROOT)}: {ln.strip()[:140]}")
    assert not offenders, (
        "#1206: these `notify --body` call-sites are missing a stable --dedup-key "
        "(keyless notify = the 76%-of-fleet phone flood):\n" + "\n".join(offenders)
    )


def test_no_recovery_or_status_message_is_phone_pinged():
    """Invariant B: a RECOVERY/status (✅) message must never reach the phone — it belongs in the
    machine channel (the `log "RECOVERY: ..."` journal line). No `notify --body` emit may carry a
    ✅ body (#1206 point 3, analyze-not-ping)."""
    offenders = []
    for p in _iter_notify_scripts():
        for ln in _notify_body_logical_lines(p):
            if "✅" in ln:  # ✅
                offenders.append(f"{p.relative_to(_ROOT)}: {ln.strip()[:140]}")
    assert not offenders, (
        "#1206: these `notify --body` call-sites still phone-ping a ✅ recovery/status message "
        "(must be machine-channel/log only):\n" + "\n".join(offenders)
    )
