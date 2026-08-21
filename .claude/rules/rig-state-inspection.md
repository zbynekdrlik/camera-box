---
paths:
  - "scripts/rig-mode.sh"
  - "scripts/launch-obs-genlock.sh"
  - "scripts/lib/rig-test-dropin.sh"
  - "scripts/obs_burn_filter.py"
  - "vendor/av-sync-dock/**"
  - "vendor/distroav/**"
  - "vendor/obs-studio/**"
---

# Inspecting live rig state without fooling yourself (#942 deploy session, 2026-08-02)

Traps hit inspecting AND deploying live rig state, one deploy session at a time. Each one either
wasted calls or nearly produced a false claim about the rig — which is worse than the wasted time,
because the standing rule is that a degraded rig gets a LOUD alarm and a rig that is fine must
never be reported as broken.

## 1. A merged `vendor/<plugin>/**` change is NOT live — check the deployed file's timestamp

The dock plugin `obs-audio-video-sync-dock.dll` (and any other `vendor/<plugin>/**` DLL) has **no
fast deploy path** — `windows-genlock-fast.yml` stages only `obs.dll` + `distroav.dll`. So a PR
that changes `vendor/av-sync-dock/**`, passes CI, merges, and closes its issue changes **nothing
on the rig**. #942 merged as `24ee66176` while both boxes were still running the DLL built
`2026-08-01T20:29:06` — the pre-fix binary — and the ticket looked done.

**Before believing any vendored-plugin fix is in effect, read the deployed artifact's own
timestamp/size off the box** and compare against the build you expect:

```bash
sshpass -p "$PW" ssh newlevel@<box> 'powershell -NoProfile -Command "
  (Get-Content \"C:\Program Files\obs-studio\GENLOCK_BUILD_SHA.txt\" -Raw).Trim()"'
# then per component: (Get-Item $p).LastWriteTime / .Length
```

Then work out what actually differs — `git log <deployed-sha>..<build-sha> -- vendor/` is the
honest answer to "does this build even contain my fix", and it is one command.

## 2. Nested PowerShell quoting through `ssh` fails SILENTLY — always send a `.ps1`

`ssh newlevel@box 'powershell -NoProfile -Command "...\"nested\"... $_ ..."'` returns **exit 0 with
NO output** when the quoting breaks. There is no error to read: it looks exactly like a command
that ran and printed nothing, which is easy to misread as "the box has no such state". It broke
twice in one session (once on `$_` inside a `ForEach-Object`, once on nested escaped quotes).

Write the script to a file, `scp -O` it, run it by path:

```bash
cat > /tmp/.../x.ps1 <<'PS1'
$ErrorActionPreference = "Stop"
...
PS1
sshpass -p "$PW" scp -O "$SP/x.ps1" newlevel@<box>:C:/x.ps1
sshpass -p "$PW" ssh newlevel@<box> 'powershell -NoProfile -ExecutionPolicy Bypass -File C:\x.ps1'
```

The heredoc is quoted (`<<'PS1'`), so PowerShell's `$var` and `$_` pass through untouched. This is
the same class as the repo's existing "commit messages with backticks need a quoted heredoc" rule.

## 3. `pgrep painter` finds nothing, and an all-zero `/dev/fb0` is NOT a black screen

The cam2 QR painter's process is **`frame-probe`** (`/usr/local/bin/frame-probe --paint-only
--dual-qr ...`). `pgrep -af "painter|qr"` matches NONE of it, so a naive grep concludes "TEST mode
is not running" on a perfectly healthy rig. Find it by its pidfile instead — `/run/rig-painter.pid`
— which is what `rig-mode.sh` itself writes.

Worse trap: the painter draws through **DRM** (`/dev/dri/card1`, visible in `/proc/<pid>/fd`), not
fbdev. So `dd if=/dev/fb0 | tr -d '\0' | wc -c` returns **0 for the whole framebuffer** on a box
that is painting perfectly — `/dev/fb0` is an unused emulation surface here. Reading zeros there
is evidence of nothing at all, and must never be reported as a dark monitor.

What DOES prove the painter is alive and working, cheaply:

```bash
p=$(cat /run/rig-painter.pid); kill -0 $p          # alive
ps -o etimes,time -p $p --no-headers                # CPU time must ADVANCE (~43% at 60 fps paint)
ls -l /proc/$p/fd | grep dri                        # which DRM node it owns
wc -l /run/rig-qpsk-markers.csv                     # sample twice: the row count must GROW
```

and, for the real end-to-end proof, the E2E gate's own `undecodable=0` — the only signal that says
the camera actually SEES what is painted.

## 4. After any OBS restart, `genlock_burn` reverts to the last SAVED scene collection

Not a leak and not a bug — OBS restores what was saved, which after a TEST-mode run is burns ON.
Re-check with `obs_burn_filter.py check` after every relaunch and re-run `rig-mode.sh test` so the
NDI mapping and the imag program routing are re-enforced too; the burn state alone is not the whole
TEST surface. (The genuine leak class — a run cancelled before `cleanup()` — is #246/#844.)

## 5. The plain-ssh FULL-BUNDLE deploy recipe — how to actually MAKE a vendor plugin fix live (#942, 2026-08-02)

Trap 1 above says a merged `vendor/<plugin>/**` change is not live. This is the recipe that
DEPLOYS it, run end-to-end over plain ssh (no MCP, no dev1 HTTP server) against both strih
(10.77.9.202) and stream (10.77.9.204) — confirmed working cleanly on both boxes. See
`.claude/skills/genlock/SKILL.md`'s FULL-BUNDLE runbook for the win-* MCP variant and its own
note on the dev1-initiated push default (#912 session). NOTE: the rig→dev1 silent-drop that
originally forced dev1-initiated-only was VALIDATED CLOSED 2026-08-14 (issue 916 — it no longer
reproduces; the incident NIC enp1s0 is down and rp_filter is loose), so box-pull FROM dev1 now
works; dev1-initiated push is kept here as the belt-and-braces default, not a hard necessity.

1. `gh run download <windows-genlock-run-id> -n obs-genlock-windows-x64 -D <dir>` (~2 min, ~718 MB
   unpacked incl. PDBs).
2. `zip -qrX bundle.zip . -x '*.pdb'` from inside that dir (~165 MB, ~30 s — PDBs are never
   deployed).
3. `sshpass -p "$PW" scp -O bundle.zip newlevel@<box>:C:/stage-<ticket>.zip` — DEV1-INITIATED push
   (3-5 s per box; belt-and-braces default — box-pull FROM dev1 now works too, issue 916 validated
   closed 2026-08-14, see the genlock skill's note above).
4. **Verify the transfer BEFORE expanding it.** A one-off `.ps1` prints `(Get-Item ...).Length` +
   `Get-FileHash -Algorithm SHA256` on the box; compare against `stat -c%s` + `sha256sum` on dev1.
   Both must match byte-for-byte — a truncated zip expanded over a live OBS install is a
   disaster, and this check costs one extra call.
5. ONE `deploy.ps1`, scp'd to the box and run BY PATH (never nested PowerShell over ssh — trap 2
   above): `Expand-Archive`, back up the 4 changed components to
   `C:\obs-backup\<date>-<ticket>\*.pre-<ticket>`, stop AutoHotkey64 IF PRESENT (strih has the AHK
   watchdog, stream does not — detect it, report `ahk_was_running=<0|1>`, and only restart it if
   this run is what stopped it), kill obs64 + obs-browser-page, clear
   `%APPDATA%\obs-studio\.sentinel\*`, then three surgical robocopy calls (never `/MIR`/`/PURGE`):
   - `bin\64bit` — `/E /XF *.pdb /MT:16 /R:2 /W:2`
   - `data` — `/E /XF obs-virtualcam-module64.dll /MT:16 /R:2 /W:2` (the `/XF` is mandatory: the
     Windows Camera Frame Server holds that file and robocopy retries it forever without the
     exclusion — see the FULL-BUNDLE runbook's own `/XF` gotcha for the same file)
   - `obs-plugins\64bit` — `/E /XF distroav.dll /MT:16 /R:2 /W:2` (distroav lives in `ProgramData`;
     copying it here creates a shadow copy `drift-guard` flags as drift)
   then copy `BUNDLE_MANIFEST.json` + `GENLOCK_BUILD_SHA.txt` to the install root.
   **All three robocopy calls returning exit code 3 is SUCCESS**, not failure — robocopy 0-7 is
   success (3 = files copied + extras present); only bit 8 (code >= 8) is failure. A reader who
   assumes "nonzero = failed" wrongly aborts a healthy deploy.
6. The deploy script must prove itself before you trust it: print `deployed_sha` read back from
   the box's own `GENLOCK_BUILD_SHA.txt`, and per changed component a box-vs-stage `Get-FileHash`
   comparison printing `match=True`. Never trust "robocopy said ok" alone.
7. **SUPERSEDED FOR OBS/AHK (2026-08-06, issue 998 deploy):** the old recipe here — relaunch via
   `Invoke-CimMethod Win32_Process Create` **over ssh** — puts obs64 (and AHK) into **session 0**:
   invisible on the interactive desktop, and the E2E preflight session-visibility gate
   (issue 958/977: obs64 SessionId must equal explorer.exe's) REJECTS every subsequent run. Worse,
   an AHK landed in session 0 keeps re-spawning obs64 back into session 0 after you fix it. The
   ONLY sanctioned OBS relaunch: generate the launch program with
   `bash scripts/launch-obs-genlock.sh --box <strih|stream> --force` and run it through that box's
   **win-\* MCP `Shell`** (which executes in the interactive session 1) — kill AHK first, launch
   OBS, start AHK LAST (after obs64 is up, so no bare-exe respawn race). CIM-over-ssh remains fine
   ONLY for headless CLI tools (the decode section below) where desktop visibility is irrelevant.
   Poll the newest log up to ~2 min for BOTH `render tick ENABLED` and the specific plugin's own
   load line, and print them.

**The global airuleset `pre-deploy-clean-tree.sh` hook (lives in `~/devel/airuleset/hooks/`, not
in this repo) will block every scp above** — `targets.md` carries
local-only rig IPs and is permanently uncommitted by design (see "DO NOT DELETE These Files" in
the project `CLAUDE.md`), so the tree is never clean for a deploy. Add
`# airuleset:deploy-dirty-ok <reason>` inline on each Bash call in this repo that does the
transfer/deploy.

### 5b. The FAST-DLL variant (libobs-only change) — when the full bundle is overkill (issue 960, 2026-08-03)

When the merged vendor change touches ONLY `vendor/obs-studio/libobs/**` (compiles into `obs.dll`),
the `windows-genlock-fast` artifact (`obs-genlock-fast-dll`: obs.dll + GENLOCK_BUILD_SHA.txt +
fast manifest, ~1.3 MB) replaces steps 1-2 above — but FIRST prove the fast build's tree matches
the merged head: `git diff <fast-build-sha> <merged-head> -- vendor/` must be EMPTY (the fast run
may have fired on an earlier commit whose later siblings were Rust/tests-only). Copy the artifact's
`GENLOCK_BUILD_SHA.txt` to the install root; do NOT copy its manifest (it lists only obs.dll —
overwriting the box's full-bundle `BUNDLE_MANIFEST.json` would misdescribe the install). Relaunch:
per the SUPERSEDED note in step 7 above — win-\* MCP `Shell` running the `launch-obs-genlock.sh`
planner output ONLY (never CIM-over-ssh, which lands OBS in session 0). Still resolve the box's
`OBS Studio.lnk` TargetPath+Arguments (via `WScript.Shell` COM) rather than a bare exe path —
a bare-exe launch silently drops the box-specific shortcut params (strih's
`--enable-media-stream --verbose` for the interkom Browser source; on stream the lnk targets the
guarded launcher, which then owns the issue-786 redraw). PowerShell
gotcha from the same session: `(Get-Content file)[0]` on a ONE-line file indexes a CHAR (String,
not array) — read markers with `-TotalCount 1` or guard on type (or wrap in `@(Get-Content …)` so
`[0]` is always a line). Two more from the issue-940 deploy (2026-08-04): (a) the boxes' lnk
locations DIFFER — stream has `C:\Users\Public\Desktop\OBS Studio.lnk`, strih has ONLY
`C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk` (directly in `Programs\`,
NOT in an `OBS Studio\` subfolder) — a candidate list missing that exact path silently falls back
to a bare-exe launch that drops strih's `--enable-media-stream` (interkom Browser source dead);
(b) after `Stop-Process -Force` on a warm ~1 GB obs64, the `bin\64bit\obs.dll` file handle can
stay LOCKED past a 3 s sleep — the swap's Copy-Item fails `being used by another process`; wait
~5 s+ or retry the copy once after the process list confirms obs64 is gone.

**A libobs-touching deploy MUST converge ALL THREE boxes — imag included — or the next E2E
refuses on genlock_parity (issue 962 session, 2026-08-03).** The fast-DLL variant above covers
only strih+stream, but imag-nb consumes the SAME `vendor/obs-studio/**` tree (its libobs.so.30);
the drift-guard cross-box parity check (issue 949 model) compares each pair's deployed
GENLOCK_BUILD_SHA over the INTERSECTION of consumed vendor paths, so a real libobs diff between
the Windows boxes' new sha and imag's old sha = DRIFT = every subsequent Full-path E2E run is
REFUSED at preflight. Live cost: the issue-960 deploy updated strih+stream only, and the very
next PR's E2E failed 3× on `genlock_parity DRIFT` before any verdict. Converge imag in the same
deploy cycle: `gh run download <linux-genlock run at the same/vendor-equivalent sha> -n
obs-genlock-linux-x86_64` + `-n distroav-linux-fast-so`, verify all four files against the
bundle's own BUNDLE_MANIFEST.json, install libobs.so.30 + libobs-opengl.so.30 + /usr/bin/obs +
distroav.so exactly per setup-imag.sh step-12 semantics (backups to /opt/obs-backup/previous,
SONAME + `nm -D -u … obs_display_set_render_divisor` checks, markers into /opt/obs-genlock/),
then restart via imag-obs-stop.sh + `pkill -9 -x obs` + sentinel clear + a DETACHED
`setsid nohup imag-obs-start.sh` (a foreground start over ssh holds the session past the tool
timeout), and verify from a FRESH ssh: `ps -o pid,lstart -C obs` start time AFTER the swap (the
#912 stop-race), `render tick ENABLED` in the newest OBS log, and both Projector windows present
(proof the start script's seed phase completed).

**An imag "fast" deploy must ship the FULL matched bundle — libobs + EVERY `obs-plugins/*.so`
built together — never `libobs.so.30` alone, and never even the hand-picked 4-file list above
without the rest of `obs-plugins/` (issue 1026, 2026-08-13).** The Windows single-file `obs.dll`
swap has NO safe Linux equivalent: on Linux the plugins (`obs-websocket.so`, `distroav.so`, …)
link against libobs's INTERNAL struct layout, so an old plugin over a new `libobs.so.30` is a
latent SIGSEGV, not a compatibility mode. Live cost: the 2026-08-12 deploy swapped ONLY
libobs.so.30 on imag; the stale `obs-websocket.so` then segfaulted OBS in
`get_const_root <- obs_enum` (twice, hours apart) whenever a WS client enumerated
filters — killing OBS overnight and refusing two E2E runs at the dead-OBS preflight. Deploy =
untar the whole CI bundle over `bin/ + lib/x86_64-linux-gnu/ (incl. ALL obs-plugins/*.so) +
share/obs/`, refresh the `/opt/obs-genlock/` markers, and after restart deliberately exercise a
WS filter-enum op (`obs_burn_filter.py check`) to prove the previously-crashing path survives.

## 6. A hook-BLOCKED Bash call runs NOTHING — including its own heredocs

When a PreToolUse hook blocks a Bash call, the ENTIRE call is refused — a heredoc inside that call
which was meant to WRITE a local file never runs either. The next command then fails on the
REMOTE side with a misleading error (e.g. `The argument 'C:\deploy.ps1' to the -File parameter
does not exist`) that points at the remote box, when the real cause is that the local source file
was never created in the first place. Cost one wasted round trip in the #942 session.

After ANY hook block: re-run the FILE-CREATING part too, not just the part the hook complained
about. And prefer writing a script file in its OWN separate Bash call, apart from the call that
transfers/runs it — that way a block on the transfer call can never silently destroy an artifact
the write call already produced.

## A just-stopped OBS recording is NOT immediately readable — StopRecord returns before the moov atom lands (issue 901 follow-up, 2026-08-04)

OBS WebSocket `StopRecord` returns the recording's `outputPath` BEFORE the MP4 container is
finalized: an ffmpeg/ffprobe read fired immediately over ssh fails `moov atom not found` /
`Invalid data found when processing input`, and the IDENTICAL file parses fine seconds later
(live-verified: same file, immediate read failed, retry parsed `max_volume -43.2 dB`). Any ad-hoc
supervisor flow that stops a recording and then decodes/probes it in the SAME breath (volumedetect
preflight, recording-verdict decode, ffprobe stream inspection) must retry the read, bounded —
never conclude "unreadable/no audio" from the first attempt. `rig-mode.sh`'s
`verify_measurement_audio_arrives` does this via `AUDIO_CHAIN_PARSE_RETRIES` (default 10 × 5 s);
reuse that shape. A pure file COPY (scp/`copy`) right after stop has not shown this problem — the
race is in reading the container structure, not the bytes being on disk.

## `pkill -f` over ssh kills YOUR OWN ssh session when the pattern appears in the command line (issue 984 deploy, 2026-08-05)

`ssh cam2 'pkill -f "frame-probe --paint-only"; systemctl start ...'` died with exit 255 and the
`systemctl start` NEVER RAN: the ssh session's own `bash -c` wrapper carries the literal pattern
in ITS cmdline, so `pkill -f` matched and killed the enclosing shell (after killing the painter).
Same class `scripts/lib/event-assert.sh`'s header documents for pgrep counting — its fix (base64
the pattern so the literal never appears in any live cmdline) or simply running the pkill as its
OWN ssh call (nothing after it in the same command) both work. Never put a `pkill -f <pattern>`
plus follow-up steps in one remote command string that itself contains `<pattern>`.

## Long-running remote decode/CLI on the Windows boxes: launch via CIM breakaway, not plain ssh (issue 930 calibration, 2026-08-05)

The issue-859 "ssh-launched OBS dies at disconnect" class applies to ANY long process, incl.
`recording-verdict.exe` decodes: two ~5-min `--av-sync` decodes launched via ssh+`Start-Process`
died seconds after the ssh session returned (job-object teardown), leaving a truncated stdout file
and NO stderr — looks exactly like a silent crash. The working recipe (headless CLI only — GUI
apps still go through the win-* MCP per obs-ops):

```powershell
$cl = "cmd /c `"`"C:\camera-box\recording-verdict.exe`" --av-sync `"<rec.mp4>`" ... 1> out.json 2> err.log & echo done > done.marker`""
Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $cl; CurrentDirectory = "C:\camera-box" }
```

Poll the `done.marker` file from fresh ssh calls (bounded loop). The `echo done > marker` inside
the same `cmd /c` gives a reliable terminal signal a `Get-Process` poll cannot (PID reuse, races).

## A "live" program is NOT a live INPUT — and shared WS passwords make IP-based identity checks lie (strih optical-NIC outage, 2026-08-06)

Two traps from one incident (strih's optical NIC died; the box was off-network ~50 min while the
rig looked half-alive):

1. **Frozen-NDI-input diagnosis.** When a sender box drops off the network, DistroAV inputs HOLD
   THE LAST FRAME silently. Screenshot-hash liveness on the receiving program is WORTHLESS as an
   input check: the receiver's own advancing overlays (the 911004 stream-side burn) change pixels
   every frame over a frozen input. The decisive check is PER-SOURCE QR/burn PAYLOADS decoded from
   two program screenshots seconds apart: the frozen upstream payloads (cam2 dual-QR `P<run>`,
   strih burn 911002) stay byte-identical while the receiver's own 911004 advances. In an
   `--av-sync` decode the same failure reads as `video_ticks = 1` with every frame still
   `with_qr` — QRs decode fine, they just never advance. Also: a recording can be PARTIALLY
   frozen (feed died mid-recording) — recording 1 that day had ~65s live then froze, and still
   produced a trustworthy cluster estimate from the live portion (ticks ≈ 30/s x live seconds is
   the tell).

2. **Box identity.** The rig's OBS WS password is SHARED across boxes (resolume-snv answered
   strih's password at .201) and an established NDI :5960+ connection from the receiver does NOT
   identify which INPUT it feeds. Identity comes from the DHCP lease/hostname
   (`/ip dhcp-server lease print detail where address~"..."` on router 10.77.8.1, creds =
   fleet pw) or the box's own scene/profile names — never from "the password worked".
   L2 truth for a dead box: router `/ip arp print` `incomplete`, MAC absent from every switch's
   `/interface bridge host print`, and the box's switch port (`/interface ethernet monitor <port>
   once`) showing `no-link` = power/cable/NIC, not software. strih = MAC 5C:6A:80:F6:6C:F7 on
   foh2_video (10.77.9.5) `sfp-sfpplus2::basic`.

3. **Windows DistroAV lives in `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\distroav.dll`
   on strih AND stream — NOT in `C:\Program Files\obs-studio\obs-plugins\64bit`.** A DLL deploy
   that copies distroav.dll into obs-plugins\64bit does NOT update the loaded plugin — it CREATES
   a SECOND install that OBS would load alongside the real one (duplicate plugin load). Live
   caught during the round-13 deploy (2026-08-15): the backup Copy-Item errored "does not exist"
   at obs-plugins\64bit — that error IS the tell the path is wrong; the fresh copy then landed
   there anyway and had to be removed before relaunch. Deploy target for a distroav.dll swap is
   the ProgramData path (backup alongside as `*.pre-<PR>`); obs.dll stays `bin\64bit\obs.dll`.
   strih AHK relauncher = `D:\_APPS\NL_STARTUP.ahk` (Startup-folder shortcut) — after a
   force-kill relaunch, `Start-Process 'D:\_APPS\NL_STARTUP.ahk'` brings it back in session 1.

4. **`ssh box 'echo pw | sudo -S bash -s' <<'EOF'` silently runs NOTHING of the heredoc.** The
   remote pipeline's stdin chain: bash -s reads from sudo's stdin = the exhausted `echo pw` pipe,
   never ssh's own stdin carrying the heredoc — the command "succeeds" printing only the sudo
   prompt, zero script lines executed (round-13 imag install, verified: marker unchanged). Fix:
   scp the script to the box first, then `ssh box 'echo pw | sudo -S bash /tmp/script.sh'`.
   EQUIVALENT fix that leaves NO temp file on the box (#789 obs-backup-retention.sh --imag, verified
   with a fake-sudo repro): feed ONE combined stdin stream so `sudo -S` eats the password line and
   the child `bash -s` reads the rest as its program —
   `ssh box "sudo -S -p '' bash -s -- <args>" < <(printf '%s\n' "$PW"; cat script.sh)`. The trap
   either way is a REMOTE `printf|sudo` pipeline: the local `< script` then lands on printf's ignored
   stdin, never on `bash -s`, which reads sudo's exhausted pipe (empty program) — a silent no-op.
