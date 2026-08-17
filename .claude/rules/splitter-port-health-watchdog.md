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
suspicion (`DEAD_PORT`, PAGE) iff a box is capturing but GRAYSCALE AND ≥1 SIBLING is proven-good**
(reachable + capturing + colour). A proven-good sibling proves the shared camera is delivering AND
dev1's path to the rig is up, so the only element that can differ for the bad box is its own output
port. This differs structurally from the network-reach watchdog (#1001): that one needs an explicit
reference-anchor guard because its per-box reachability has no fleet consensus; here the "≥1
proven-good sibling" condition IS the anchor.

**Why the PAGE keys on GRAYSCALE-while-capturing, not on liveness (learned from live verification).**
The ORIGINAL 2026-07-13 dead-port failure kept the grabbers PRODUCING frames (Elgato purple noise /
ShadowCast flat grey) — i.e. `capturing=1` with bad CONTENT. `capturing=0` (no fresh `capture chroma:`
line) is a **different, ambiguous class** — camera-box crashed / device-busy / stopped by an E2E run /
a genuine grabber stall — and on this rig it is ROUTINE (the camboxes cycle down constantly: an E2E
run stops camera-box to take the devices; a fresh live check found all three boxes stopped one minute
and all three OK the next). Attributing that to the splitter port would be a mis-attribution / false
page, so `capturing=0` → `NO_CAPTURE` = **report-only, never paged** (a fully-stalled grabber on a
dead port therefore lands in the log's NO_CAPTURE bucket for an operator, not a wrong page). The other
report-only verdicts: `SOURCE_WIDE` (every reachable box grayscale, no proven-good sibling → shared
camera / AWB / idle rig) and `NODATA` (unreadable box).

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

## Elgato purple-noise no-signal mode — a `rough=` spatial-roughness term, REPORT-ONLY (#1079)

The liveness + colour/grayscale signals catch the **flat-grey** no-signal mode (ShadowCast) and any
**frame-stall** mode (no fresh chroma line), but the **Elgato 4K S purple-noise** no-signal mode is
colourful AND keeps producing frames, so `is_color_frame` reads it as `capturing=1, colour=1` = OK.
The missing axis is **spatial structure**: a real picture has strongly correlated neighbouring pixels;
random static does not.

`camera-box` now logs a per-frame **spatial-roughness** term on the `capture chroma:` line
(`src/capture.rs::luma_roughness` — the mean `|Y0 − Y1|` adjacent-pixel luma delta over the same #299
subsample; low for structured content, high for noise):
`capture chroma: u_dev=X.X v_dev=Y.Y rough=R.R -> colour|grayscale`. `splitter_health_parse_probe`
parses `rough=` (6th field) and the watchdog **SURFACES it REPORT-ONLY** in each box's per-box log line.
It is fleet-wide telemetry only — `splitter_health_classify` is UNCHANGED and nothing pages/gates on
roughness yet. The calibratable classifier + threshold ship DORMANT
(`is_likely_noise` / `NOISE_ROUGHNESS_THRESHOLD`, unit-tested, wired into no live path — the #905
"keep the mechanism dormant, not deleted" pattern). A **data-first follow-up** walks
`NOISE_ROUGHNESS_THRESHOLD` against accumulated fleet `rough=` data before flipping `is_likely_noise`
into a live page/label (the window-gate-tolerance-walkdown / verdict-gate-seam-calibration discipline).

**Backward-compat gotcha (rolling fleet redeploy):** an old cambox not yet carrying the metric logs
the OLD line (no `rough=`); `splitter_health_parse_probe` emits `rough=-` for it (a placeholder, never
a bogus number), keeping the 6-field record shape stable regardless of box version. Both existing
consumers of the line (`splitter-health.sh`, `verify-device.sh::chroma_check`) key on the
`-> colour|grayscale` tail with u_dev/v_dev at unchanged positions, so the `rough=` term sitting BEFORE
the `->` is fully additive — no consumer needed a change beyond the watchdog that reads it.

## Suspect-hardware (technician list, #688)

The HDMI splitter is a single point of failure for the whole test harness and degraded once already
(#739, 2026-07-13). A suspect-hardware / spare-unit note is posted on the #688 technician-session
ticket; this watchdog is the recurrence guard.

## Testing the driver offline (no rig, no ssh) — the sourced-main pattern

`main()` is guarded by `[[ "${BASH_SOURCE[0]}" == "$0" ]]`, so a test can source the whole watchdog
(which pulls in `camera-set.sh`/`obs-watchdog-decision.sh`/`splitter-health.sh`) and then override
`probe_box` with a canned per-IP fleet + `sshpass` with a no-op stub (`sshpass() { :; }`) so the
fail-loud tool preflight passes without a real binary. Run `main` N times in `--dry-run` against a
per-test temp `SPLITTER_WATCH_STATE_FILE` and assert on stderr (the `log()` stream, incl. the
`WOULD alert` line). `tests/harness_splitter_port_watchdog_739.rs` is the worked example — it pins
the sibling arithmetic in BOTH directions (a lone colour sibling still pages the grey box; a lone
grey box with no reachable sibling never pages) and the DEAD_PORT/SOURCE_WIDE/NO_CAPTURE/NODATA
wiring, none of which the pure-lib test can reach. The pure lib is tested separately by
`tests/harness_splitter_port_health_739.rs` (source-only, no `main`). Both run under Tier-0 (bash
sources the scripts at test time; `cargo test --no-run` only compiles the Rust harness — the actual
RED→GREEN is observed by running the equivalent bash directly, since Tier-0 forbids `cargo test`).

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
