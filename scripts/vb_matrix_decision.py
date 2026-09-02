#!/usr/bin/env python3
"""#1227 — PURE decision core for the dev1 VB-Matrix alert watchdog.

WHY: VB-Audio Matrix (`VBAudioMatrix_x64.exe`) was NOT RUNNING on the stream box from the
2026-08-30 10:45 reboot until 2026-09-02 14:01 — the Scheduled Task `StartVBMatrix` has only a stale
one-shot TIME trigger, no AtLogon trigger, so after the reboot the virtual "VB-Matrix VASIO-8" ASIO
driver had no host. Both stream OBS inputs bound to it (`ASIO Input Capture`, `test-audio`) starved
for 3+ days (`asrc: … starved_blocks≈2940/min`) while `mbc` (Dante VSC) stayed healthy, and NOTHING
alarmed: the #1023 asio-starve watchdog ships DISABLED and needs a healthy-sibling discriminator; the
#1226 audio-lag watchdog reads `ts_lag_ms`, not process presence. bundle_state_gather now exposes a
`vb_matrix_running` facet on `:8899/bundle-state.json`; this module is the pure kernel of the dev1
watchdog that reads it from strih+stream and decides when to page.

No I/O, no ssh, no OBS, no MCP — exhaustively unit-testable (pytest), the strih-nic-selfheal #1199 /
ndi-halving #1203 / audio-lag #1226 python-mirror precedent, so the decision RED->GREENs LOCALLY
under Tier-0 (#557 kills cargo). The orchestrator scripts/vb-matrix-alert-watchdog.sh curls the JSON,
calls `analyze` here, and drives obs-watchdog-decision.sh's confirm/throttle + airuleset notify
(--dedup-key #1206).

Verdicts (classify):
  SKIP     -- box could not be fetched (:8899 down / box down). That page is #732 (bundle-state) /
              #1001 (network-reach) territory, never this watchdog's -- so paging requires a
              successfully fetched reading, and a dev1-side outage can only produce SKIP.
  UNKNOWN  -- box fetched OK but the vb_matrix_running facet is ABSENT: a box with no VB-Matrix
              install (imag) -> the gather omits it, or an old bundle-state-server not serving the
              facet yet. No reading -> no page (never a false negative on a non-VB-Matrix box).
  RUNNING  -- vb_matrix_running == "1": the VBAudioMatrix* process is alive. Healthy.
  DOWN     -- vb_matrix_running == "0": the box HAS the install on disk but the process is not
              running. The watchdog pages after a 2-pass confirm. (The install-present gate lives in
              the box-side gather; "0" already means "installed but dead", never "not installed".)
"""
import argparse
import json
import sys


def _loads_obj(bundle_json_text):
    """A /bundle-state.json body -> its dict, or None (empty/None input, non-JSON, or a non-object
    top level). The ONE json parse — `extract_vb_matrix`/`analyze`/`_main` route through it so a
    single pass never parses the body more than once."""
    if not bundle_json_text:
        return None
    try:
        obj = json.loads(bundle_json_text)
    except (ValueError, TypeError):
        return None
    return obj if isinstance(obj, dict) else None


def _str_or_none(obj, key):
    """`obj[key]` as a stripped str, or None for a missing/empty value (matching the gather's
    omit-when-empty / never-a-fabricated-reading contract)."""
    if not isinstance(obj, dict):
        return None
    raw = obj.get(key)
    if raw is None:
        return None
    s = str(raw).strip()
    return s if s != "" else None


def _vb_from_obj(obj):
    """`(running, name, pid, start)` from an already-parsed bundle dict (or None). `running` is the
    facet string "1"/"0" (or None when the facet is absent — UNKNOWN downstream); name/pid/start are
    context strings (or None). Never fabricates a reading."""
    return (
        _str_or_none(obj, "vb_matrix_running"),
        _str_or_none(obj, "vb_matrix_name"),
        _str_or_none(obj, "vb_matrix_pid"),
        _str_or_none(obj, "vb_matrix_start"),
    )


def extract_vb_matrix(bundle_json_text):
    """Parse a /bundle-state.json body -> `(running, name, pid, start)` (see `_vb_from_obj`)."""
    return _vb_from_obj(_loads_obj(bundle_json_text))


def classify(running, box_reachable):
    """One box's verdict. `box_reachable` is 1 iff the JSON was fetched this pass.

      box_reachable != 1  -> SKIP     (defer to #732/#1001; never our page)
      running is None     -> UNKNOWN  (facet absent: a non-VB-Matrix box / an old server; no page)
      running == "1"      -> RUNNING
      running == "0"      -> DOWN
      any other value     -> UNKNOWN  (junk value fails SAFE to no-page, never a false RUNNING/DOWN)
    """
    if box_reachable != 1:
        return "SKIP"
    if running is None:
        return "UNKNOWN"
    if running == "1":
        return "RUNNING"
    if running == "0":
        return "DOWN"
    return "UNKNOWN"


def analyze(bundle_json_text, box_reachable):
    """Fetch-result -> `{"verdict", "running", "name", "pid", "start"}`. When the box was not
    reachable, returns SKIP WITHOUT parsing the (empty) body, mirroring the caller's no-double-page
    guard."""
    if box_reachable != 1:
        return {"verdict": "SKIP", "running": None, "name": None, "pid": None, "start": None}
    running, name, pid, start = _vb_from_obj(_loads_obj(bundle_json_text))
    return {
        "verdict": classify(running, box_reachable),
        "running": running, "name": name, "pid": pid, "start": start,
    }


def _fmt(v):
    return "" if v is None else str(v)


def _main(argv):
    ap = argparse.ArgumentParser(description="pure VB-Matrix watchdog decisions (#1227)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    a = sub.add_parser("analyze",
                       help="read /bundle-state.json on stdin -> verdict + running + name + pid + start")
    a.add_argument("--box-reachable", type=int, required=True)

    ns = ap.parse_args(argv)

    if ns.cmd == "analyze":
        # The bundle-state body is well-formed UTF-8 JSON, but read bytes + tolerant-decode anyway
        # (the ndi_halving #1203 hotfix precedent: a strict read that raised was swallowed by the
        # caller's 2>/dev/null and read as SKIP forever). box_reachable=0 needs no stdin.
        text = "" if ns.box_reachable != 1 else sys.stdin.buffer.read().decode("utf-8", errors="replace")
        res = analyze(text, ns.box_reachable)
        for k in ("verdict", "running", "name", "pid", "start"):
            print(f"{k}={_fmt(res[k])}")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
