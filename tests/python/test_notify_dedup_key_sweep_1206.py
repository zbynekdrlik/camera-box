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


# --------------------------------------------------------------------------------------------------
# #1307/#1308 -- PRODUCTION-CRITICAL class: the ONE sanctioned exception to invariant "no per-pass
# component".
#
# Owner ruling (ROZHODNUTÉ on #1307, 2026-09-13, verbatim): „aj ntp aj ostatne veci bez ktorych nevie
# produkcia bezat spravne musi notifikovat ... nech kazdu minutu chodia notifikacie ze nemaju dante
# clock ... byt o tom dokolecka notifikovany". A production-critical condition (a lost dante clock, a
# wedged OBS, a dead bundle-state server, a missing VB-Matrix, an unreachable box ...) whose loss is
# INVISIBLE without a page must be RE-pinged while it PERSISTS -- reversing rule 1's one-ping-per-
# incident default for THIS class only. The mechanism is a TIME-BUCKETED --dedup-key
# (<incident-key>-<floor(now/interval)>) built by the ONE shared helper (watchdog_notify_key /
# scripts/watchdog_reping.py, #1308): within a bucket it still card-edits (no flood), each new bucket
# re-pings. See .claude/rules/watchdog-notify-dedup.md "Production-critical class".
#
# This allowlist is CLASS-based (#1308): it names EVERY production-critical watchdog whose keys may
# carry the time bucket. It makes the exception EXPLICIT -- these files intentionally rotate the key,
# so a future stricter "no timestamp/per-pass component in the key" hardening must exempt them BY NAME
# -- never silently. It does NOT weaken invariant A (a --dedup-key is still present) or B (no recovery
# ping) for these files or any other watchdog; the default stable-key rule stands for everything else,
# and the sweep below REJECTS a bucketed key in any NON-allowlisted script.
_PRODUCTION_CRITICAL_TIME_BUCKETED = {
    "dantesync-clock-alert-watchdog.sh",   # #1307 -- dev1 dante-clock loss / DNS / GM-move paging
    "genlock-lock-alert-watchdog.sh",      # #1299 -- fleet genlock LOCK facet
    "network-reach-alert-watchdog.sh",     # #1001 -- strih/stream unreachable
    "bundle-state-alert-watchdog.sh",      # #732  -- :8899 bundle-state server down
    "obs-liveness-watchdog.sh",            # #391  -- broadcast-OBS render wedge
    "audio-lag-alert-watchdog.sh",         # #1226 -- OBS audio-timeline lag / band drift
    "asio-starve-alert-watchdog.sh",       # #1023 -- ASIO source starved
    "vb-matrix-alert-watchdog.sh",         # #1227 -- VB-Matrix down
    "ndi-portmap-alert-watchdog.sh",       # #1181 -- NDI sender port-map moved
    "avsync-heartbeat-alert-watchdog.sh",  # #812  -- A/V-sync heartbeat stale
    "imag-obs-alert-watchdog.sh",          # #882  -- imag OBS down / latency-drift / restart-storm
}

# The bucketing markers an inline --dedup-key carries when it time-buckets: the shared bash helper
# call, its python-CLI form, or a raw timestamp. Any of these in the --dedup-key VALUE means the key
# rotates by time -- allowed ONLY for the allowlisted class above. Scanned against the KEY SEGMENT
# (everything from `--dedup-key` to end of the logical line), NOT the whole line, so a legitimate
# `$(date ...)` in a notify BODY (a timestamp in the message text) never false-positives (#1308 review).
_BUCKET_MARKERS = ("watchdog_notify_key", "watchdog_reping", "dedup-key --now", "$(date")


def _dedup_key_segment(ln):
    """The `--dedup-key ...` portion of a notify logical line (bash `--dedup-key` or py `"--dedup-key"`),
    or "" if the line has no dedup-key. Isolates the KEY from the BODY so a marker scan only judges the
    key's value, never the message text."""
    for tok in ("--dedup-key", '"--dedup-key"'):
        i = ln.find(tok)
        if i != -1:
            return ln[i:]
    return ""


def test_production_critical_watchdogs_are_swept_and_carry_a_key():
    """Every production-critical watchdog is still discovered by the sweep (so invariants A + B apply
    to it too) -- the time-bucket exception is about the key's VALUE rotating, never about escaping the
    presence + no-recovery-ping invariants."""
    names = {p.name for p in _iter_notify_scripts()}
    for allowed in _PRODUCTION_CRITICAL_TIME_BUCKETED:
        assert allowed in names, (
            f"#1308 production-critical allowlist names '{allowed}' but the sweep did not discover it "
            "(it must still emit a `notify --body ... --dedup-key` to be covered)"
        )


def test_only_allowlisted_watchdogs_time_bucket_their_key():
    """The class exception is NARROW: only an allowlisted production-critical watchdog may rotate its
    --dedup-key by time. A NON-allowlisted script that inlines a bucketing marker into a notify key is
    a regression of rule 1 ("no per-pass/timestamp component") -- fail loudly, naming the file, so the
    exception can never spread silently to a diagnostic/TEST-mode watchdog."""
    offenders = []
    for p in _iter_notify_scripts():
        if p.name in _PRODUCTION_CRITICAL_TIME_BUCKETED:
            continue
        for ln in _notify_body_logical_lines(p):
            seg = _dedup_key_segment(ln)
            for marker in _BUCKET_MARKERS:
                if marker in seg:
                    offenders.append(f"{p.relative_to(_ROOT)}: [{marker}] {ln.strip()[:140]}")
    assert not offenders, (
        "#1308: these NON-production-critical notify call-sites time-bucket their --dedup-key "
        "(a per-pass/timestamp component -- only the allowlisted class may; see "
        ".claude/rules/watchdog-notify-dedup.md):\n" + "\n".join(offenders)
    )


def test_production_critical_watchdogs_actually_bucket_their_inline_key():
    """The 10 bash watchdogs that wrap an INLINE --dedup-key (all but dante-clock, which buckets via a
    variable from bucketed_key) must actually carry the shared helper on their notify line -- proof
    the #1308 rollout landed, not just that the allowlist names them. dante-clock is exempt here (its
    key is built earlier into $key, off the notify line) but is pinned behaviorally below."""
    names_to_paths = {p.name: p for p in _iter_notify_scripts()}
    inline_bucketers = _PRODUCTION_CRITICAL_TIME_BUCKETED - {"dantesync-clock-alert-watchdog.sh"}
    missing = []
    for name in sorted(inline_bucketers):
        p = names_to_paths.get(name)
        assert p is not None, f"#1308: {name} not discovered by the sweep"
        if not any("watchdog_notify_key" in ln for ln in _notify_body_logical_lines(p)):
            missing.append(name)
    assert not missing, (
        "#1308: these production-critical watchdogs no longer time-bucket their inline --dedup-key "
        "(the shared watchdog_notify_key wrap was dropped -- re-pinging is silently broken):\n"
        + "\n".join(missing)
    )


def test_production_critical_key_is_intentionally_time_bucketed():
    """Pin the sanctioned exception behaviorally: the shared re-ping helper (via dante-clock's pure
    decision module, which delegates to scripts/watchdog_reping.py) rotates the --dedup-key by time
    bucket (a fresh ping per interval while the fault persists), the DELIBERATE reversal of rule 1 for
    this class (owner ruling #1307/#1308). If this ever stops rotating, the production-critical
    re-ping is silently broken -- fail loudly here."""
    import importlib.util
    mod_path = _ROOT / "scripts" / "dantesync_clock_decision.py"
    spec = importlib.util.spec_from_file_location("dantesync_clock_decision", mod_path)
    dc = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(dc)
    base = "dante-clock-cam1"
    within = dc.dedup_key(base, 600000, 600)
    same_bucket = dc.dedup_key(base, 600599, 600)
    next_bucket = dc.dedup_key(base, 600600, 600)
    assert within == same_bucket, "same bucket must card-edit (no flood), not re-ping"
    assert within != next_bucket, "a new bucket MUST produce a fresh key (re-ping while it persists)"
