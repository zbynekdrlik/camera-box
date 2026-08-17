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
| `ForceFullCompositionPipeline=On` | → **no counterpart.** `TearFree` is a dead option on `modesetting` (#841 live-verified — it's a legacy `xf86-video-intel` DDX feature). Tear-free comes from the ABSENCE of a compositor (picom off) → direct `Present`+PageFlip full-screen scanout. `#790`'s "+1 frame from FFCP" is inherently moot here (no FFCP). |
| picom off | applies (GPU-independent) — a compositor re-introduces a frame + tearing risk. |
| touchpad tap conf (#779) | applies (GPU-independent). |

## The drift facet (guard the live state, `drift-guard --check-imag` + the E2E `[0/8]` preflight)

`scripts/lib/imag-display-path.sh` is the SHARED gather + verdict core (same discipline as
`imag-power-envelope.sh` #1040 / `timesync-authority.sh` #596): `imag_display_path_verdict GATHER`
emits `<facet>|<STATUS>|<detail>` lines (facets `picom_process`, `picom_autostart`, `igpu_maxperf`,
`tap_conf`; OK/DRIFT/UNKNOWN), `imag_display_path_gather_remote_snippet` is the remote block, and
`imag_display_path_preflight_assert HOST [USER]` is the E2E fail-fast (DRIFT aborts; UNKNOWN warns).
Two-tier every facet: empty gather (SSH hiccup) → UNKNOWN, gathered-but-wrong → DRIFT, never a silent
pass. `pgrep` presence is probed + emitted (`PICOM_PGREP|ok/missing`) so a missing `pgrep` reads
UNKNOWN by name (#833), never a false "picom not running = OK". `igpu_maxperf` is hardware-agnostic
(#816): a box with no i915 `gt_min_freq` sysfs → `MAXPERF_APPLICABLE|0` → UNKNOWN, never false DRIFT.

Wired into `drift-guard.sh` as `check_imag_report`'s check #10 (the 15th optional arg) and into
`recording-e2e.sh`'s `[0/8]` preflight via the #675 sourced-lib pattern (a new source line + a new
call line, no anchored line edited — mind the recording-e2e.sh static-anchor collision GOTCHA).
