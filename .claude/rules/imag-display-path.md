---
paths:
  - "scripts/lib/imag-display-path.sh"
  - "scripts/drift-guard.sh"
  - "tests/harness_imag_display_path_780.rs"
---

# imag display-path drift facets — the box is Intel-iGPU-only, NVIDIA-era tuning is obsolete (#780)

The "imag" projection box (10.77.9.182) is now an **Intel-iGPU-only** notebook (Raptor Lake-P UHD,
`modesetting`+glamor driver, **no discrete NVIDIA GPU** — `nvidia-settings`/`nvidia-smi` are ABSENT).
This was established live in #816 (`imag_has_discrete_nvidia` gate in `setup-imag.sh`) and #841. So
the NVIDIA-era display-tuning knobs the older tickets/comments reference **do not exist on this box**
— do not re-derive this, and never add a facet/check that shells `nvidia-settings` here (it will
`command not found` and, if read as a measured zero, becomes a false verdict — the #833 trap):

| NVIDIA-era knob | Intel reality on this box |
|---|---|
| `GPUPowerMizerMode=1` | → `imag-igpu-maxperf.service` (#841): pins `gt_min_freq_mhz` → `gt_RP0_freq_mhz`. This is a SEPARATE mechanism from the #1040 power-envelope (PL1/slpc/thermald = the thermal CEILING); maxperf is the clock FLOOR. |
| `ForceFullCompositionPipeline=On` | → the picom vsync compositor (issue 1146), not FFCP. `TearFree` is a dead option on `modesetting` (#841 live-verified — a legacy `xf86-video-intel` DDX feature), so the inert `20-tearfree.conf` on the live box is deliberately NOT provisioned. |
| picom **ON with vsync** (issue 1146, REVERSES #841) | applies (GPU-independent). imag drives TWO 60Hz outputs (eDP panel + HDMI projector) on independent crystals; GL/scanout vsyncs only the PRIMARY CRTC, so a compositor-free direct scanout does not guarantee the projector is the sync target → the two clocks BEAT → a walking tear line. The fix: a picom v10 glx `vsync=true` compositor (`unredir-if-possible=false` so the fullscreen Program projector stays composited) ANCHORED on the projector by making HDMI the xrandr primary. picom OFF is now the DRIFT. |
| HDMI = xrandr **primary** (issue 1146) | applies (GPU-independent). The projector must be the primary output so picom/GL vsync anchors on it. A non-HDMI primary (the panel) is a DRIFT (the panel becomes the anchor → the projector tears). REVERSES the #522/#488 panel-primary autostart doctrine (whose real regression was a lost self-heal, now handled by imag-obs.service); projector placement is by connector type (imag_scenes.py), NOT the `--primary` flag, so the flip is safe. |
| touchpad tap conf (#779) | applies (GPU-independent). |

## The drift facet (guard the live state, `drift-guard --check-imag` + the E2E `[0/8]` preflight)

`scripts/lib/imag-display-path.sh` is the SHARED gather + verdict core (same discipline as
`imag-power-envelope.sh` #1040 / `timesync-authority.sh` #596): `imag_display_path_verdict GATHER`
emits `<facet>|<STATUS>|<detail>` lines (facets `picom_process`, `picom_service`, `hdmi_primary`,
`igpu_maxperf`, `tap_conf`; OK/DRIFT/UNKNOWN — the picom polarity is INVERTED vs the original #780/#841
"picom off" facet, see the lib's compositor-doctrine-reversal header for issue 1146),
`imag_display_path_gather_remote_snippet` is the remote block, and `imag_display_path_preflight_assert
HOST [USER]` is the E2E fail-fast (DRIFT aborts; UNKNOWN warns). Two-tier every facet: empty gather
(SSH hiccup) → UNKNOWN, gathered-but-wrong → DRIFT, never a silent pass. `pgrep`/`xrandr` presence is
probed + emitted (`PICOM_PGREP|ok/missing`, `XRANDR|ok/missing`) so a missing tool reads UNKNOWN by
name (#833), never a false verdict; `hdmi_primary` with an empty PRIMARY_OUTPUT (X unreachable over
ssh) is UNKNOWN, never a false DRIFT. `picom_service` reads the bus-free `*.target.wants/picom.service`
enable symlink. `igpu_maxperf` is hardware-agnostic (#816): a box with no i915 `gt_min_freq` sysfs →
`MAXPERF_APPLICABLE|0` → UNKNOWN, never false DRIFT.

Wired into `drift-guard.sh` as `check_imag_report`'s check #10 (the 15th optional arg, a generic
per-facet loop so new facets flow automatically), into `recording-e2e.sh`'s `[0/8]` preflight via the
#675 sourced-lib pattern (a source line + a call line, no anchored line edited — mind the
recording-e2e.sh static-anchor collision GOTCHA), and into `verify-imag.sh`'s post-reboot acceptance
gate as check (z). The persistence side lives in `setup-imag.sh`: step 27 installs+enables picom
(enable-only), step 16 sets HDMI the xrandr primary.

## Maintenance — a facet's OK/DRIFT DOCTRINE is duplicated in ~5 parallel PROSE copies (issue 1146)

When you change a facet's polarity or add/remove a facet, the CODE lives in one place (the pure
verdict in `scripts/lib/imag-display-path.sh`), but the human-facing DOCTRINE is restated as prose
in ~5 places that all drift independently — and a review caught the drift-guard one after every
other copy had been updated (issue 1146 flipped picom-off→picom-on and it was missed):

1. the lib's own compositor-doctrine header comment,
2. `scripts/drift-guard.sh` check #10's comment block (above the generic facet loop),
3. `scripts/verify-imag.sh` check (z)'s header comment + the header Checks-list entry,
4. this rules doc's table + facet list,
5. every `tests/harness_imag_display_path_780.rs` / `tests/drift_guard.rs` comment + the
   `DISPLAY_PATH_GATHER_CLEAN` fixture that encodes the "clean" state.

The CODE at (2) needs no edit (the loop is generic), which is exactly why its stale COMMENT is easy
to miss. When reversing/adding a facet, `grep -rn "picom off\|picom OFF\|picom-off\|<old-doctrine
phrase>"` across `scripts/` + `tests/` + `.claude/rules/` and update every prose hit in the same
branch, not just the verdict function.
