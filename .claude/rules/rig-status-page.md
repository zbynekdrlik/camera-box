---
paths:
  - "scripts/rig-status.py"
  - "scripts/rig-health-audit.py"
  - "systemd/rig-status-*.service"
  - "systemd/rig-status-*.timer"
  - "tests/python/test_rig_status.py"
---

# Rig status page (#787) — a RENDERER over rig-health-audit.py, never a second prober

`scripts/rig-status.py` renders `scripts/rig-health-audit.py`'s sweep as a dev1-hosted status page
(HTML + JSON), on a systemd timer, with a deduped Discord alarm on FAIL. **The audit is the ONE
prober; the page never ssh/WS-es a node itself** — `update` shells out to the audit and parses its
stdout (the same discipline the audit uses shelling out to `cadence-health.sh`). Keep that boundary.

## The parser is GENERIC — do NOT add per-facet render code

`parse_audit()` tokenises the audit's `[VERDICT] node<pad> key=value... <<problems>>` line with NO
per-facet knowledge: it peels the `<<problems>>` block FIRST (it can contain internal spaces, e.g.
`warn:cadence cam2=50fps(!=60)`), then splits the rest into `key=value` / `key[bracketed]` chips
(bracket-before-kv order keeps `arrivals[CAM1=60,CAM2=60]` / `cadence[cam1=60]` whole). Consequence:
**any NEW facet the FEEDER adds renders as a chip automatically** — the cadence column (#1089) today,
the build-sha / genlock-parity column (bod 3 of #789) whenever the feeder gains it. So the #789
build-sha work belongs in `rig-health-audit.py` (a REPORT-ONLY feeder facet), NOT here — adding a
prober to this file would break the "renderer, not prober" boundary. Never special-case a facet key
in `rig-status.py`; if a facet needs a dedicated column, first ask whether the generic chip suffices.

## The parser's contract is the audit's `emit()` format — changing one breaks the other

`_NODE_RE` matches `emit()`'s exact `f"[{verdict}] {node:<7} {detail}"`. If you change
`rig-health-audit.py`'s emit shape (the `[VERDICT]` prefix, the node padding, or the `<<...>>`
problems wrapper), re-run `tests/python/test_rig_status.py` — the parser tests model the real
`check_cam`/`check_imag`/`check_windows_box` detail strings and will catch a drift.

## FALSE-GREEN is the load-bearing gotcha — an empty/crashed audit must be ERROR, never PASS

`summarize([])` returns `overall="PASS"` — so a crashed / empty / timed-out audit (0 node lines)
would render "VŠETKY NODY ZDRAVÉ — PASS" and page no one. That is the exact `tiché unknown` the
`rig-degradation-alert-immediately` + `no-overstatement` HARD rules forbid. The guard is
`overall_state(records, exit_code)` (empty records OR a crash exit outside the audit's 0/1/2 contract
→ `ERROR`) — the renderers use THIS, never a bare `summarize()`. `_run_audit` catches
`TimeoutExpired`/`OSError` → sentinel exit 124/127 (never crashes the updater), and `alert_condition`
pages on `prober-down:exitN` too, not only on FAIL nodes. Any change to the overall/alert path must
preserve: **no data ⇒ ERROR + a page, never a silent green.**

## Conventions to keep

- **Discord dedup reuses `scripts/lib/obs-watchdog-decision.sh`** (`obs_watchdog_alert_throttle`) via
  subprocess — no second throttle impl. `_throttle` defaults `alert_now=1` on a missing key (fail
  LOUD). `--dry-run` must NEVER mutate `alert.state` (it would eat the next real alert's fingerprint).
- **Serve dir (`--dir`) holds ONLY `index.html` + `status.json`;** private state (`history.jsonl` +
  `alert.state`) lives in a separate `--state-dir`, so `http.server` never lists internal files.
- **Ships DISABLED** — no `setup-*.sh` enables the units; enable opt-in with
  `systemctl --user enable --now rig-status-update.timer rig-status-page.service`. Page binds dev1
  TAILSCALE `100.104.8.125:8790` (address-by-tailscale; the rig LAN IP drifts on event travel), never
  localhost/public. version-on-dashboard: DOM-readable `data-version` + visible `v<semver>` + timestamp.
- **Tier-0 tested via pytest**, not cargo: `tests/python/test_rig_status.py` (CI runs
  `python -m pytest tests/python`, ci.yml). Load the module with `importlib.util.spec_from_file_location`
  (the `rig-health-audit.py` hyphenated-filename pattern) and unit-test the PURE functions — no ssh/WS.

## A feeder facet that COUNTS a remote signal — the remote program IS the counter; never a Python twin (#1108)

When a new facet counts something ON the box (an awk/grep over the existing ssh gather — e.g. the
`#1108` dantesync step-rate facet counting `[NTP] Stepped` lines), the REMOTE program is the
production counter: the Python side only parses its emitted marker (`re.search(r"ntp_steps_1h=(\d+)",
out)`), never re-counts. So make that remote program a module CONSTANT (`NTP_STEP_COUNT_AWK`, the awk
analogue of `CADENCE_LIB`) used verbatim in every `check_*` ssh command, and unit-test THAT constant
by shelling out to the real `awk`/`grep` against synthetic journals (`subprocess.run(["awk",
_mod.NTP_STEP_COUNT_AWK], input=...)`). Do NOT ALSO write a Python re-implementation of the same match
logic — a `count_ntp_steps(blob)`-style twin is DEAD in production (nothing calls it) and its passing
tests give FALSE coverage of a path the awk actually owns (the `#1108` review caught exactly this).
Two placement rules for the awk line in the combined ssh command: keep it so it is NOT the LAST
`;`-command (a `grep -c` exits 1 on no match → `ssh()` reads returncode≠0 as "unreachable"; `awk`'s
`END{print}` always exits 0 but still must not displace a positional `lines[-1]` read like check_cam's
loadavg), and gate its interpretation on a same-journal proxy — an unreadable dantesync journal also
nulls the `offset:` read, so `off_us is None ⇒ steprate UNKNOWN`, never a false 0 (#833).
