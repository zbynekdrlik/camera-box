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
note that a box cannot pull FROM dev1 over HTTP (dev1-initiated push only, #912 session); this is
the plain-ssh counterpart of that same runbook.

1. `gh run download <windows-genlock-run-id> -n obs-genlock-windows-x64 -D <dir>` (~2 min, ~718 MB
   unpacked incl. PDBs).
2. `zip -qrX bundle.zip . -x '*.pdb'` from inside that dir (~165 MB, ~30 s — PDBs are never
   deployed).
3. `sshpass -p "$PW" scp -O bundle.zip newlevel@<box>:C:/stage-<ticket>.zip` — DEV1-INITIATED push
   (3-5 s per box; a box cannot pull FROM dev1 — see the genlock skill's note above).
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
7. Relaunch in a SEPARATE `relaunch.ps1`: kill, clear sentinel, then `Invoke-CimMethod
   -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine=...obs64.exe;
   CurrentDirectory=...bin\64bit}` — **not** `Start-Process` (an ssh-spawned OBS started via
   `Start-Process` dies when the ssh session closes, #859). Poll the newest log up to ~2 min for
   BOTH `render tick ENABLED` and the specific plugin's own load line, and print them. Restart AHK
   last, also via `Invoke-CimMethod`, only on the box that had it running.

**This repo's `pre-deploy-clean-tree.sh` hook will block every scp above** — `targets.md` carries
local-only rig IPs and is permanently uncommitted by design (see "DO NOT DELETE These Files" in
the project `CLAUDE.md`), so the tree is never clean for a deploy. Add
`# airuleset:deploy-dirty-ok <reason>` inline on each Bash call in this repo that does the
transfer/deploy.

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
