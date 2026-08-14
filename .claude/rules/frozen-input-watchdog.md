---
paths:
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/lib/frozen-input-health.sh"
  - "systemd/frozen-input-alert-watchdog.*"
---

# stream FROZEN-INPUT alert watchdog (#1052 — network-reach watchdog PHASE 2)

Phase-2 of `.claude/rules/network-reach-watchdog.md` (#1001). Reachability alone classifies a box
REACHABLE/UNREACHABLE — it is blind to a box that is fully REACHABLE while its NDI input is silently
frozen on the last frame (the #767 receiver-rebind class: the cambox is fine but stream's DistroAV
receiver froze). Emit-freeze (#944) is CAMBOX-side and never fires for a receiver-side freeze. This
watchdog watches per-source ADVANCEMENT on the receiver (stream) and pages when an expected-live
input stops advancing while both boxes are reachable.

## The tap: genlock-fifo `received=` per-source counter (NOT screenshot-hash, NOT burn-decode)

Every genlocked source prints `genlock-fifo audit '<src>': received=N …` to the stream OBS log
~every 5 s (`GENLOCK_AUDIT_LOG_INTERVAL_NS`, `vendor/obs-studio/libobs/obs-source.c`). `received=` is
the cumulative count of frames the FIFO RECEIVED from the source, so a frozen input STOPS advancing
it. Chosen over the two alternatives on purpose:

- **NOT `GetSourceScreenshot` hash-delta** (what `frozen-camera-gate.py` #365 does for strih cameras):
  a live-but-STATIC source (a held program slide / a genuinely still shot) repeats identical PNG
  bytes and reads as FROZEN — a false page during a real event. `received=` counts network arrivals,
  not pixels, so it is immune to that. Screenshot-hash also needs an OBS-WS connection + per-source
  scene activation each pass (heavier; perturbs a live box).
- **NOT QR/burn-decode advancement** (the ticket's own sketch): heaviest, and TEST-mode only — an
  event run has burns OFF, blind exactly when coverage matters. The `received=` tap works in BOTH
  test and event mode (inherent genlock telemetry — no burns, no warm-up).

Per-pass state model (avoids the audit's ~5 s intra-log cadence trap entirely): sample the newest
`received=` per watched source ONCE per pass, PERSIST it (`recv_<key>`), compare to the prior pass.
`curr==prev` = FROZEN; `curr>prev` = ADVANCING; `curr<prev` = counter reset (OBS restarted) → UNKNOWN
(reseed, never page); no prior / unreadable current → UNKNOWN. The pure classifier is
`frozen_input_classify <prev> <curr> <expected_live> <sender_reachable>` in
`scripts/lib/frozen-input-health.sh` (tested exhaustively in
`tests/harness_frozen_input_health_1052.rs`).

## Scope + the NO-DOUBLE-PAGE guard — reuse #1001's on-disk state, never re-probe

- **Expected-live scope = the watched-source LIST** (`FROZEN_INPUT_SOURCES`, default `NDI 2ME PGM`).
  Only list inputs you expect continuously live; an idle input that may legitimately stop is simply
  not listed (the seam returns SKIP for `expected_live != 1`).
- **No double page:** before deciding, read issue-1001's OWN state file (`alerted_<box>`) — never
  re-probe. If the SENDER (`strih`, which produces `NDI 2ME PGM`) OR the RECEIVER (`stream`) box is
  CONFIRMED unreachable there, #1001 already owns the page and a frozen input is a downstream
  symptom → SKIP. Only "both boxes reachable BUT the input frozen" pages — exactly this class.

## Reuse the shared dev1-side alert framework — never invent a second mechanism

Same shape as `network-reach-alert-watchdog.sh` and the imag-obs/imag-power/optical-chain siblings: a
`set -uo pipefail` (NOT `-e`) systemd timer, a PURE decision lib, `airuleset.py notify` from dev1.
Reuse `scripts/lib/obs-watchdog-decision.sh` `obs_watchdog_confirm` (2-pass confirm) +
`obs_watchdog_alert_throttle` (~1 h re-alert). Per-source state
(`recv_/confirm_/alert_sig_/alert_passes_/alerted_<key>`, key = source name sanitized to `[A-Za-z0-9_]`)
so multiple sources page independently; an ADVANCING pass clears that source's confirm+throttle and
fires a one-shot recovery ("advancing again") ping.

## The best-effort probe — one FLAT ssh OBS-log tail, NEVER nested PowerShell

The counter is read with one flat `sshpass -p … ssh newlevel@stream 'powershell -NoProfile -Command
"gc (gci $env:APPDATA\obs-studio\logs\*.txt | sort LastWriteTime | select -last 1).FullName -Tail
N"'` — a session-agnostic FILE read, allowed for a headless dev1 watchdog per
`.claude/rules/win-ssh-vs-mcp.md`. `$env:APPDATA` has no spaces so NO inner double-quotes are needed
→ no nested-PowerShell trap (`.claude/rules/rig-state-inspection.md`). A failed read → empty sample →
the seam returns UNKNOWN → NEVER a false page (this is why the probe stays out of the pure lib and is
unit-tested only via the seam, exactly like #1001's `probe_ping`/`probe_tcp`). Override the whole read
with `FROZEN_INPUT_PROBE_CMD` (run with `<receiver_ip> <source>`, stdout = raw log text) for a
`--dry-run` smoke test or a future alternate tap.

- **Future enhancement (recorded, not built here):** extend the `:8899` bundle-state server to
  surface per-source `received=` so dev1 reads it via a clean HTTP GET (the mechanism #1001 already
  TCP-probes), retiring the ssh read. Deferred: it needs an updated `bundle-state-server.py` deployed
  to BOTH Windows boxes — extra surface not needed while the box is reachable by definition.

## Install

Install on dev1 like the siblings: `systemctl --user enable --now frozen-input-alert-watchdog.timer`
(units in `systemd/`). Runs entirely dev1-side (one ssh log read to the reachable stream box);
nothing new is deployed to strih/stream. Smoke-test with
`FROZEN_INPUT_PROBE_CMD=<stub> scripts/frozen-input-alert-watchdog.sh --dry-run`.
