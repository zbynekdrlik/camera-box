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
parses `rough=` (6th field). As of #1099 `splitter_health_classify` also CLASSIFIES on it — a colour
frame whose `rough=` exceeds the calibrated `NOISE_ROUGHNESS_THRESHOLD` (40.0) is a `PURPLE_NOISE`
verdict — but the watchdog **SURFACES it REPORT-ONLY** (a `NOISE-SUSPECT` per-box line), **never a
page**: the threshold is calibrated from the MEASURED healthy side, but the positive class is still
unmeasured, so arming the page is deferred (see the calibration section below; the `is_likely_noise`
Rust classifier stays the source-of-truth for the threshold value the watchdog mirrors). This follows
the data-first / verdict-gate-seam-calibration discipline: the healthy side sets the lower bound now,
a real positive-class episode sets the final page-arming threshold later.

**Backward-compat gotcha (rolling fleet redeploy):** an old cambox not yet carrying the metric logs
the OLD line (no `rough=`); `splitter_health_parse_probe` emits `rough=-` for it (a placeholder, never
a bogus number), keeping the 6-field record shape stable regardless of box version. Both existing
consumers of the line (`splitter-health.sh`, `verify-device.sh::chroma_check`) key on the
`-> colour|grayscale` tail with u_dev/v_dev at unchanged positions, so the `rough=` term sitting BEFORE
the `->` is fully additive — no consumer needed a change beyond the watchdog that reads it.

## Calibration + live status (#1099) — healthy side CALIBRATED; PURPLE_NOISE verdict WIRED REPORT-ONLY; the PAGE stays deferred

Phase 2 of the #1079 metric. The healthy side is measured densely and the classifier is now WIRED —
`splitter_health_classify` returns a `PURPLE_NOISE` verdict for a colour frame whose `rough=` exceeds
the calibrated `NOISE_ROUGHNESS_THRESHOLD` (40.0), and the watchdog SURFACES it REPORT-ONLY (a
`NOISE-SUSPECT` per-box log line). It **never pages**, because the ONE piece a live page needs — a real
Elgato purple-noise `rough=` (the positive class) — still does not exist in any live data. So the page
stays disarmed (owner stance "prah bez kalibracneho bodu nehybem"); DEAD_PORT remains the only paging
verdict.

**Calibration table (mined 16.9.2026, read-only journal probe of all 7 camboxes' OWN journals since
13.9 — the 60×-denser source vs the sparse 5-min watchdog).** ~38,119 colour + 421 grayscale live
`capture chroma:` samples. Real-content COLOUR roughness per box, plus the no-signal bands:

| box | grabber / role | colour n | median | p95 | p99 | max | grayscale max |
|-----|----------------|---------:|-------:|----:|----:|----:|--------------:|
| cam1 | ShadowCast 2 (issue 909) | 3468 | 10.8 | 11.4 | 11.7 | 12.2 | — |
| cam2 | imag-HDMI projection path | 19914 | 15.4 | 17.4 | 19.3 | **22.5** | 14.7 |
| cam3 | splitter camera | 2777 | 10.8 | 11.4 | 11.7 | 13.6 | — |
| cam4 | splitter camera | 3914 | 5.8 | 6.3 | 6.5 | 7.0 | — |
| cam5 | splitter camera | 2675 | 10.8 | 11.4 | 11.7 | 12.2 | — |
| cam6 | splitter camera | 2118 | 10.8 | 11.4 | 11.7 | 12.1 | — |
| cam7 | splitter camera | 3253 | 10.8 | 11.4 | 11.7 | 13.7 | — |
| **FLEET** | | **38119** | 11.7 | 16.7 | **18.5** | **22.5** | 14.7 |

- **Real-content (healthy) COLOUR band:** fleet p99 18.5, **max 22.5** (the max is cam2, the imag-HDMI
  projection path — the roughest LEGITIMATE content; splitter cameras top out at 7.0–13.7).
- **No-signal FLAT band (ShadowCast / re-cabling / dead port):** `colour=0`, `rough` 0.0–14.7 (mostly
  ~0.1 flat black; the cam2 12:59–13:04 re-cabling window on 16.9 read `colour=0 rough=0.1 → DEAD_PORT`,
  a flat grey, NOT purple noise). This is handled by the grayscale DEAD_PORT/SOURCE_WIDE path, not by
  the roughness classifier.
- **Purple-noise (positive) band:** **ABSENT** — 0 colour samples ≥ 25, 0 ≥ 30, 0 ≥ 40 anywhere in the
  window; every degraded watchdog verdict since 13.9 (DEAD_PORT/SOURCE_WIDE) was `colour=0` flat, never
  `colour=1` high-rough. The Elgato no-signal purple-noise mode is the only content that would reach the
  classifier, and no Elgato box produced a no-signal episode in the window.

**Threshold pick — `NOISE_ROUGHNESS_THRESHOLD = 40.0` (mirrored in `src/capture.rs` + the watchdog):**
- ≥ 2× the fleet healthy colour p99 (18.5 → 37.0) — a data-first separation margin.
- Above the measured healthy colour max (22.5) by 1.78× — no observed legitimate content, including
  cam2's high-frequency imag path, comes within 17 units.
- Well below the analytic uncorrelated-luma noise floor ≈73 (`E[|Y0−Y1|]=(235−16)/3`) — so genuine
  structureless static (physics: roughness ≫ picture) is still caught.
- On current data 40.0 (like the prior 30.0) never fires the report-only surface, so the change
  introduces NO false NOISE-SUSPECT lines; it only sharpens a FUTURE real-noise sample's classification.

**GO-LIVE condition (what arming the page still needs — do NOT arm until BOTH):**
1. **A real Elgato purple-noise `rough=` (the positive class)** — from a genuine no-signal episode on an
   Elgato box, or a reproduction that does NOT unplug the live rig signal path. Its measured roughness
   sets the TRUE noise floor (the ~73 is analytic, unverified against a rendered Elgato pattern), which
   is what makes "clearly below the noise floor" checkable and lets the threshold be finalised.
2. **The sibling self-anchor for NOISE**, mirroring DEAD_PORT: a box is a per-port NOISE suspect only if
   it reads colour+high-rough WHILE ≥1 sibling on the same camera+splitter reads colour+LOW-rough; if
   every reachable box is equally rough → report-only (a shared source, e.g. a rig-wide artifact), never
   a page. (This phase's PURPLE_NOISE verdict is per-box report-only, so it does not yet carry the anchor.)

Until both hold, the verdict is surfaced (operator-legible telemetry) but the page is off. Re-verify the
#1079 metric binary on the Elgato positive-class boxes (CAM6/CAM7) whenever they rejoin the wire, or the
positive class stays silently starved. Full analysis: #1099 design comment.

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
