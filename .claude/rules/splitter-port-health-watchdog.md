---
paths:
  - "scripts/splitter-port-alert-watchdog.sh"
  - "scripts/lib/splitter-health.sh"
  - "systemd/splitter-port-alert-watchdog.*"
  - "tests/harness_splitter_port_health_739.rs"
---

# Per-cambox HDMI-splitter-port no-signal recurrence watch (#739)

Closes the "a dead splitter port starves one cambox but masquerades as a per-camera colour bug" gap
(live 2026-07-13: 4/6 splitter ports died; each grabber renders no-signal differently — Elgato 4K S =
purple noise, ShadowCast 2 = flat grey — so the failures looked like per-camera tint problems and
burned two days of tint-hunting). The mid-run "recurrence" chased in the ticket's 2026-07-14 comments
was later exonerated as a recording-verdict decoder bug (#754); this watchdog is scoped to the
ORIGINAL no-signal event only.

## The FIRST check for "weird colours on some cameras" is per-box SIGNAL PRESENCE, never card tuning (playbook)

The rig feeds **ONE camera through an HDMI splitter to every cambox** (memory: identical picture on
all boxes ⇒ a fault is ONE fault, never N per-box faults). So the ONLY way per-cambox capture can
differ is each box's **individual leg** — its splitter output port (+ cable/grabber). When some
cameras look wrong (grey / tinted / noisy) while others look fine, the FIRST diagnosis is **compare
each box's signal presence against the fleet**, NOT tune the per-card V4L2 colour controls. The
per-card no-signal-rendering table (Elgato purple noise / ShadowCast flat grey / etc.) lives in
`.claude/skills/capture`. Tuning a card to "fix" a dead-port grey is the exact two-day dead end #739
records.

## The discriminator (the whole design) — self-anchoring, no reference-anchor guard needed

`scripts/lib/splitter-health.sh` (pure, `splitter_health_classify`) fires a **SPLITTER-PORT
suspicion iff a box is degraded (not capturing OR grayscale) AND ≥1 SIBLING is proven-good**
(reachable + capturing + colour). A proven-good sibling proves the shared camera is delivering AND
dev1's path to the rig is up, so the only element that can differ for the bad box is its own output
port. If EVERY reachable box is equally degraded → `SOURCE_WIDE` (shared camera / AWB / idle rig, NOT
a per-port fault) → **report-only, never paged** (paging here would false-fire every time the source
camera is legitimately off between events). An unreadable box → `NODATA`, never a page. This differs
structurally from the network-reach watchdog (#1001): that one needs an explicit reference-anchor
guard because its per-box reachability has no fleet consensus; here the "≥1 proven-good sibling"
condition IS the anchor.

## Reuse the shared dev1-side alert framework — never invent a second mechanism

Same shape as the siblings (`network-reach-alert-watchdog.sh` #1001, `imag-obs-alert-watchdog.sh`
#882, `optical-chain-alert-watchdog.sh` #860): a `set -uo pipefail` (NOT `-e`) systemd `--user`
timer, a PURE decision lib, `airuleset.py notify` from dev1. Reuses
`scripts/lib/obs-watchdog-decision.sh` — `obs_watchdog_confirm` (2-pass confirm, so a single ssh /
journal blip never pages) + `obs_watchdog_alert_throttle` (~1h re-alert). State is PER-BOX
(`confirm_<cam>`/`alert_sig_<cam>`/`alert_passes_<cam>`/`alerted_<cam>`) so each cambox pages
independently; an OK pass clears that box's confirm+throttle and, if it was paged, fires ONE
"colour again" recovery ping.

## Reads the metric camera-box ALREADY logs — zero cambox code change

The per-box signal is the #299 chroma metric camera-box logs every ~5s to its journal:
`capture chroma: u_dev=X.X v_dev=Y.Y -> colour|grayscale (source likely monochrome)`. The watchdog
ssh-reads each ACTIVE cambox's last such line within a freshness window
(`journalctl -u camera-box --since "@<epoch>"`, epoch computed on dev1 and passed absolute since the
rig is dantesync-synced). The line's PRESENCE = liveness; its `-> colour|grayscale` = the content
signal. The active fleet is derived from `CAMERA_ACTIVE_SET` via `camera_resolve` (the #827
camera-active-set discipline — never a literal cam range). sshpass is fail-loud-preflighted (issue
833: a missing tool must fail by NAME, never read as a measured "all boxes unreachable").

## KNOWN RESIDUAL — the Elgato purple-noise no-signal mode is NOT caught

The readable signals (liveness + colour/grayscale) catch the **flat-grey** no-signal mode (ShadowCast)
and any **frame-stall** mode (grabber stops producing frames → no fresh chroma line). The **Elgato 4K
S purple-noise** no-signal mode is colourful AND keeps producing frames, so it reads as `capturing=1,
colour=1` = OK and is NOT caught. Catching it would need a NEW per-frame variance/entropy metric in
`src/capture.rs` (production Rust on every cambox + a fleet redeploy) — a separate, larger change,
deliberately out of scope here. If the active fleet uses Elgato grabbers on the shared-camera leg,
file that follow-up.

## Suspect-hardware (technician list, #688)

The HDMI splitter is a single point of failure for the whole test harness and degraded once already
(#739, 2026-07-13). A suspect-hardware / spare-unit note is posted on the #688 technician-session
ticket; this watchdog is the recurrence guard.

## Install + verify (dev1) — SHIPS DISABLED

The units in `systemd/` are NOT enabled by the PR. Install on dev1 like the siblings:

```bash
cp systemd/splitter-port-alert-watchdog.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now splitter-port-alert-watchdog.timer
# one-shot read-only check (never pages): logs each active box's verdict
scripts/splitter-port-alert-watchdog.sh --dry-run
```

Runs entirely dev1-side; nothing is deployed to the camboxes (read-only ssh journal reads).
