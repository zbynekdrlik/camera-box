---
paths:
  - "scripts/frozen-input-alert-watchdog.sh"
  - "scripts/lib/frozen-input-health.sh"
  - "systemd/frozen-input-alert-watchdog.*"
  - "systemd/frozen-strih-input-alert-watchdog.*"
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

- **Blind-tap visibility (never a silent UNKNOWN):** fail-safe-to-UNKNOWN alone would let a source
  whose audit label never matches (a rename / re-create / drop-from-scene, or a `FROZEN_INPUT_SOURCES`
  drift) stay UNKNOWN forever and never page while the watchdog looks green — the "silent unknown" the
  standing rig-degradation-alert rule forbids. So the watchdog counts CONSECUTIVE BLIND probes (keyed
  on the PROBE returning no usable value, NOT on the verdict — a first-sample / counter-reset UNKNOWN
  with a real reading proves the tap works and resets it) and fires ONE "tap broken" WARN past
  `TAP_BROKEN_THRESHOLD` (~2 h). A SKIP pass (a box down per #1001, no probe) leaves the counter
  untouched.

- **Future enhancement (recorded, not built here):** extend the `:8899` bundle-state server to
  surface per-source `received=` so dev1 reads it via a clean HTTP GET (the mechanism #1001 already
  TCP-probes), retiring the ssh read. Deferred: it needs an updated `bundle-state-server.py` deployed
  to BOTH Windows boxes — extra surface not needed while the box is reachable by definition.

## Byte-safety of the received= extraction (#1258 layer 2)

`probe_received`'s extraction (grep the newest `genlock-fifo audit '<source>':` line, then sed out
the `received=` digits) runs `LC_ALL=C grep -aF ... | LC_ALL=C sed -n ...` — NOT plain grep/sed.
PowerShell 5.1's `gc` (no `-Encoding`) re-encodes a non-ASCII glyph anywhere in the fetched tail
(e.g. the approx-sign in `(approx F frames @ ...)`) as invalid UTF-8; in this fleet's UTF-8 locale,
plain grep then flags stdin BINARY (empty extraction) and plain sed's trailing `.*` leaves line-tail
garbage after the digits -- either way the value reads as "none" downstream. Full root cause + the
fix pattern: `.claude/rules/mv-reverify-escalate.md` “Layer 2” section. Never re-add a plain
(non-`LC_ALL=C`) grep/sed on this extraction.

## Install

Install on dev1 like the siblings: `systemctl --user enable --now frozen-input-alert-watchdog.timer`
(units in `systemd/`). Runs entirely dev1-side (one ssh log read to the reachable stream box);
nothing new is deployed to strih/stream. Smoke-test with
`FROZEN_INPUT_PROBE_CMD=<stub> scripts/frozen-input-alert-watchdog.sh --dry-run`.

## #1069 — the STRIH cambox-INPUT instance (SAME script, ENUMERATE mode)

The #1052 watchdog above watches the STREAM box + only `NDI 2ME PGM`. A wedged strih DistroAV
receiver (issue-1096: `genlock-fifo audit 'NDI camN': received=` FROZEN — the line KEEPS printing
with a stuck value — while strih keeps compositing the frozen frame into program) is a DIFFERENT
hop that #1052 / #1001 / #391 are all blind to. #1069 closes it with a SECOND INSTANCE of the SAME
`frozen-input-alert-watchdog.sh` (`systemd/frozen-strih-input-alert-watchdog.{service,timer}`),
NOT a duplicate script — set via env:

- `FROZEN_INPUT_RECEIVER=strih|10.77.9.202` + `FROZEN_INPUT_SENDER=strih` — read strih's OBS log;
  the no-double-page guard keys on strih (#1001 does not track the cambox senders).
- `FROZEN_INPUT_ENUMERATE=1` — derive the watched cambox set from the live OBS log EACH PASS (never
  a static cam-number list; the set moves with `CAMERA_ACTIVE_SET` / provisioning). The rig-verified
  cambox labels are `NDI cam1`..`NDI camN`; `NDI 2ME PGM (mv)` / `NDI 2ME PVW` are the program /
  preview feeds and are excluded. Enumeration self-filters to the EXPECTED-LIVE set: a source only
  prints an audit line while OBS receives it, and a WEDGED receiver KEEPS printing the line with a
  stuck `received=` (#1096) so a frozen input stays enumerated → the classify flags it FROZEN, while
  a legitimately-removed source simply drops out (no false page).
- `FROZEN_INPUT_ALERT_TAG=#1069` + a strih-OBS-restart `FROZEN_INPUT_CURE_HINT` — ALERT-ONLY (the
  ticket asks for an alarm, not auto-restart): the cure is EMBEDDED in the alert text (obs-liveness
  #391 convention). A wedged strih receiver only recovers via an OBS restart — reattach / recv-rebuild
  are ineffective (#1096).
- A distinct `FROZEN_INPUT_ALERT_STATE_FILE` (so the two instances' per-source + enum-blind state
  never collide), while the #1001 no-double-page state file stays the SHARED default.

### Reuse, never re-implement (the round-24 building blocks)

- The received=-DELTA verdict = the SAME `frozen_input_classify` the #1052 stream instance uses (the
  4-way FROZEN/ADVANCING/UNKNOWN/SKIP with the expected_live + sender_reachable gates). NOT a second
  delta impl — `mv_reverify_wedge_verdict` (#1093, WEDGE/NO_WEDGE binary) is the E2E-harness sibling
  of the SAME concept; the watchdog reuses the gated classify because the alert framework consumes it.
- The strih OBS-log RAW read (for enumeration) reuses `mv_reverify_probe_raw`
  (`scripts/lib/mv-reverify-escalate.sh`, #1093) — the flat session-agnostic `sshpass … powershell …
  -Tail` tail — NOT a third ssh-tail. Per-source `received=` reads keep the #1052 `probe_received`
  (already reads the RECEIVER box's log = strih here). Both overridable (`FROZEN_INPUT_ENUMERATE_CMD`
  / `FROZEN_INPUT_PROBE_CMD`) for the Tier-0 stubbed end-to-end test.

### The enumeration filter + the fail-loud blind guard

- Pure `frozen_input_cambox_sources [include_regex] [exclude_regex]` in
  `scripts/lib/frozen-input-health.sh` (defaults `cam` / `2me|pgm|pvw|multiview|preview|program`,
  case-insensitive; `|| true` makes "no match" a normal exit-0, not a pipefail failure — callers key
  on the empty OUTPUT). Tier-0 tested in `tests/harness_frozen_input_enum_1069.rs` (the pure filter +
  the whole watchdog ENUMERATE pass end-to-end with stubbed I/O).
- A failed/empty enumeration is NEVER a silent green (burn-target FAIL-CLOSED + rig-degradation
  rule): `enumerate_and_guard` counts consecutive-ZERO-source passes (only when the receiver is
  reachable) and fires ONE fail-loud "enumeration blind" WARN past `FROZEN_INPUT_ENUM_BLIND_THRESHOLD`
  (~2h), latched, reset the moment a real enumeration returns. The per-source tap-broken WARN (#1052)
  still covers a single source whose audit line vanishes while others enumerate.

### Install

`systemctl --user enable --now frozen-strih-input-alert-watchdog.timer` (units in `systemd/`; 5-min
cadence, offset from the #1052 stream instance). Dev1-side only; nothing deployed to strih. Smoke-test:
`FROZEN_INPUT_ENUMERATE=1 FROZEN_INPUT_RECEIVER='strih|10.77.9.202' FROZEN_INPUT_SENDER=strih FROZEN_INPUT_ALERT_TAG='#1069' FROZEN_INPUT_ENUMERATE_CMD=<stub-prints-raw-log> FROZEN_INPUT_PROBE_CMD=<stub> scripts/frozen-input-alert-watchdog.sh --dry-run`.

### bash quoting traps hit while building this (both cost a live debug loop)

- An apostrophe inside a `"${VAR:-default}"` default OPENS a single-quote context even within double
  quotes (`"${X:-a source's b}"` → `unexpected EOF looking for matching '`) — the parser then drifts
  and reports the syntax error at a much later `(`/line. Phrase env-default strings WITHOUT an
  apostrophe (`"the OBS receiver for the frozen source"`, not `"the frozen source's OBS receiver"`).
- A grep-filter pipeline exits 1 on "no match"; under `set -o pipefail` that fails the whole pipeline.
  A pure text FILTER whose empty result is a NORMAL outcome must end `|| true` so callers under
  pipefail (and the pure-fn test) do not read "nothing matched" as an error.
