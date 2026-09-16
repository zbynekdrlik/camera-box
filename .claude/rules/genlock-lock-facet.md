---
paths:
  - "scripts/genlock-lock-alert-watchdog.sh"
  - "scripts/genlock_lock_decision.py"
  - "systemd/genlock-lock-alert-watchdog.*"
  - "tests/python/test_genlock_lock_decision_1299.py"
  - "tests/python/test_genlock_lock_gather_1299.py"
  - "tests/genlock_lock_json_guards.rs"
---

# Fleet-visible genlock LOCK facet + dev1 watchdog (#1299)

Makes the in-OBS genlock LOCK state (#1298, `genlock-lock-indicator.md`) visible to the FLEET, not
just to an operator at each box's statusbar: a `genlock_lock` bundle-state facet + a dev1 alert
watchdog that pages once per incident when a managed OBS leaves LOCKED, + a rig-status chip. The
consuming sibling of #1298; one of the dev1 alert-watchdog family (#732/#1001/#1226).

## The transport decision — a log line, NOT a WS request (and WHY)

The three genlock producers (per-source FIFO counters, the NDI output's wall-stamping flag, the
dantesync `:8898` clock facet) are joined into ONE decided verdict in exactly ONE place — the
#1298 `OBSBasicStatusBar::UpdateGenlockLabel` widget. The clock facet is polled + cached INSIDE
that Qt widget; it is NOT reachable from the obs-websocket RequestHandler. So:

- A true obs-websocket **vendor** request (`obs_websocket_vendor_register_request`) needs a
  separate plugin to host it — none exists in this tree, and `linux-genlock.yml` builds
  `ENABLE_PLUGINS=OFF`. Not reachable without a new fleet plugin → rejected.
- A **built-in** RequestHandler request (the #806 `SetAsrcOuterBiasPpm` precedent) IS reachable,
  but can read only inputs[]+output{} from libobs — NOT the widget's cached clock; it would have to
  re-poll `:8898` itself (a second read that can DISAGREE with the statusbar) → rejected.
- **CHOSEN:** the widget emits its ALREADY-decided verdict as a versioned `genlock-lock-json:`
  OBS-log line; bundle-state reads the newest one from its #1222 bounded tail. The clock facet is
  delivered exactly as the statusbar decided it (one producer join point — the fleet facet can
  never disagree with the widget), the vendored-C++ surface is the smallest of the three, and it is
  fleet-reachable the moment the #1298/#1299 bundle deploys. The ticket sanctions exactly this
  fallback.

## Where each piece lives

| Piece | File | Notes |
|---|---|---|
| Producer emission | `vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp` (`genlock_build_lock_json` + `genlock_json_append_escaped` + the `genlock-lock-json: %s (#1299)` blog) | On state/reason change AND a ~30 s heartbeat (`GENLOCK_JSON_HEARTBEAT_TICKS`) so the bounded TAIL always holds a fresh one. Pure `std::string` (no obs_data) → lift-compilable under g++. |
| Vendored anchors | `tests/genlock_lock_json_guards.rs` + BOTH `windows-genlock{,-fast}.yml` pwsh gates (in the #1298 "Assert in-OBS genlock LOCK indicator present" step) | 3-copy lock-step per `obs-titlebar-build-id.md`. The marker is mutually non-substring with `genlock-lock:` and every `genlock-*`/`*-audit:` family. |
| Gather parser | `scripts/bundle_state_gather.py` (`genlock_lock_facet_from_log`) + wired in `scripts/bundle-state-server.py` | Reshapes the newest `genlock-lock-json:` line into the nested facet `{state, reason, n_inputs, n_locked, n_absent, latency_ms, recent_event, qpc_drift_ms, clock:{state}, output:{present, stamping_wallclock}, inputs:{<name>:{locked, connected, ...}}, source:"log"}`. Attached AFTER `build_bundle_state` (a nested object, NOT a flat version-integrity string). OMIT-when-absent (a stock OBS → no facet, never a false UNLOCKED). Reuses the already-bounded log_text (no second read → no new #1222 cache). #1299 (schema v2) added `n_absent` (senderless input count) + per-input `connected`; #1299 Part 3 (schema v3) added `recent_event_inputs:[{name, events}]` (the top recent-event offender, omit-when-absent). #1303 (schema v4) added `audio_unexpected_inputs:[{name}]` (a silent-by-contract source found AUDIBLE — the double-audio hazard; names-only, omit-when-absent), enriched to `reason=audio_unexpected:<name>` by `genlock_lock_decision.analyze`. All degrade gracefully for an older line (`n_absent`→None, `connected`→True, no `recent_event_inputs`/`audio_unexpected_inputs` key). |
| Box roster | `scripts/lib/obs-fleet.sh` `genlock-lock` facet = `strih stream imag resolume` | imag is a pure receiver that still locks every input → IN scope. resolume is paged only while `obs_fleet_is_home` (traveling). |
| Pure decision | `scripts/genlock_lock_decision.py` (`decide` mirror of `src/genlock_lock_state.rs` + `analyze`) | pytest Tier-0 (#1199 mirror). The watchdog trusts the facet's carried `state`; `decide()` is the parity cross-check fed the SAME precedence table as the Rust/C gate. |
| Watchdog | `scripts/genlock-lock-alert-watchdog.sh` + `systemd/genlock-lock-alert-watchdog.{service,timer,README.md}` | Reuses `obs-watchdog-decision.sh` confirm/throttle. Page UNLOCKED/DEGRADED after 2-pass confirm, stable `--dedup-key genlock-lock-$box` (#1206), recovery machine-channel-only, SKIP when `:8899` unreachable (→ #732/#1001), UNKNOWN when facet absent. SHIPS DISABLED. |
| Status chip | `scripts/rig-health-audit.py` (`genlock_lock_state_from_log` → a `genlock_lock=<state>` token) | Report-only FEEDER token; `rig-status.py`'s generic parser renders the chip (rig-status-page.md). |

## Gotchas

- **The `genlock-lock-json:` marker must stay mutually non-substring with `genlock-lock:`** (they
  sit on adjacent log lines from the same widget). `genlock-lock-json:` does not contain
  `genlock-lock:` (the `-json` sits before the colon) and vice versa — guarded by
  `tests/genlock_lock_json_guards.rs`. bundle-state greps `genlock-lock-json:`, rig-health-audit
  greps `genlock-lock:` — they never cross-match.
- **The facet is NESTED, so it bypasses `build_bundle_state`** (whose every-value-is-a-quoted-string
  contract the version-integrity gate regex depends on — it ignores the extra nested key). Attach it
  to the served `result` dict in bundle-state-server.py, only when present.
- **`output:"not-stamping"` means the output IS present, just not stamping** — the parser maps it to
  `{present:True, stamping_wallclock:False}`, NOT present:False. Only `"absent"` → present:False.
- **An absent-sender input does NOT page (#1299 reopen).** The widget's decided `state` (which the
  watchdog trusts) now excludes a senderless input from the DEGRADED gate via `n_absent` /
  `n_connected` (see `genlock-lock-indicator.md`), so stream's 'NDIA cg stream' (sender not running)
  no longer produces a chronic false DEGRADED page. The facet still SHOWS the absent input
  (`connected:false`, report-only) — a dead sender is #1001/#1052's alarm, not this watchdog's. The
  watchdog logs `n_absent` per pass for observability.
- **imag NOW HAS a `:8899` bundle-state server — the coverage hole is CLOSED (#1299 Part 2).**
  `systemd/imag-bundle-state-server.service` is a `--user` unit running the CANONICAL
  `bundle-state-server.py` with imag's flags (`--obs-log-dir %h/.config/obs-studio/logs
  --genlock-build-sha-file /opt/obs-genlock/GENLOCK_BUILD_SHA.txt`, empty OBS-WS password);
  `setup-imag.sh` step 28 installs the three sibling files (`bundle-state-server.py` +
  `bundle_state_gather.py` + `obs_phase2.py`) into `/opt/camera-box` and ENABLES the unit only (never
  `--now` — **the supervisor starts it once** after the fleet genlock deploy), and `verify-imag.sh`
  check `(ba)` gates it (unit enabled+active + `:8899` listening + `/bundle-state.json` carries
  `genlock_build_sha`). The Windows-only IDENTITY gathers (native tasklist/netstat/CIM, ProgramData
  DLL byte hashes, the AHK/shortcut Start-Menu paths, the PowerShell NDI-runtime read) DEGRADE to
  absent facets on Linux via a single `IS_WINDOWS = os.name == "nt"` gate at the gather boundary in
  `bundle-state-server.py` (no Windows-only subprocess spawned, no per-request WARNING);
  `genlock_build_sha` is lifted out of that gate (a cross-platform file read) so it keeps serving.
  `bundle_state_gather.py` needed NO change (it holds no Windows-only subprocess gather). So the
  watchdog now gets a real imag verdict instead of SKIPping it.
- **The watchdog trusts the carried `state`, not per-input deltas.** The widget already folds the
  60 s recent-event window into `state=DEGRADED` (reason `recent_event`); bundle-state is stateless
  per request, so the facet carries CUMULATIVE per-input counters (not `_delta`), for observability.
- **#1299 Part 3 — `recent_event` is CONNECTED + PHASE only, and NAMES the offender.** The widget's
  recent-event driver counts only `relocks+late_holds+backward_steps` of CONNECTED inputs (underruns
  DROPPED — a latency-budget miss owned by cg-chain-verify / issue 1302; absent-sender rebind churn
  excluded), which stops the chronic DEGRADED/recent_event false-page the cg feed + absent inputs
  produced. A v3 line carries `recent_event_inputs:[{name, events}]` (the top offender), which
  `genlock_lock_facet_from_log` surfaces OMIT-WHEN-ABSENT (a v1/v2 line, or an empty list, → no key,
  never a fabricated attribution); `genlock_lock_decision.analyze` enriches `reason` to
  `recent_event:<name>`, so the watchdog log line + Discord body name the input. Underruns remain in
  the per-input `underruns` counter (report-only). The 60 s window semantics are UNCHANGED (the fix
  was the feed, not the window).
- **The decision module lives in `scripts/`, not `scripts/lib/`** — the ticket said `scripts/lib/`,
  but all five sibling decision mirrors (audio_lag/av_step/ndi_halving/vb_matrix/strih_nic_selfheal)
  are in `scripts/`; `scripts/lib/` holds only `.sh`. Followed the established convention.
- **Acceptance dry-run needs the `GENLOCK_LOCK_FETCH_CMD` seam** — a `<cmd> <ip>`-prints-the-body
  executable replaces the live curl so `--dry-run` classifies a captured fixture with no live box
  (README §supervisor procedure). This is ALSO the worktree/CI Tier-0 seam (the #1265 rule: a
  worktree worker cannot source the `.sh` or run a stubbed dry-run locally; the supervisor runs it
  at integration).

## Reusable patterns this lane established

- **A vendored-C++ PRODUCER ↔ python CONSUMER JSON contract gets a MECHANICAL round-trip gate, not
  a hand-mirrored fixture.** `tests/python/test_genlock_lock_json_roundtrip_1299.py` LIFTS the pure
  builder (`genlock_build_lock_json` + `genlock_json_append_escaped`, sliced by signature → first
  `\n}\n`, the vendored-libobs / qpsk-marker lift pattern), compiles it under `g++ -Wformat=2
  -Werror`, RUNS it to emit real `genlock-lock-json:` lines, and feeds each through the ACTUAL
  `genlock_lock_facet_from_log` parser — asserting key names/values + the inputs-by-name reshape.
  This is what catches a C++ key rename/reorder that every text-anchor + symbol-presence gate
  misses. Make the builder pure `std::string` (no obs_data/Qt) precisely so it lifts; FAIL LOUD
  (pytest.fail, never skip) when no compiler. Reuse this shape for ANY vendored-emitter/py-reader
  JSON contract. (Closed the #1299 YELLOW-1 review finding.)
- **A NEW dev1 alert-watchdog `.sh` opens `set -euo pipefail` then `set +e` then `set -uo
  pipefail`.** The sibling watchdogs use `set -uo pipefail` (NOT -e, so one box's hiccup never
  aborts the pass), but `pre-write-script-check.sh` blocks a NEW shebang'd `.sh` lacking a literal
  `set -euo pipefail` in the first 15 lines (no `# airuleset:script-ok` bypass for that check). The
  three-line open satisfies the hook (`-euo` present) and nets exactly `-uo pipefail` (the `set +e`
  turns `-e` back off). Document the `set +e` with a one-line reason so a reviewer sees it is
  deliberate.

## Roster override on dev1 = quoted `name|host` entries (16.9.2026)

`GENLOCK_LOCK_BOXES` mirrors `obs_fleet_boxes genlock-lock` byte-for-byte: `strih|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan` (bare names resolve as hosts and read `:8899 not fetchable`). In a systemd drop-in the value has spaces, so it MUST be quoted — `Environment="GENLOCK_LOCK_BOXES=strih|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"`; unquoted, systemd keeps only the first token and logs `Invalid environment assignment`. imag is excluded this way while the box is being returned (issue 1316) and runs thermally clamped (issue 1040).
