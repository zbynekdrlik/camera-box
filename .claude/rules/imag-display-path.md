---
paths:
  - "scripts/lib/imag-display-path.sh"
  - "scripts/drift-guard.sh"
  - "tests/harness_imag_display_path_780.rs"
  - "tests/drm_lease_tolerance_1152.rs"
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
| picom must **NOT run** (issue 1146 REVERT — the #841 "picom off" doctrine STANDS) | applies (GPU-independent). imag drives TWO 60Hz outputs (eDP panel + HDMI projector) on independent crystals; a compositor-free direct scanout does not guarantee the projector is the sync target → the two clocks BEAT → a walking tear line, so issue 1146 first tried a picom v10 `vsync=true` compositor anchored on the projector. That cure was **REVERTED** (live-measured 2026-08-20): the compositor cost 21.57% OBS render skips on the 25W envelope — worse than the display-only tearing it cured; stopping picom returned the same session to 0.00% skips. So picom must NOT run (package/config/unit stay installed DORMANT), **picom RUNNING is the DRIFT**, and the tear-free direction is the OBS projector's own vsync (#1107) + this ticket's extended-layout facet. The dual-output beat analysis stays valid physics; only the compositor cure is rejected. |
| HDMI = xrandr **primary** (issue 1146) | applies (GPU-independent). The projector must be the primary output so picom/GL vsync anchors on it. A non-HDMI primary (the panel) is a DRIFT (the panel becomes the anchor → the projector tears). REVERSES the #522/#488 panel-primary autostart doctrine (whose real regression was a lost self-heal, now handled by imag-obs.service); projector placement is by connector type (imag_scenes.py), NOT the `--primary` flag, so the flip is safe. |
| touchpad tap conf (#779) | applies (GPU-independent). |
| EXTENDED layout, not MIRROR (issue 1146, 2026-08-27) | applies (GPU-independent). The eDP panel + HDMI projector must run at DISTINCT xrandr origins (extended); a MIRROR (both outputs at the SAME origin, e.g. `+0+0`) is two independent 60Hz CRTCs at one position, so present-vsync (#1107) locks to only ONE and the other free-runs → a walking tear line. This is the facet that CATCHES a mirror while `hdmi_primary` stays OK (in a mirror HDMI genuinely IS primary — the gate stayed green for days). The committed `~/.config/openbox/autostart` sets the extended layout at boot; a mirror is a LIVE DRIFT from that intent. Position-agnostic (origins must be distinct, never a hardcoded position). Facet `layout`; DRIFT names the beat. |

## The drift facet (guard the live state, `drift-guard --check-imag` + the E2E `[0/8]` preflight)

`scripts/lib/imag-display-path.sh` is the SHARED gather + verdict core (same discipline as
`imag-power-envelope.sh` #1040 / `timesync-authority.sh` #596): `imag_display_path_verdict GATHER`
emits `<facet>|<STATUS>|<detail>` lines (facets `picom_process`, `picom_service`, `hdmi_primary`,
`layout`, `igpu_maxperf`, `tap_conf`, `drm_output`; OK/DRIFT/UNKNOWN — the picom facets expect picom
NOT running per the issue-1146 REVERT (the #841 "picom off" doctrine STANDS — the vsync-compositor
cure was reverted for its 21.57% render-skip cost; picom RUNNING is the DRIFT), see the lib's
compositor-doctrine header for the full reversal→revert history; `drm_output` is the issue-1152 M4 facet: the DEFAULT-OFF `~/.camera-box/drm-output.json`
dormant = OK, ENABLED demands the current OBS log's `program scanout LIVE` proof else DRIFT, and
with it ENABLED the `hdmi_primary` facet flips lease-aware — HDMI is leased OUT of the X layout by
design, so a panel primary is then OK, never the issue-1146 DRIFT; full doctrine + the enable/
rollback runbook in `.claude/rules/obs-drm-output.md`),
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
