---
description: Read OBS/DistroAV/NDI versions + critical settings off live strih/stream (read-only) and FAIL loudly on drift from the pinned zero-loss set (vendor/README.md).
argument-hint: "[strih | stream | both]"
allowed-tools: Bash, Read, mcp__win-strih__Shell, mcp__win-strih__FileRead, mcp__win-stream-snv__Shell, mcp__win-stream-snv__FileRead
---

# /drift-guard — verify the production OBS boxes still match the pinned zero-loss set

User directive (2026-06-12): strih (`10.77.9.202`) + stream (`10.77.9.204`) must be **kept** on the
exact versions + settings that guarantee permanent zero-loss functionality. This command reads the
live state **read-only** via the win-* MCP tools and compares it against the pinned set in
`vendor/README.md` using `scripts/drift-guard.sh --compare`, which **FAILS loudly on any drift**.

The deterministic engine is `scripts/drift-guard.sh` (unit-tested in `tests/drift_guard.rs`); CI runs
its `--check-pins` facet on every build. This command drives the **live** facet, which CI cannot do
(GitHub runners can't reach the production LAN).

**Read-only only.** This command NEVER writes settings, restarts OBS, or touches prod. Restoring a
drifted box (redeploy / settings change / OBS restart) is a separate, off-air, **user-approved** step.

## Steps (run for `strih`, `stream`, or both — default both)

1. **Gather observed values off each box via its win-* MCP `Shell` (read-only).** Newest OBS log +
   NDI runtime DLL + genlock master-gate env var:

   ```powershell
   $d="$env:APPDATA\obs-studio\logs"
   $f=Get-ChildItem $d -Filter *.txt | Sort-Object LastWriteTime -Desc | Select-Object -First 1
   $log=Get-Content $f.FullName -Raw
   if($log -match 'OBS (\d+\.\d+\.\d+)'){ "obs_version=$($Matches[1])" }
   if($log -match 'DistroAV \(Version (\d+\.\d+\.\d+)\)'){ "distroav_version=$($Matches[1])" }
   $dll="C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll"
   if(Test-Path $dll){ "ndi_runtime=$((Get-Item $dll).VersionInfo.FileVersion)" }
   # OUTPUT fps = the `fps:` line inside the "video settings reset:" block:
   $vs=($log -split "`n"); for($i=0;$i -lt $vs.Count;$i++){ if($vs[$i] -match 'video settings reset:'){ for($j=$i;$j -lt $vs.Count;$j++){ if($vs[$j] -match 'fps:\s+(\d+)/'){ "output_fps=$($Matches[1])"; break } }; break } }
   # #257: the genlock master gate is a BUILD DEFAULT (always on, no OBS_GENLOCK_WALL_CLOCK env) —
   # the gate is PROVEN by the running OBS log marker, NOT an env read. ENABLED -> 1, DISABLED -> 0.
   # The genlock_wall_clock=1 we emit is the build-default sentinel (the capability marker is the
   # real proof, gathered below as genlock_capability).
   if($log -match 'genlock:.*render tick ENABLED'){ "genlock_wall_clock=1" } elseif($log -match 'genlock:.*render tick DISABLED'){ "genlock_wall_clock=0" }
   ```

   (`ndi_runtime` from the DLL `FileVersion` is the robust source; the OBS log
   `NDI Library Version detected:` line is an equivalent fallback. If OBS is **not running**, the
   newest log is stale — note that, do not treat a stale read as live truth. #257: do **not** read the
   genlock gate from any `OBS_GENLOCK_WALL_CLOCK` env — there is none; the gate is the OBS-log render-tick
   marker. (Pre-#257 the MCP shell `$env:` read inherited a stale snapshot and showed empty 2026-06-14 while the OBS log
   correctly read `1`.)

1b. **Gather the per-input NDI ingest latency off the running OBS (#84).** The `latency` mode is a
   per-input DistroAV setting, read from the live obs-websocket (`ws://strih.lan:4455` /
   `ws://stream.lan:4455`; WS password is NOT committed — see the local memory
   `rig-obs-ws-credentials`, #260), NOT the OBS log/registry. For each genlocked
   **broadcast-path** input — on strih the camera ingests (`NDI cam5`=CAM1, `NDI cam1`=CAM3,
   `NDI cam3`=CAM4), on stream the strih→stream program feed (`NDI 2ME PGM`) — read
   `GetInputSettings`→`latency` (`0`=Normal is the pin — the certified low-latency zero-loss mode, #84) and build a comma-separated
   `input name=latency` list. (Non-broadcast inputs — preview/CG/lyrics — are out of scope of the
   pin; do not include them.) The reusable reader is `~/.cache/obsprobe/obs_inputs.py <host> <pw>`
   (read-only; lists every NDI input + its settings). If OBS is not running there is no live
   obs-websocket — omit the key so the engine reports it UNKNOWN rather than a stale guess.

1c. **Gather every `distroav.dll` location across the OBS scan paths (#124, single canonical plugin
   path).** OBS scans MULTIPLE module locations; the SAME `distroav.dll` in more than one of them lets
   a stale copy silently shadow the intended build (the mixed-version incident #119). Read-only enumerate
   every `distroav.dll` under the three scan paths and build a comma-separated list:

   ```powershell
   $paths = @(
     'C:\Program Files\obs-studio\obs-plugins\64bit',
     'C:\ProgramData\obs-studio\plugins',
     "$env:APPDATA\obs-studio\plugins"
   )
   $found = foreach($p in $paths){ if(Test-Path $p){ Get-ChildItem $p -Recurse -Filter distroav.dll -EA SilentlyContinue | Select-Object -ExpandProperty FullName } }
   "distroav_dll_paths=$($found -join ',')"
   ```

   The invariant: exactly ONE `distroav.dll`, at the canonical `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit`.
   (The `C:\Program Files\obs-studio\data\obs-plugins\distroav` folder is resources/locale — it carries
   NO `.dll`, so it never matches this scan. D:\_APPS\*, C:\genlock-*, C:\obs-backup\* are SEPARATE
   portable installs / staging / backups, NOT in the Program Files genlock OBS's scan paths — do not
   include them.) If the scan returns nothing, OBS may not be installed at the expected path — omit the
   key so the engine reports UNKNOWN rather than a false clean.

1d. **Gather each component's live BUILD SHA + the genlock CAPABILITY marker (#122).** A stock OBS
   32.2.0 is byte-for-byte a DIFFERENT build from our genlock 32.2.0 but reports the IDENTICAL
   marketing version — the marketing-version facet above cannot tell them apart (the #119/#120
   wrong-build-right-version that silently shipped). #122 closes that: read the deployed `obs.dll` +
   `distroav.dll` **Get-FileHash SHA256** (the actual bytes on the box) and the **genlock capability
   markers** the running OBS emitted (lines only our build produces), then compare them to the #120
   per-component SHA manifest of the build under test.

   ```powershell
   # The deployed component DLLs (Get-FileHash = the actual bytes; the #122 BUILD-SHA proof):
   $obsdll = "C:\Program Files\obs-studio\bin\64bit\obs.dll"
   $dadll  = "C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"
   if (Test-Path $obsdll) { "obs_dll_sha256=" + (Get-FileHash $obsdll -Algorithm SHA256).Hash.ToLower() }
   if (Test-Path $dadll)  { "distroav_dll_sha256=" + (Get-FileHash $dadll -Algorithm SHA256).Hash.ToLower() }
   # The genlock CAPABILITY marker text from the running OBS log (only OUR build emits these — a STOCK
   # OBS emits NONE). Pass the whole marker block as genlock_capability; the engine asserts presence.
   $d="$env:APPDATA\obs-studio\logs"
   $f=Get-ChildItem $d -Filter *.txt | Sort-Object LastWriteTime -Desc | Select-Object -First 1
   ((Get-Content $f.FullName) | Where-Object { $_ -match 'genlock:.*(render tick ENABLED|sub-frame jitter reserve|timestamp-aligned release)' }) -join "`n"
   # #246/#257 prod burn: the measurement burn is a per-source `genlock_burn` bool (no OBS_BURN_* env
   # any more) and must NEVER be left ON in prod. Read it over OBS WebSocket (NOT Machine env): for
   # each program-feeding input, check genlock_burn via the harness from dev1 (the boxes are reachable
   # over WS, not ssh) — emit `burn_env=none` when no source has it on, else a `SOURCE=on` list:
   #   for src in "NDI cam5" ... ; do scripts/obs_burn_filter.py check --host <ip> --input "$src"; done
   #   # any line with burn_on=True -> add "$src=on" to the list; none -> burn_env=none
   # (The `burn_env` key name is kept for the --compare contract; its value is now the genlock_burn state.)
   # #548 DEPLOYED-BUILD currency: the deployed vendored-genlock commit, read from the marker the
   # deploy writes at the install root. The engine dynamically compares it to origin/main's
   # vendored-genlock HEAD (#1292: git merge-base(sha, origin/main)..origin/main -- vendor/obs-studio
   # vendor/distroav, never a plain sha..origin/main ancestry range — a box AHEAD of main on the dev
   # candidate line must read OK, not falsely STALE) — the ONLY check that catches a box left on a
   # STALE genlock build, since the OBS/DistroAV/NDI version strings are byte-identical old-vs-new.
   # Read the CLEAN 40-hex from BUNDLE_MANIFEST.json .build_sha:
   $mf = "C:\Program Files\obs-studio\BUNDLE_MANIFEST.json"
   if (Test-Path $mf) { "genlock_build_sha=" + ((Get-Content $mf -Raw | ConvertFrom-Json).build_sha) }
   ```

   **Get the build-under-test's manifest** (the #120 `BUNDLE_MANIFEST.json` shipped inside the genlock
   bundle) so the engine has the EXPECTED per-component SHA to compare against. It is in the
   windows-genlock / windows-genlock-fast artifact for the deployed build's commit (CI runs can't reach
   prod, so the agent downloads it on dev1):

   ```bash
   # Full bundle (obs.dll + distroav.dll) — the deployed full stack:
   gh run download <full-genlock-run-id> --repo zbynekdrlik/camera-box -n obs-genlock-windows-x64 --dir ./gbundle
   # …or the hot-swap obs.dll bundle (obs.dll only) for an event-time DLL swap:
   gh run download <fast-run-id> --repo zbynekdrlik/camera-box -n obs-genlock-fast-dll --dir ./gbundle
   # The manifest the engine reads: ./gbundle/BUNDLE_MANIFEST.json
   ```

   If the box's deployed obs.dll came from a fast-dll hot-swap on top of an older full bundle, the
   obs.dll SHA matches the FAST manifest and the distroav.dll SHA the FULL bundle's manifest — match
   each component to the manifest that built it (the engine matches the dll by BASENAME, so either
   layout — flat `obs.dll` or nested `bin/64bit/obs.dll` — resolves). If OBS is not running or a DLL is
   missing, omit that key so the engine reports UNKNOWN rather than a false clean.

1e. **Gather the per-source genlock FIFO held-latency (#357) + (stream only) the last-calibrated
   A/V-align value (#390).** The effective held-latency per source comes from the SAME `genlock-fifo
   audit` log lines used for burns/health — filter the newest OBS log and build a comma-separated
   `SOURCE=latency_ms` list:

   ```powershell
   $d="$env:APPDATA\obs-studio\logs"
   $f=Get-ChildItem $d -Filter *.txt | Sort-Object LastWriteTime -Desc | Select-Object -First 1
   $pairs = (Get-Content $f.FullName) | ForEach-Object { if ($_ -match "genlock-fifo audit '([^']+)':.* latency_ms=(\d+)") { "$($Matches[1])=$($Matches[2])" } }
   "genlock_source_latency=$($pairs -join ',')"
   ```

   On **stream** ONLY, also try to read the #427-persisted last-calibrated value (OPTIONAL —
   best-effort; omit the key entirely if the file is missing, the facet degrades gracefully):

   ```powershell
   $j = "C:\ProgramData\camera-box\av-sync-last.json"
   if (Test-Path $j) { $v = (Get-Content $j | ConvertFrom-Json).applied_latency_ms; "av_sync_calibrated_ms=$v" }
   ```

2. **Compare against the pinned set** — feed every observed value to the engine, INCLUDING the #122
   per-component BUILD SHA + capability keys (supply `manifest=` to activate that facet):

   ```bash
   ./scripts/drift-guard.sh --compare host=strih \
     obs_version=<v> distroav_version=<v> ndi_runtime=<v> output_fps=<n> genlock_wall_clock=<0|1> \
     ndi_input_latency="NDI cam5=<n>,NDI cam1=<n>,NDI cam3=<n>" \
     distroav_dll_paths="<every distroav.dll location, comma-separated>" \
     manifest=./gbundle/BUNDLE_MANIFEST.json \
     obs_dll_sha256=<live Get-FileHash of obs.dll> \
     distroav_dll_sha256=<live Get-FileHash of distroav.dll> \
     genlock_capability="<the live genlock marker text>" \
     burn_env="<none, or the NAME=VALUE list from the burn-env gather>" \
     genlock_source_latency="<SOURCE=latency_ms list from step 1e>" \
     genlock_build_sha=<BUNDLE_MANIFEST.json .build_sha from the box — #548 dynamic staleness vs origin/main> \
     av_sync_calibrated_ms=<applied_latency_ms from av-sync-last.json, stream only, if reachable>
     # stream: ndi_input_latency="NDI 2ME PGM=<n>"
   ```

   - Exit `0` → **NO DRIFT**, the box matches the pinned zero-loss set AND the per-component BUILD SHAs
     + genlock capability match the manifest AND no test-burn is set in the prod Machine env. Report it.
   - Exit `20` → **DRIFT**: the output names each setting/SHA that differs (expected vs observed). A
     `obs_dll_sha256 DRIFT` or `genlock_capability DRIFT` line means the live OBS is a STOCK/wrong build
     even though its version matches (the #122 catch); a `burn … DRIFT (test-burn var set in prod Machine
     env)` line means a #246 test-burn is set on prod — clear it with `scripts/rig-mode.sh event` (and
     remove it from Machine scope). Report loudly. Do NOT fix it silently — restoring prod is
     off-air + user-approved.
   - Exit `11` → at least one value was **UNKNOWN** (not read). Drift status is incomplete, not
     clean — re-read the missing value (e.g. OBS not running, DLL path moved, manifest not downloaded)
     before trusting it.

3. **Report** per box: the observed set, the verdict (NO DRIFT / DRIFT `<settings>` / INCOMPLETE),
   and — on drift — exactly what to restore. Never claim a clean box you did not fully read.

## Post-deploy WHOLE-BUNDLE byte/SHA verify (#121) — run RIGHT AFTER a genlock deploy

The steps above check the two genlock DLLs (#122). After **deploying** a new genlock bundle to a box,
verify the deploy is byte-for-byte complete: EVERY file the bundle shipped must match the
`BUNDLE_MANIFEST.json` on the live box, and the deploy FAILS on ANY mismatch (a partial/corrupted
deploy where even one non-DLL file is stale must never pass — deploy-from-clean-tree's contract).

1. **Hash every deployed bundle file off the box** (read-only, win-* MCP `Shell`). Walk the install
   roots the bundle deployed to and emit one `relpath=sha256` line per file, where `relpath` matches
   the manifest's `files[]` path (forward slashes). The deployed roots map to the manifest layout —
   `bin/64bit/*` under `C:\Program Files\obs-studio\bin\64bit`, `obs-plugins/64bit/distroav.dll` at
   the canonical `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll`, top-level
   files (`GENLOCK_BUILD_SHA.txt`) at the install root:

   ```powershell
   # Adjust the roots to the deployed bundle's layout; emit manifest-relative path = sha256 per file.
   $obsRoot = "C:\Program Files\obs-studio"
   $da      = "C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll"
   $pairs = @()
   Get-ChildItem "$obsRoot\bin\64bit" -File -Recurse | ForEach-Object {
     $rel = "bin/64bit/" + $_.FullName.Substring("$obsRoot\bin\64bit\".Length).Replace('\','/')
     $pairs += "$rel=" + (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
   }
   if (Test-Path $da) { $pairs += "obs-plugins/64bit/distroav.dll=" + (Get-FileHash $da -Algorithm SHA256).Hash.ToLower() }
   # …add any top-level bundle files (e.g. GENLOCK_BUILD_SHA.txt) at their manifest path.
   $pairs -join ','
   ```

   (Only hash the files the manifest LISTS — first-party OBS plugins / locale already on the box that
   the bundle did not ship are not in `files[]` and are not part of the verify.)

2. **Verify against the manifest** — feed every observed file hash as `bundle_hashes=`:

   ```bash
   ./scripts/drift-guard.sh --compare host=strih \
     obs_version=… distroav_version=… ndi_runtime=… output_fps=… genlock_wall_clock=… \
     ndi_input_latency="…" distroav_dll_paths="…" genlock_capability="…" \
     manifest=./gbundle/BUNDLE_MANIFEST.json \
     bundle_hashes="GENLOCK_BUILD_SHA.txt=<sha>,bin/64bit/obs64.exe=<sha>,bin/64bit/obs.dll=<sha>,obs-plugins/64bit/distroav.dll=<sha>"
   ```

   - Exit `0` → **NO DRIFT**, the `bundle_files N/N verified` line shows every shipped file matches —
     the deploy is byte-for-byte complete. (`bundle_hashes=` SUPERSEDES the #122 two-DLL SHA keys; it
     already covers obs.dll + distroav.dll by exact path, so you need not also pass
     `obs_dll_sha256=`/`distroav_dll_sha256=`.)
   - Exit `20` → a deployed file's bytes differ from the manifest (named in a `file … DRIFT` line) —
     the deploy is corrupt/partial. Re-deploy that file. Do NOT certify the box.
   - Exit `11` → a manifest file was not hashed (named `file … UNKNOWN`) — re-hash it before trusting.

3. **Record `DEPLOYED_MANIFEST.json` on the box** (the audit artifact #121 requires — the live bytes
   that actually shipped, so the deployed state is auditable on the box after the fact). Write the
   per-file `Get-FileHash` set + the deployed build SHA next to the install via win-* MCP:

   ```powershell
   $manifest = @{ schema = "camera-box/deployed-manifest@1"; deployed_at = (Get-Date -Format o)
     build_sha = (Get-Content "C:\Program Files\obs-studio\GENLOCK_BUILD_SHA.txt" -EA SilentlyContinue)
     files = @() }
   # …populate $manifest.files from the same Get-FileHash walk as step 1 (path + sha256 + size)…
   $manifest | ConvertTo-Json -Depth 4 | Set-Content "C:\ProgramData\obs-studio\DEPLOYED_MANIFEST.json"
   ```

## Notes

- **imag-nb (`10.77.9.182`, Linux) — `scripts/drift-guard.sh --check-imag`** (no win-* MCP needed;
  drift-guard SSHes there itself from dev1). This is the MANUAL imag drift check. **#531: the genlock
  BUILD-STALENESS check is now DYNAMIC** — `--check-imag` compares the box's deployed
  `/opt/obs-genlock/GENLOCK_BUILD_SHA.txt` against **origin/main's vendored-genlock HEAD**
  (`git fetch` + **#1292**: `git merge-base(box, origin/main)..origin/main -- vendor/obs-studio
  vendor/distroav`, never a plain `box..origin/main` ancestry range — that reads a box AHEAD of main
  on the dev candidate line as FALSELY stale): a non-empty range = the box is genuinely BEHIND merged
  genlock commits = **STALE = DRIFT (exit 20)**, the #530 recurrence guard (imag-nb ran a stale
  genlock build at a live event -> 45fps); an empty range plus commits AHEAD of `origin/main` that
  ARE reachable from `origin/dev` reads **OK** (a recognized release-candidate build); ahead but
  reachable from neither `origin/main` nor `origin/dev` reads **ORPHAN = DRIFT (exit 20)**. This
  replaced the pre-#531 inert empty-static-pin compare (always UNKNOWN, could never FAIL). It is
  ALSO wired as a **pre-event LOUD WARNING** in `scripts/rig-mode.sh` (both `test` and `event`
  paths, advisory — never blocks going live). On a STALE report, the operator deploys the current
  build to imag-nb via `scripts/setup-imag.sh` step-12 at a safe off-event moment (NEVER an
  auto-redeploy — genlock deploys are user-timed). Run it read-only any time:
  `./scripts/drift-guard.sh --check-imag`.
- The OBS **auto-update dialog disabled** (#43) is a *build* property, not runtime-readable off a
  running box, so it is guarded against the vendored source by `tests/obs_updater_disabled.rs`, not
  here. A box running stock OBS 32.2.0 instead of our genlock build USED to be indistinguishable by
  version alone — **#122 closes that**: step 1d reads each component's live BUILD SHA (obs.dll /
  distroav.dll Get-FileHash) + the genlock capability markers and step 2 fails on any mismatch vs the
  #120 manifest, so a stock/wrong build is now caught even when the marketing version matches. (The
  deploy-only-our-build discipline remains the primary protection; this is the runtime backstop.)
- Re-pin (edit the table in `vendor/README.md`) only as part of a *deliberate* rollout — e.g. the
  30→60 fps step (#11) or activating genlock — never to silence a drift you did not intend.
- **#246 prod burn guard (#257 model):** the `burn_env` observed value (gathered in step 1) is
  asserted by `--compare`. **Post-#257 there are no `OBS_BURN_*` env vars** — the burn is a per-source
  `genlock_burn` bool toggled at runtime over OBS WebSocket. So `burn_env` now carries the observed
  `genlock_burn` WS state (`none` when every program-feeding source is off, else a `SOURCE=on` list),
  and `--compare` is DRIFT on ANY source left burning (it would draw QR onto the live broadcast — the
  #246 incident, originally a Machine-scope burn env that survived reboot). Clear a stray burn with
  `scripts/rig-mode.sh event` (drives `obs_burn_filter.py remove` over WS). For a quick read-only view
  of the genlock + burn state in ONE place (no full drift compare) use `scripts/drift-guard.sh
  --status host=<h> genlock_wall_clock=<0|1> genlock_capability="…" burn_env="…"` — informational
  (exit 0); the rich live OBS dock is the separate #188.
