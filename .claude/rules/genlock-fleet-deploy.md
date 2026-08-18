---
paths:
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/lib/genlock-markers.sh"
---

# One canonical genlock deploy path across the whole rig (#789 bod 4 + bod 5)

`scripts/deploy-genlock-fleet.sh` is the SINGLE entry point for deploying the OBS genlock build to
strih + stream + imag from ONE anchor CI run id. Before it, each box had its own uncommitted path
(strih/stream = the prose runbook in `.claude/rules/rig-state-inspection.md §5`; imag =
`setup-imag.sh`) from THREE separate workflows/run-ids, so imag routinely lagged and refused the
Full-path E2E at the `genlock_parity` preflight (the recurring #923/#932 pain).

## The shape (a PLANNER + bounded ssh-executor, same as `launch-obs-genlock.sh`)

- Pure builder functions emit each box's program; unit-tested by sourcing (`tests/deploy_genlock_fleet.rs`).
- **Windows (strih/stream) is EMIT-ONLY** — a bash script cannot drive the `win-*` MCP
  (`win-ssh-vs-mcp` HARD rule), so it PRINTS the PowerShell program the agent pastes into the box's
  MCP Shell. **imag is Linux** (file copy + CLI = Context B), so execute mode actually scp's + ssh-runs.
- `--plan --run-id <id> --sha <headSha> --stage <dir> [--full|--fast] [--boxes …]` prints the whole
  plan offline (no network) — this is the Tier-0-tested surface. Execute mode (`--run-id …`) resolves
  + downloads + deploys live (supervisor-driven; the download/scp/ssh glue is untestable offline).

## Same-SHA cross-workflow resolution — the heart of "one canonical version"

Windows and Linux genlock artifacts come from SEPARATE workflows with SEPARATE run-ids
(`windows-genlock.yml` / `windows-genlock-fast.yml` vs `linux-genlock.yml`). So `--run-id` is only an
ANCHOR: its `headSha` is read, then `fleet_pick_run_at_sha` picks each sibling workflow's SUCCESSFUL
run AT THAT SAME SHA (`gh run list --json databaseId,headSha,conclusion | fleet_pick_run_at_sha SHA`).
If no sibling run exists at the SHA, it fails loud (build it first — e.g. the tag-dispatch recipe in
`rig-state-inspection.md`'s #923 note). This is what makes all three boxes converge to one commit.

## GOTCHA — a Windows deploy that stops AHK MUST restart it (verified) BEFORE handing off to `launch-obs-genlock.sh`

strih runs the `NL_STARTUP.ahk` (`AutoHotkey64`) watchdog that respawns obs64 via the `.lnk`. A deploy
must stop AHK for the robocopy (else it re-locks `data\`/`obs-plugins\` mid-copy → robocopy exit ≥ 8).
But **`launch-obs-genlock.sh` only restarts AHK it stopped ITSELF** (`if ($ahkStopped)`), and its #978
session gate then HARD-FAILS `exit 8` on `AutoHotkey64 count != 1`. So a deploy program that stops AHK
and defers the restart to `launch` leaves strih with NO watchdog AND makes the STEP-2 launch fail —
the two-step "one deploy path" breaks on the one box that has the watchdog. **Fix: the deploy program
restarts AHK itself, VERIFIED, before exiting** — reuse the ONE shared `scripts/lib/ahk-watchdog.sh`
`ahk_resolve_and_relaunch_ps` helper `launch` uses (never a fork; sets `$ahkRelaunchVerified`), fail
loud (`exit 9`) if it doesn't come back. This leaves AHK count==1 so `launch`'s gate passes.

## imag deploy = the WHOLE bundle, never a hand-picked subset (issue 1026)

The imag leg ships the ENTIRE linux bundle (`cp -a lib/x86_64-linux-gnu/.` incl EVERY
`obs-plugins/*.so` + the frontend + `share/obs/`), NOT the 4-file libobs/distroav/frontend/opengl set
that `setup-imag.sh` step 12 installs — a stale `obs-plugins/*.so` over a new `libobs.so.30` is a
latent SIGSEGV (`.claude/rules/rig-state-inspection.md §5b`, issue 1026). It sha256-verifies the
staged bytes vs `BUNDLE_MANIFEST.json` before install, checks BOTH the libobs and libobs-opengl SONAME,
and directs the mandatory post-restart WS filter-enum survival check
(`obs_burn_filter.py check` from dev1).

## Marker helper is ONE source of truth (bod 4)

`scripts/lib/genlock-markers.sh` (`genlock_write_markers` — atomic temp-then-rename of
`GENLOCK_BUILD_SHA.txt` + `DISTROAV_BUILD_SHA.txt` + `DEPLOYED_AT`) is shared. `setup-imag.sh` carries
a behaviorally-identical INLINE copy (it ships standalone to the box, so it cannot source the sibling
lib) locked by a byte-parity test. Retention (`genlock_retention_delete_plan`, keep newest N=3) is
PLAN-ONLY by default (`--yes` gates actual deletion; the deploy never silently `rm`s a backup).

## GOTCHA — DistroAV loads ONLY from ProgramData; the FULL deploy ships + byte-verifies it there (#1115)

OBS loads DistroAV EXCLUSIVELY from `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\
distroav.dll` — NEVER from `C:\Program Files\obs-studio\obs-plugins\64bit\distroav.dll` (that path is
absent on the boxes; a copy there is a shadow `drift-guard.sh` #124 flags). The FULL `copy_block`
therefore `/XF distroav.dll` on the Program-Files obs-plugins robocopy — that exclusion STAYS. What
`build_windows_deploy_program` FULL mode ALSO does (the `(6b)` block, #1115 / Option A): explicit
path-mapped `Copy-Item` of the staged bundle's `obs-plugins\64bit\distroav.dll` → the ProgramData
load path, a `distroav.dll.pre-789` backup alongside `obs.dll.pre-789`, and a fail-closed sha256
verify of the DEPLOYED ProgramData DLL against the manifest's distroav entry (mirrors the obs.dll
verify). FAST is obs.dll-only → the block is a no-op there (no distroav in the fast bundle).

- **DLL-scoped on purpose.** The `distroav\data\` tree is NOT mirrored: the byte-parity gate hashes
  only the DLL, and DistroAV data is stable across genlock rebuilds at the pinned 6.2.1. The
  bundle→ProgramData layout is NOT 1:1 (bundle `obs-plugins/64bit/distroav.dll` + `data/obs-plugins/
  distroav/`; on-box `plugins\distroav\bin\64bit\` + `plugins\distroav\data\`) — so it is an
  EXPLICIT one-DLL path map, never a bulk `robocopy` of the wrong tree.
- **The three-layer facet:** deploy writes the ProgramData DLL → `bundle-state-server.py` hashes the
  distroav FIRST-located copy (= ProgramData, since Program Files carries no shadow) into
  `distroav_dll_sha256` → `drift-guard.sh --compare` matches it to the manifest's `obs-plugins/64bit/
  distroav.dll` BY BASENAME. So Option A makes the gathered ProgramData sha equal the manifest sha.
- **Runtime ENABLE of the distroav compare is separate (issue 1100 / issue 1082 ENFORCE):** the gate
  auto-sources the FAST obs.dll-only manifest, so distroav is labelled SKIPPED at runtime until a
  distroav-bearing (FULL) manifest is auto-sourced — do NOT "fix" the FAST manifest to add distroav.
