---
paths:
  - "scripts/lib/rig-mode-state.sh"
  - "scripts/splitter-port-alert-watchdog.sh"
  - "tests/harness_rig_mode_state_1290.rs"
  - "tests/harness_splitter_port_event_gate_1290.rs"
---

# Gating a TEST-premise dev1 watchdog on rig EVENT mode (#1290)

The dev1 splitter-port watchdog (#739) paged the owner's phone 5× during a LIVE broadcast
(2026-09-03 19:20–20:45): cam4/5/6/7 had no camera connected, read grayscale, and its
sibling-anchor `DEAD_PORT` verdict fired because cam1/2/3 (real cameras) read colour. That
discriminator ("a proven-good sibling on the SAME camera+splitter proves the shared camera is
delivering, so a grey box is a dead port") is a **TEST-rig premise** — the rig feeds ONE camera
through an HDMI splitter to EVERY cambox. In **EVENT/production** each cambox has its OWN real
camera, so a camera-less cambox is legitimately black and the sibling anchor says nothing about it.

## The EVENT signal = the DURABLE cam2 painter state — NOT the ledger, NOT the #281 heartbeat

`rig-mode.sh event` STOPS+DISABLES `cam2-painter.service` (#892) and removes `/run/rig-painter.pid`;
`rig-mode.sh test` enable-`--now`s it (its steady state since #1008/#937 is the ENABLED service).
So the painter state is the durable, non-staling TEST/EVENT discriminator — and
`scripts/lib/optical-chain-health.sh` ALREADY owns it (`optical_chain_painter_expected_from_snapshot`
= pidfile OR service-enabled; `optical_chain_painter_alive_from_snapshot` = pid-alive OR
service-active; `optical_chain_painter_probe_remote_snippet`;
`.claude/rules/optical-chain-health-watchdog.md`). The new `scripts/lib/rig-mode-state.sh`
**self-sources** it (ONE definition each, ONE cam2 probe snippet) and adds a `RIG_MODE_PROBE_OK`
reachability sentinel → a **3-state** `rig_mode_from_painter_snapshot`, fail-safe toward UNKNOWN in
every ambiguous case:

- **UNKNOWN** — no sentinel (cam2 ssh failed / empty), OR any of the four painter fields
  (`PID_PRESENT`/`PID_ALIVE`/`SVC_ENABLED`/`SVC_ACTIVE`) MISSING (a truncated/partial ssh read) or
  `?` (a systemctl no-answer HICCUP). The mode is UNREADABLE → the caller behaves EXACTLY as today.
  Fail-safe: an unreadable/partial/hiccup read must NEVER silence a real TEST-mode fault, and must
  NEVER be misread as a provable EVENT. **The sentinel's PRESENCE alone is NOT proof** — a hiccup
  after the (first-echoed) sentinel would otherwise read EVENT (#1290 review 🔴), so all four fields
  are required present + a definite 0/1.
- **TEST** — reachable AND the painter is **EXPECTED** (pidfile OR service enabled) **OR ALIVE**
  (pid alive OR service active). The `OR alive` is load-bearing (#1290 review 🔴): an
  active-but-**DISABLED** painter — an E2E `systemctl start cam2-painter` (start, not `enable --now`;
  `recording-e2e.sh` + the dead-man) on a rig last set to EVENT leaves the unit active+disabled
  until a reboot / `rig-mode.sh test` — is a RUNNING painter, and #892 makes a running painter
  incompatible with a clean broadcast, so it is positive evidence of NOT-EVENT. `expected`-only
  would have read it EVENT and silenced every real DEAD_PORT for days.
- **EVENT** — reachable AND the painter is NEITHER expected NOR alive (all four fields a definite 0).

The `?`-on-empty state is emitted by the shared `optical_chain_painter_probe_remote_snippet`
(`SVC_ENABLED|1` on `enabled`, `|0` on any other NON-EMPTY answer — disabled/not-found/static — and
`|?` on an EMPTY answer = a systemd-manager hiccup). Backward-compatible for optical-chain (`?` ≠ 1
→ expected=0 → skip, exactly today); rig-mode-state maps `?` → UNKNOWN.

**Rejected alternatives (do not switch to them):** the rig-test ledger (`rig_test_ledger_*`,
`/run/camera-box-rig-tests.jsonl`) tracks WHAT a harness STARTED (a kill-by-PID cleanup target),
not the MODE — it misreads an idle-but-in-TEST rig, a partially-cleaned ledger, or a rebooted TEST
rig (tmpfs cleared, painter respawned). The **#281 rig-heartbeat** (`rig_heartbeat_active`) is
stale-after-10-min BY DESIGN — an idle-but-in-TEST rig would misread as EVENT and silence a real
dead port (`optical-chain-health.sh` explicitly warns against it for this discriminator); the
heartbeat is the E2E-WINDOW axis (#1117), a different concern from the EVENT/production axis.

## The gate (splitter-port-alert-watchdog.sh)

`main()` computes `RIG_MODE` ONCE per pass (`rig_mode_probe` = one cam2 ssh, `timeout`-bounded and
`|| true`-guarded so a wedged cam2 `systemctl` can never stall the whole pass, mirroring
optical-chain's cam2 probe; overridable wholesale in tests) and passes it to `handle_box`. The
EVENT gate sits **only before the DEAD_PORT confirm block** — DEAD_PORT is the ONE TEST-premise
verdict; OK/NODATA/NO_CAPTURE/SOURCE_WIDE already return above and are report-only +
mode-independent, so they must NOT get the skip line. In **EVENT** mode a DEAD_PORT box logs
`<box> DEAD_PORT skip: rig in EVENT mode — TEST-premise verdict, no page (#1290)` + clears the
per-box confirm/throttle and returns before notify. TEST/UNKNOWN are byte-unchanged. No systemd
change (one extra cam2 ssh is well inside the unit's 120s budget); no python mirror (the gate lives
entirely in the bash orchestrator).

## Per-watchdog TEST-premise audit — only splitter-port needed the gate

| Watchdog | TEST-premise? | EVENT gate | Why |
|---|---|---|---|
| **splitter-port** (#739) | YES | **ADDED** | Sibling-anchor DEAD_PORT assumes one camera via the splitter to every cambox; false in EVENT. |
| **optical-chain** (#860) | already handled | none (verified) | Already gates on the SAME painter_expected signal — EVENT (painter disabled) → `skip`, never pages. |
| **grabber-stuck** (#1128) | NO | none | Pages on a specific over-rate (~62.5 fps) + persistent-corrupted HARDWARE signature a camera-less cambox never produces; a stuck grabber on-air is a genuine fault in both modes. |
| **cadence** (#794) | NO | none | Pages on a source delivering non-60 cadence (genlock/timing fault); a camera-less cambox at 60-fps-black reads OK, a frozen source is UNKNOWN (deferred #1052), a real 50-fps camera on-air IS a fault. |
| **frozen-input** (#1052/#1069) | NO | none | Pages on `received=` NOT advancing (wedged receiver / dead feed); designed to work in BOTH modes, scope self-filters to sources strih actually receives; a frozen production camera IS an owner-actionable fault — an EVENT gate here would MASK it. |

**The discriminator for "does a watchdog need an EVENT gate":** its paging verdict must depend on
the shared-camera-via-splitter TEST topology (splitter-port), OR the painter injection leg that
EVENT deliberately disables (optical-chain). A watchdog that pages on a genuine
hardware/timing/receiver fault equally real in production must NOT be EVENT-gated — that would
silence a real fault in the exact window it matters (the same reasoning as the #1117 gap-b audit
for E2E-window suppression). This is distinct from the E2E-WINDOW (#1117 `rig_heartbeat`) axis: a
splitter-port cam2-grey-mid-E2E edge (transient painter between states, cam2-painter stopped but
still enabled → classified TEST) is a possible follow-up (mirror optical-chain's rig_busy veto),
deliberately out of #1290's EVENT-mode scope.

## Residuals (stated, not silently accepted)

- **The gate depends on cam2 being REACHABLE during a show.** cam2 is the fixed painter box
  (`$PAINTER_IP`, `10.77.9.62`) and was retired as the camera-UNDER-TEST 2026-08-24
  (`scripts/camera-set.sh`) — its PAINTER role is unrelated and stays live. But if cam2 is powered
  off during a future production, the mode probe returns empty → UNKNOWN → the fleet false-pages
  again (fail-safe UNKNOWN pages as today). Acceptable (fail-safe direction is correct: page rather
  than silence), but worth knowing the fix has a single reachability dependency on cam2.
- **The per-watchdog audit covers the 5 dev1 watchdogs the ticket named**, out of ~25
  `scripts/*watchdog*.sh`. The sweep basis: only splitter-port and optical-chain carry a
  cambox-topology / painter premise; `avsync-lineup` is bound to stream-LIVE state (already
  EVENT-oriented); the rest are OBS-render / box-reachability / network / bundle-state level and
  page on faults equally real in production. A NEW dev1 watchdog whose paging verdict assumes the
  shared-camera-via-splitter topology (or the painter injection leg EVENT disables) must reuse
  `rig-mode-state.sh`; one that pages on a genuine hardware/timing/receiver fault must NOT gate.

## Tier-0

`rig-mode-state.sh` is a pure source-only lib (no `set -e`); verify RED→GREEN by sourcing it and
calling `rig_mode_from_painter_snapshot` over the five cases, and by the driver end-to-end (source
the watchdog `--dry-run`, override `probe_box`/`sshpass`/`rig_mode_probe`, run `main` twice). A
worktree worker may find `bash -c '…source…'` blocked (#1265) — the SUPERVISOR runs the Rust
harnesses (`tests/harness_rig_mode_state_1290.rs`, `tests/harness_splitter_port_event_gate_1290.rs`)
at CI; local net is `bash -n` + `shellcheck -S warning` + `cargo fmt --all --check`.
