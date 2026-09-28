#!/usr/bin/env python3
"""#399 — enforce the strih OBS NDI-input->camera mapping (OBS-WS harness).

The strih NDI-input→camera-box bindings drift from the pins (the recurring bug: two inputs both on
CAM4, so a camera shows twice and another is missing). A pure hot WS rebind does NOT survive a
force-kill OBS relaunch (a distroav.dll swap reverts to the stale saved scene). So rig activation
(scripts/rig-mode.sh) must ENFORCE the correct distinct mapping every time — set it + verify every
input is bound to a DISTINCT camera — instead of the operator/agent re-doing it by hand.

**#753 PIVOT (2026-07-14, binding user directive) — the mapping is now 1:1, the pre-2026-07-14
INVERTED table below is HISTORY.** The user: "chcem aby uz bolo ze cam 1 je cam1 ndi source, nie
pomenene" (cam 1 IS the cam1 NDI source, not relabeled). `NDI cam<N>` now carries `CAM<N> (usb)`
for every N — no more offset. Each camera's individually-tuned `genlock_latency_ms_src` MOVED WITH
the physical camera during the live rebind (CAM4=20ms, CAM5=8ms, CAM6=13ms, every other camera=3ms
— unchanged VALUES, just re-attached to the NEW input that now carries that camera), verified live
on strih 2026-07-14.

**#757 RE-BASELINE (2026-07-15) — current per-camera `genlock_latency_ms_src` pins:** cam1=3,
cam2=14, cam3=18, cam4=50, cam5=8, cam6=43, cam7=36 — equalized to a uniform ~71 ms delivery p50
from run 1984131963's measured per-camera delivery table (eff-A/V spread correlated 1:1 with the
p50 spread; see #757 for the math). Applied live + read back on all 28 inputs: strih
`NDI cam1..7` + `MV NDI cam1..7`, imag `NDI CAM1..7` + `MV CAM1..7` (MV clones carry the identical
latency — parity rule). A deliberate future latency rollout re-derives these from a fresh fused
run's delivery table, never by hand-tuning a single camera in isolation.

**#827 RETIREMENT (2026-07-27, binding owner directive) — cam5/cam6/cam7 removed from the
ACTIVE mapping, but REVERSIBLY.** The test rig shrank: cam5/cam6/cam7's USB grabber cards were
returned to their owner and those boxes are powered off. The owner's binding requirement: this
retirement MUST be a one-line reversal when the boxes come back — so `FULL_MAP` below keeps
EVERY camera's pin as a FACT (never deleted), and `--active` (defaulting to the `CAMERA_ACTIVE_SET`
env var camera-set.sh exports, or "cam3" if that's unset too -- issue 1170: cam2's
camera-under-test role retired [grabber cure-decay], cam1 retired earlier) filters it down to
the pins actually ENFORCED this run. Re-enable procedure: cam5 back? add "cam5" to
CAMERA_ACTIVE_SET in scripts/camera-set.sh (scripts/rig-mode.sh passes it through automatically
via `--active "$CAMERA_ACTIVE_SET"`), rerun the gate — nothing here needs to change. Whatever OBS
scenes for a retired input remain configured on strih are simply no longer enforced while
inactive — they carry no live camera feed anyway.

**#898 RETIREMENT (2026-07-31) — cam3 ALSO removed from the ACTIVE mapping, same mechanism.**
cam3's grabber card was physically destroyed (moved into cam1 during the #728/#688 power-supply
recovery), leaving cam3 with zero capture hardware. Retired via the exact same reversible
`CAMERA_ACTIVE_SET` membership mechanism as #827 above — `FULL_MAP`'s "NDI cam3"→"CAM3 (usb)"
pin stays a FACT, never deleted; DEFAULT_ACTIVE_SET's fallback literal moved from
"cam1 cam2 cam3 cam4" to "cam1 cam2 cam4". Re-enable procedure: once a replacement grabber card
is fitted, add "cam3" back to CAMERA_ACTIVE_SET — nothing here needs to change.

Pre-2026-07-14 HISTORY (superseded, kept for context only — do NOT use): the mapping used to be
OFFSET by one slot for the six original cameras (NDI cam5→CAM1, NDI cam1→CAM3, NDI cam3→CAM4,
NDI cam4→CAM5, NDI cam6→CAM6, NDI cam2→CAM2 — cam2 was ALREADY 1:1 even then, coincidentally).
#312 (fleet growth 4→6, #451) added two of those offset pins: "NDI cam4" USED TO duplicate CAM4's
own feed (the exact drift bug this module exists to catch) — repointed to the then-unwired CAM5
physical box instead; "NDI cam6" was already correctly bound live on strih to CAM6 but had no
canonical pin, so it could silently drift on the next OBS relaunch — now it is enforced like every
other input. #753 (fleet growth 6→7, 2026-07-14) initially added "NDI cam7"→CAM7 as a NEW direct
pin (cam7 never had a legacy offset slot to inherit) — and the SAME session then retired the offset
for cam1/cam3/cam4/cam5/cam6 too, per the binding directive above.

Exit codes:
  0  PASS  — every input set to its pin AND all senders distinct
  1  FAIL  — could not set an input, or a duplicate binding remains
  2  ERROR — OBS WS connection / request failure

Usage:
  python3 scripts/set-ndi-mapping.py --host 10.77.9.202 [--password PW]
  python3 scripts/set-ndi-mapping.py --host 10.77.9.202 --verify-only   # check, do not set
  python3 scripts/set-ndi-mapping.py --map "NDI cam1=CAM1 (usb)" ...     # override the pins
  python3 scripts/set-ndi-mapping.py --active "cam1 cam2 cam4 cam5" ...  # reactivate cam5
"""
import argparse
import base64
import hashlib
import json
import os
import sys
import time

PORT = 4455

# #399/#312/#753 — the FULL strih NDI mapping FACT table (Claude-owned; never a user question) --
# every camera the fleet has ever wired, REGARDLESS of which are currently active (#827: a fact
# lookup, never deleted on retirement). #753 PIVOT (2026-07-14, binding user directive): 1:1 --
# NDI cam<N> -> CAM<N> (usb), for every N. The pre-2026-07-14 offset/inverted table is HISTORY
# (see the module docstring above); do not reintroduce it.
FULL_MAP = [
    ("NDI cam1", "CAM1 (usb)"),
    ("NDI cam2", "CAM2 (usb)"),
    ("NDI cam3", "CAM3 (usb)"),
    ("NDI cam4", "CAM4 (usb)"),
    ("NDI cam5", "CAM5 (usb)"),
    ("NDI cam6", "CAM6 (usb)"),
    ("NDI cam7", "CAM7 (usb)"),
]

# DEFAULT_MAP kept as an alias to FULL_MAP for anything that still imports it directly (e.g. a
# caller that wants every known pin regardless of activity) -- active_map() below is the ONE
# place "which pins are ENFORCED today" is decided.
DEFAULT_MAP = FULL_MAP

# #827: the ACTIVE camera set default -- mirrors scripts/camera-set.sh's CAMERA_ACTIVE_SET
# exactly (this module is invoked as a standalone subprocess, so it reads the SAME env var rather
# than re-declaring its own separate default; when unset, falls back to the identical literal
# camera-set.sh itself defaults to, so the two can never silently disagree). issue 1216
# (2026-08-28): bigger splitter fitted, cam5/cam6/cam7 back in. issue 1217 (same day): cam5 OUT
# again -- a DEAD_PORT leg on the new splitter (flat static frame, siblings cam6/cam7 read
# colour). issue 1216 completion (2026-08-30, owner directive "kamery od 1-7 bezia" after a
# physical cable reseat): cam4 (#947) and cam5 (DEAD_PORT) BOTH rejoin -- the full
# seven-camera fleet is active, for the first time simultaneously.
DEFAULT_ACTIVE_SET = os.environ.get("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4 cam5 cam6 cam7")


def _camera_name_of(ndi_input):
    """'NDI cam3' -> 'cam3' -- the camera name a strih NDI-input label carries (#753: literal
    1:1, input 'NDI cam<N>' always names camera 'cam<N>')."""
    prefix = "NDI "
    return ndi_input[len(prefix):] if ndi_input.startswith(prefix) else ndi_input


def active_map(active_set=None):
    """FULL_MAP filtered down to only the cameras named in `active_set` (space/comma-separated
    string, or an iterable of names). Defaults to DEFAULT_ACTIVE_SET. This is THE single place
    "which pins are enforced today" is decided -- #827's whole point: change what's active by
    changing the SET passed in, never by editing FULL_MAP."""
    if active_set is None:
        active_set = DEFAULT_ACTIVE_SET
    if isinstance(active_set, str):
        names = set(active_set.replace(",", " ").split())
    else:
        names = set(active_set)
    return [(inp, snd) for inp, snd in FULL_MAP if _camera_name_of(inp) in names]


def baseline_sender_for(input_name):
    """#1158: the CANONICAL #399 baseline NDI sender for a strih input label (e.g. 'NDI cam1' ->
    'CAM1 (usb)'), or None if the input is not in the FULL_MAP fact table. This is the SINGLE source
    of truth the #1158 recovery paths (strih_mv_scenes.reattach() + obs_phase2.reenforce_ndi_name)
    re-enforce -- never a stale/drifted saved-scene name and never a hardcoded 'CAM{N} (usb)'
    duplicate that could drift from FULL_MAP."""
    for inp, snd in FULL_MAP:
        if inp == input_name:
            return snd
    return None


# websocket-client is imported LAZILY (inside the WS helpers), not at module top: the pure helpers
# (parse_map_args / duplicates / active_map) must be importable + unit-testable WITHOUT the WS
# dependency (a top-level import here made harness_rig_ndi_mapping.rs fail on a CI runner that has no
# websocket-client). Only the actual OBS-WS connect needs it.
def _ws():
    try:
        from websocket import WebSocketTimeoutException, create_connection
        return WebSocketTimeoutException, create_connection
    except ImportError:
        sys.exit("missing dep: pip install websocket-client")


# ─── pure helpers (unit-testable without OBS) ────────────────────────────────

def parse_map_args(items, active_set=None):
    """Parse repeated `--map "INPUT=SENDER"` into [(input, sender), ...]; an explicit --map
    ALWAYS wins outright (an operator override is never filtered by activity). With no --map,
    fall back to active_map(active_set) -- #827: only the currently-ACTIVE cameras' pins, never
    the full historical fact table unfiltered."""
    if not items:
        return active_map(active_set)
    out = []
    for it in items:
        if "=" not in it:
            raise ValueError(f"--map must be INPUT=SENDER, got {it!r}")
        k, v = it.split("=", 1)
        out.append((k.strip(), v.strip()))
    return out


def duplicates(bindings):
    """Given {input: sender}, return {sender: [inputs...]} for any sender bound to >1 input."""
    by_sender = {}
    for inp, snd in bindings.items():
        by_sender.setdefault(snd, []).append(inp)
    return {s: v for s, v in by_sender.items() if len(v) > 1}


# ─── OBS WebSocket helpers (same _conn/_rpc pattern as obs_burn_filter.py) ────

def _conn(host, password=""):
    _, create_connection = _ws()
    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())
    ident = {"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())
    return ws


def _rpc(ws, rtype, rdata=None):
    ws_timeout_exc, _ = _ws()
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if time.monotonic() - t0 >= 30:
            raise TimeoutError(f"obs-websocket request {rtype!r} timed out")
        try:
            m = json.loads(ws.recv())
        except ws_timeout_exc:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"]:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def _get_binding(ws, inp):
    return _rpc(ws, "GetInputSettings", {"inputName": inp}) \
        .get("inputSettings", {}).get("ndi_source_name", "")


def heal_active_mapping(op, ws, want, get_binding, log_err):
    """#1158 self-heal: re-enforce ONLY the DRIFTED-or-EMPTY inputs among `want`
    [(input, baseline), ...] -- discoverability-gated + read-back-verified via
    op.reenforce_ndi_name. Correct inputs are left UNTOUCHED (so this never fights a healthy
    mapping), and an empty/drifted input whose baseline sender is OFFLINE is left as-is + logged
    LOUD (a real rig degradation, never a silent mangle-attempt). Pure/dependency-injected (op, ws,
    get_binding, log_err) so it is fully unit-testable with fakes, no live OBS. Heals by
    DIFFERS-FROM-BASELINE, never empty-only: a #795 mangle yields a drifted NON-empty name that
    #1096 can never rebind either, so the empty-only criterion would miss it. Returns
    (healed, offline, failed, skipped)."""
    healed = offline = failed = skipped = 0
    for inp, snd in want:
        cur = get_binding(ws, inp)
        if cur == snd:
            skipped += 1
            continue
        status = op.reenforce_ndi_name(ws, inp, snd)
        if status == op.REENFORCE_HEALED:
            log_err(f"#1158 auto-revive: '{inp}' ndi_source_name {cur!r} -> {snd!r} "
                    f"(re-enforced #399 baseline, read-back verified)")
            healed += 1
        elif status == op.REENFORCE_OFFLINE:
            log_err(f"#1158 auto-revive: '{inp}' is {cur!r} (drifted/empty) but baseline {snd!r} is "
                    f"OFFLINE (absent from the DistroAV finder) -- left as-is; a real rig degradation")
            offline += 1
        else:  # REENFORCE_VERIFY_FAILED
            log_err(f"#1158 auto-revive: '{inp}' set to baseline {snd!r} but read-back MISMATCHED "
                    f"(possible #795 mangle) -- treat as unhealed")
            failed += 1
    return healed, offline, failed, skipped


def _heal_exit_code(healed, offline, failed):
    """#1158 --heal exit contract (pure, testable): 1 iff a read-back verify FAILED (loud, do not
    trust); 0 iff >=1 input HEALED (the caller re-samples a revived leg); 3 otherwise (nothing was
    drifted, or every drifted input's baseline is offline -> no heal possible, the caller proceeds
    to its own fail-loud path, the #1158 log lines already surfaced why)."""
    if failed > 0:
        return 1
    if healed > 0:
        return 0
    return 3


# ─── #1197: bounded COLD-finder discovery-wait heal ──────────────────────────
# WHY (issue 1197): right after a strih OBS BOOT or the #1093 escalation force-kill restart, the
# fresh DistroAV finder is COLD — a genuinely-live sender is simply not-yet-discovered. The #1114
# reattach CLEAR-then-SET then EMPTIES a correct ndi_source_name and (mangle-protection) refuses to
# re-apply it, leaving "" — a stopped-receiver PERMANENT wedge (#1158) — and nothing WAITS for the
# finder to warm up and re-enforce the #399 baseline. `--heal` fires once and gives up; this rides
# out the cold finder with a bounded wall-clock poll, re-enforcing each baseline the instant it is
# discoverable (never blind-setting one that is absent — the #795 mangle ban).

def _discover_reenforce_once(op, ws, inp, baseline, get_binding):
    """One discovery+re-enforce probe for a single input. Returns:
      "waiting" — `baseline` is NOT in the DistroAV finder yet (cold finder) → keep waiting; never set
                  (setting a name absent from the finder MANGLES it, #795).
      "done"    — `baseline` is discoverable AND the input is bound to it (already correct, or just
                  re-enforced + read-back verified).
      "failed"  — set, but read-back MISMATCHED (a #795 mangle / RPC failure) → loud, do not trust.
    Uses the SHARED obs_phase2.reenforce_ndi_name policy for the set+verify (never a second path)."""
    if baseline not in op._ndi_source_list(ws, inp):
        return "waiting"
    if get_binding(ws, inp) == baseline:
        return "done"  # discoverable + already correct — never fight a healthy mapping
    status = op.reenforce_ndi_name(ws, inp, baseline)
    if status == op.REENFORCE_HEALED:
        return "done"
    if status == op.REENFORCE_VERIFY_FAILED:
        return "failed"
    return "waiting"  # OFFLINE — vanished between the finder read and the set; retry next iteration


def heal_wait_active_mapping(op, ws, want, get_binding, log_err, deadline_s,
                             interval_s=4.0, now=time.monotonic, sleep=time.sleep):
    """#1197: bounded-wall-clock discovery-wait heal over `want` [(input, baseline), ...]. Polls the
    DistroAV finder for each baseline to become discoverable, then re-enforces it via
    _discover_reenforce_once. RETURNS EARLY the instant every input is discoverable+bound (a warm
    finder pays ~one probe per input, no sleep), so only a genuinely cold finder spends real time —
    always bounded by `deadline_s` WALL CLOCK (not accumulated sleeps: each iteration also spends the
    per-input probes, so a sleep-counter would overrun the documented window, the #1114 review 🔵-2
    discipline). A done input is never re-probed; a failed input is terminal (loud). Pure/dependency-
    injected (op, ws, get_binding, log_err, now, sleep) so it is Tier-0 pytest-able with fakes and
    zero real sleep. Returns (done, waiting, failed)."""
    pending = list(want)   # [(input, baseline), ...] not yet resolved
    done = failed = 0
    start = now()
    deadline = start + deadline_s
    while True:
        still = []
        for inp, snd in pending:
            r = _discover_reenforce_once(op, ws, inp, snd, get_binding)
            if r == "done":
                log_err(f"#1197 finder-warm: {inp!r} baseline {snd!r} discoverable + bound "
                        f"(+{(now() - start):.0f}s)")
                done += 1
            elif r == "failed":
                log_err(f"#1197 finder-warm: {inp!r} baseline {snd!r} set but read-back MISMATCHED "
                        f"(possible #795 mangle) — left as-is")
                failed += 1
            else:  # waiting
                still.append((inp, snd))
        pending = still
        if not pending or now() >= deadline:
            break
        sleep(interval_s)
    for inp, snd in pending:
        log_err(f"#1197 finder-warm: {inp!r} baseline {snd!r} STILL absent from the DistroAV finder "
                f"after {deadline_s:.0f}s (sender offline? still-cold finder?) — left as-is, a real "
                f"rig degradation")
    return done, len(pending), failed


def _heal_wait_exit_code(done, waiting, failed):
    """#1197 --heal-wait exit contract (pure, testable): 1 iff a read-back verify FAILED (loud, do
    not trust); 3 iff any input never became discoverable within the bound (the caller logs loud +
    proceeds — the pixel re-verify / the next camera's own reverify is the real gate); 0 iff every
    targeted input ended discoverable + bound."""
    if failed > 0:
        return 1
    if waiting > 0:
        return 3
    return 0


# ─── #1180: RECEIVER-liveness verify (the LIVENESS term the name-only verify misses) ─────────
# WHY (2026-08-27 strih NIC-swap aftermath): --heal / reenforce_ndi_name verify only that the
# ndi_source_name STRING is right. A receiver can hold a FROZEN frame with the CORRECT name (an
# issue-1158 wedged receiver thread) -- --heal then reports "nothing healable" (the name never
# drifted) and the input stays dead, indistinguishable from a live camera that is momentarily
# still. This mode samples frame-delivery liveness over WS (obs_phase2.sample_receiver_liveness, a
# screenshot-diff) and FAILS LOUD (exit 1) on a FROZEN input so the caller escalates to an OBS
# restart -- the only cure for the wedge. It runs over the active inputs REGARDLESS of name drift
# (the whole point: the frozen cam1 had a correct name). The C++ #1180 fix owns the separate
# wrong-source IDENTITY half; this owns the correct-name-no-frames LIVENESS half.

def verify_live_mapping(op, ws, want, sampler, log_err):
    """#1180 liveness verify over `want` [(input, baseline), ...]: sample each input's receiver
    frame-delivery liveness via `sampler(ws, input) -> (state, reason)` and count LIVE / FROZEN /
    INCONCLUSIVE. A FROZEN input is the wedge a name-only verify misses -- logged LOUD, pointing the
    caller at the real cure (an OBS restart), because the name may be correct all along. An
    INCONCLUSIVE input (could not sample enough frames) is left as-is + logged, never torn down on a
    can't-confirm. Pure/dependency-injected (op, ws, sampler, log_err) so it is Tier-0 pytest-able
    with fakes, no live OBS. Returns (live, frozen, inconclusive)."""
    live = frozen = inconclusive = 0
    for inp, _snd in want:
        state, reason = sampler(ws, inp)
        if state == op.LIVENESS_LIVE:
            live += 1
        elif state == op.LIVENESS_FROZEN:
            log_err(f"#1180 liveness: '{inp}' FROZEN -- no new frames are being presented "
                    f"({reason}); ndi_source_name may be correct all along. Two causes produce the "
                    f"SAME byte-identical signal: (a) a WEDGED receiver thread (issue 1158 class) -- "
                    f"an OBS RESTART is the cure, a name re-set / --heal cannot revive it; (b) an "
                    f"upstream SENDER outage (dead camera / cambox down / paused) -- an OBS restart "
                    f"will NOT help. Confirm which via `recv-timing #797` (a frozen SENDER keeps "
                    f"advancing received=; a wedged RECEIVER freezes it) or a sibling box before "
                    f"restarting OBS")
            frozen += 1
        else:  # LIVENESS_INCONCLUSIVE
            log_err(f"#1180 liveness: '{inp}' INCONCLUSIVE ({reason}) -- left as-is, never torn "
                    f"down on a can't-confirm")
            inconclusive += 1
    return live, frozen, inconclusive


def _verify_live_exit_code(live, frozen, inconclusive):
    """#1180 --verify-live exit contract (pure, testable): 1 iff >=1 input is FROZEN (a
    name-correct-but-no-frames wedge -- the caller escalates to an OBS restart, loud); 0 iff every
    sampled input is LIVE (or nothing to verify); 3 iff none FROZEN but >=1 INCONCLUSIVE (could not
    confirm frame delivery -- the caller proceeds, never a false FROZEN). (2 is reserved by the
    caller for a WS connect/request error, mirroring --heal / --heal-wait.)"""
    if frozen > 0:
        return 1
    if inconclusive > 0:
        return 3
    return 0


def _run_verify_live_mode(args, want):
    """#1180 --verify-live: connect via the shared obs_phase2 client and run verify_live_mapping over
    the active inputs. Exits per _verify_live_exit_code (0 all live / 1 >=1 FROZEN / 2 WS error / 3
    could-not-confirm). Kept out of main()'s normal enforce path, mirroring _run_heal_mode /
    _run_heal_wait_mode, so rig-activation behaviour is unchanged."""
    # Empty active set: nothing to verify -> exit 0 BEFORE importing obs_phase2, which imports
    # websocket EAGERLY (sys.exit on a websocket-less host); the contract is 0 for an empty set,
    # not a missing-dep failure, and no socket is opened here regardless.
    if not want:
        print("#1180 verify-live: no active inputs to verify (empty active set)")
        sys.exit(0)
    import obs_phase2 as op  # lazy: the pure helpers above stay importable without websocket/obs_phase2
    try:
        ws = op._conn(args.host, args.password)
    except Exception as e:
        print(f"ERROR: OBS WS connect {args.host}: {e}", file=sys.stderr)
        sys.exit(2)

    def _sampler(ws_, inp):
        return op.sample_receiver_liveness(
            ws_, inp, args.verify_live_samples, args.verify_live_interval)

    try:
        live, frozen, inconclusive = verify_live_mapping(
            op, ws, want, _sampler, lambda m: print(m, file=sys.stderr))
    except Exception as e:
        print(f"ERROR: OBS WS request: {e}", file=sys.stderr)
        sys.exit(2)
    finally:
        try:
            ws.close()
        except Exception:
            # airuleset:script-ok best-effort ws.close() on an already-torn-down socket -- mirrors
            # _run_heal_mode's own close pattern; a close failure has no recovery path and no signal.
            pass

    print(f"#1180 verify-live: {live} live, {frozen} FROZEN, {inconclusive} inconclusive "
          f"(of {len(want)} active inputs)")
    sys.exit(_verify_live_exit_code(live, frozen, inconclusive))


def _run_heal_wait_mode(args, want):
    """#1197 --heal-wait: connect via the shared obs_phase2 client and run heal_wait_active_mapping
    over the active baselines, bounded by --heal-wait SECONDS. Exits per _heal_wait_exit_code
    (0 all discoverable+bound / 1 verify-failed / 2 WS error / 3 timed-out-with-inputs-still-absent).
    Kept out of main()'s normal enforce path, mirroring _run_heal_mode, so rig-activation is
    unchanged."""
    import obs_phase2 as op  # lazy: the pure helpers above stay importable without websocket/obs_phase2
    # #1197 review 🔵-3: an empty active set (--active "" from an unset CAMERA_ACTIVE_SET) has nothing
    # to warm -- return before opening a socket (which would otherwise burn the 10s connect timeout on
    # an unreachable OBS only to return (0,0,0)).
    if not want:
        print("#1197 heal-wait: no active inputs to warm (empty active set)")
        sys.exit(0)
    try:
        ws = op._conn(args.host, args.password)
    except Exception as e:
        print(f"ERROR: OBS WS connect {args.host}: {e}", file=sys.stderr)
        sys.exit(2)

    def _get_cur(ws_, inp):
        return (op._rpc(ws_, "GetInputSettings", {"inputName": inp}, ignore_err=True)
                .get("inputSettings", {}) or {}).get("ndi_source_name", "")

    try:
        done, waiting, failed = heal_wait_active_mapping(
            op, ws, want, _get_cur, lambda m: print(m, file=sys.stderr),
            args.heal_wait, args.heal_wait_interval)
    except Exception as e:
        print(f"ERROR: OBS WS request: {e}", file=sys.stderr)
        sys.exit(2)
    finally:
        try:
            ws.close()
        except Exception:
            # airuleset:script-ok best-effort ws.close() on an already-torn-down socket — mirrors
            # _run_heal_mode's own close pattern; a close failure has no recovery path and no signal.
            pass

    print(f"#1197 heal-wait: {done} discoverable+bound, {waiting} still absent, {failed} verify-failed "
          f"(of {len(want)} active inputs, bound {args.heal_wait:.0f}s)")
    sys.exit(_heal_wait_exit_code(done, waiting, failed))


def _run_heal_mode(args, want):
    """#1158 --heal: connect via the shared obs_phase2 client and run heal_active_mapping over the
    active baselines. Exits per _heal_exit_code (0 healed / 1 verify-failed / 2 WS error / 3
    nothing-healable). Kept out of main()'s normal enforce path so rig-activation behaviour is
    unchanged."""
    import obs_phase2 as op  # lazy: the pure helpers above stay importable without websocket/obs_phase2
    try:
        ws = op._conn(args.host, args.password)
    except Exception as e:
        print(f"ERROR: OBS WS connect {args.host}: {e}", file=sys.stderr)
        sys.exit(2)

    def _get_cur(ws_, inp):
        return (op._rpc(ws_, "GetInputSettings", {"inputName": inp}, ignore_err=True)
                .get("inputSettings", {}) or {}).get("ndi_source_name", "")

    try:
        healed, offline, failed, skipped = heal_active_mapping(
            op, ws, want, _get_cur, lambda m: print(m, file=sys.stderr))
    except Exception as e:
        print(f"ERROR: OBS WS request: {e}", file=sys.stderr)
        sys.exit(2)
    finally:
        try:
            ws.close()
        except Exception:
            # airuleset:script-ok best-effort ws.close() on an already-torn-down socket — mirrors
            # main()'s own close pattern; a close failure has no recovery path and no signal.
            pass

    print(f"#1158 heal: {healed} healed, {offline} offline (baseline absent), {failed} verify-failed, "
          f"{skipped} already-correct (of {len(want)} active inputs)")
    sys.exit(_heal_exit_code(healed, offline, failed))


def main():
    ap = argparse.ArgumentParser(description="#399 enforce strih NDI mapping")
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--map", action="append", help='"INPUT=SENDER" (repeatable; default = the active pins)')
    ap.add_argument(
        "--active",
        default=None,
        help="#827/#898: space/comma-separated camera names to enforce (default: "
        "$CAMERA_ACTIVE_SET env, or 'cam3'). Ignored when --map is given explicitly.",
    )
    ap.add_argument("--verify-only", action="store_true", help="check + report, do not set")
    ap.add_argument(
        "--heal",
        action="store_true",
        help="#1158 self-heal: re-enforce ONLY the drifted/emptied active inputs "
        "(discoverability-gated + read-back-verified via obs_phase2.reenforce_ndi_name); "
        "exit 0 iff >=1 healed (caller re-samples), 1 verify-failed, 2 WS error, 3 nothing healable.",
    )
    ap.add_argument(
        "--heal-wait",
        type=float,
        default=None,
        metavar="SECONDS",
        help="#1197 bounded COLD-finder discovery-wait heal: poll the DistroAV finder up to SECONDS "
        "(wall clock) for each active input's #399 baseline to become discoverable, then re-enforce "
        "it (shared obs_phase2.reenforce_ndi_name policy; never blind-sets an absent name). Returns "
        "early the instant every input is discoverable+bound. exit 0 all recovered, 1 verify-failed, "
        "2 WS error, 3 timed out with input(s) still absent.",
    )
    ap.add_argument(
        "--heal-wait-interval",
        type=float,
        default=4.0,
        metavar="SECONDS",
        help="#1197 poll cadence for --heal-wait (default 4s).",
    )
    ap.add_argument(
        "--verify-live",
        action="store_true",
        help="#1180 RECEIVER-liveness verify: for each active input, sample frame delivery over WS "
        "(a screenshot-diff -- proof frames ADVANCE, not just that the name is right, the term a "
        "name-only --heal misses). exit 0 all live, 1 >=1 FROZEN (name-correct-but-wedged receiver "
        "-> escalate to an OBS restart), 2 WS error, 3 could-not-confirm.",
    )
    ap.add_argument(
        "--verify-live-samples",
        type=int,
        default=None,
        metavar="N",
        help="#1180 screenshots per input for --verify-live (default: OBS_RECEIVER_LIVENESS_SAMPLES, 3).",
    )
    ap.add_argument(
        "--verify-live-interval",
        type=float,
        default=None,
        metavar="SECONDS",
        help="#1180 poll cadence for --verify-live (default: OBS_RECEIVER_LIVENESS_POLL_S, 2s).",
    )
    args = ap.parse_args()

    try:
        want = parse_map_args(args.map, args.active)
    except ValueError as e:
        sys.exit(f"ERROR: {e}")

    if args.verify_live:
        _run_verify_live_mode(args, want)  # exits
        return

    if args.heal_wait is not None:
        _run_heal_wait_mode(args, want)  # exits
        return

    if args.heal:
        _run_heal_mode(args, want)  # exits
        return

    try:
        ws = _conn(args.host, args.password)
    except Exception as e:
        print(f"ERROR: OBS WS connect {args.host}: {e}", file=sys.stderr)
        sys.exit(2)

    try:
        for inp, snd in want:
            cur = _get_binding(ws, inp)
            if cur == snd:
                print(f"  {inp!r:12} already -> {snd!r}")
                continue
            if args.verify_only:
                print(f"  {inp!r:12} DRIFT: {cur!r} (want {snd!r})")
                continue
            _rpc(ws, "SetInputSettings",
                 {"inputName": inp, "inputSettings": {"ndi_source_name": snd}, "overlay": True})
            print(f"  {inp!r:12} set: {cur!r} -> {snd!r}")

        bindings = {inp: _get_binding(ws, inp) for inp, _ in want}
    except Exception as e:
        print(f"ERROR: OBS WS request: {e}", file=sys.stderr)
        sys.exit(2)
    finally:
        try:
            ws.close()
        except Exception:
            pass

    dups = duplicates(bindings)
    wrong = [(inp, bindings[inp], snd) for inp, snd in want if bindings[inp] != snd]
    if dups:
        print(f"FAIL: duplicate camera bindings remain: {dups}", file=sys.stderr)
        sys.exit(1)
    if wrong and not args.verify_only:
        print(f"FAIL: inputs not bound to their pin: {wrong}", file=sys.stderr)
        sys.exit(1)
    if wrong and args.verify_only:
        print(f"DRIFT: {len(wrong)} input(s) off their pin (verify-only)", file=sys.stderr)
        sys.exit(1)
    print(f"PASS: {len(want)} inputs bound to distinct cameras "
          f"({', '.join(f'{i}->{s}' for i, s in want)})")
    sys.exit(0)


if __name__ == "__main__":
    main()
