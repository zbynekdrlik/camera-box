---
paths:
  - "scripts/launch-obs-genlock.sh"
  - "scripts/obs-guarded-launch.ps1"
  - "scripts/obs-self-heal-install.sh"
  - "scripts/strih/**"
  - "tests/launch_obs_genlock.rs"
  - "tests/strih_ahk_respawn_774.rs"
---

# OBS launch-path contract (.lnk primary, per-box params) — #774/#775

Every automated OBS (re)launch on strih/stream goes through the box's **Start-Menu shortcut**
`OBS Studio.lnk`, NOT a bare `obs64.exe` — the shortcut carries the box's per-box params (strih:
`--enable-media-stream --verbose`, needed by the interkom VDO.ninja Browser source; a bare launch
drops them → "Permissions denied" rendered on program output, live incident 2026-07-15). This
contract is now **test-pinned across every path** — do not silently revert it:

- **`scripts/launch-obs-genlock.sh`** (`build_launch_program`): `.lnk` primary
  (`if (Test-Path $lnk) { Start-Process -FilePath $lnk }`), bare `$exe -WorkingDirectory bin\64bit`
  ONLY in the `else` fallback with a LOUD "params will be MISSING" warning — in BOTH the initial
  launch and the #786 redraw. Pinned by `tests/launch_obs_genlock.rs`
  (`program_launches_lnk_as_primary_bare_exe_only_as_fallback_775`,
  `redraw_relaunch_also_prefers_lnk_775`). The older `program_launches_with_bin64_cwd` pins only
  the FALLBACK — it is NOT the primary-path guard.
- **`scripts/obs-self-heal-install.sh`** (#411, ships disabled): reuses `build_launch_program`
  VERBATIM, so it inherits `.lnk`. Pinned by `self_heal_reuses_wrapper_launch_program_775`. Never
  fork a second launch path here.
- **strih AHK respawn** is versioned at **`scripts/strih/NL_STARTUP.ahk`** (was live-only, #774).
  `app1_path` = the `.lnk`; window match is PROCESS-based (`ahk_exe obs64.exe`, never a title, so
  an OBS title change can't stop respawn); `#SingleInstance Force` is intentional (clean
  double-start replace + re-arms `SafeLoop=1` on relaunch). Deploy is a **win-\* MCP** step per
  `scripts/strih/README.md` (never ssh for the GUI/AHK) — read that README before deploying it,
  and diff the committed copy vs the live `D:\_APPS\NL_STARTUP.ahk` for fidelity FIRST.

**`scripts/obs-guarded-launch.ps1` is the exception, and it is CORRECT:** it launches bare
`obs64.exe` with **NO per-box args**. That is fine because it is what STREAM's `OBS Studio.lnk` is
retargeted to (stream does not need `--enable-media-stream`; the script's job is the #786 ASIO
audio-buffering launch gate, not param-carrying). Do NOT "fix" it to add args blind — it is a
latent footgun ONLY if it were ever made **strih's** launcher (it would drop
`--enable-media-stream`). If strih ever needs the guarded launcher, teach it strih's params first.

Standing docs already covering related facts: obs-ops SKILL §144 ("every relaunch MUST go through
`launch-obs-genlock.sh`; do NOT hand-roll a Start-Process"); `.claude/rules/rig-state-inspection.md`
(the per-box `.lnk` TargetPath+Arguments live-resolve, the box-specific `.lnk` locations differ).
