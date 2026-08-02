---
paths:
  - "scripts/rig-mode.sh"
  - "scripts/launch-obs-genlock.sh"
  - "scripts/lib/rig-test-dropin.sh"
  - "scripts/obs_burn_filter.py"
---

# Inspecting live rig state without fooling yourself (#942 deploy session, 2026-08-02)

Three traps, all hit in one session. Each one either wasted calls or nearly produced a false
claim about the rig — which is worse than the wasted time, because the standing rule is that a
degraded rig gets a LOUD alarm and a rig that is fine must never be reported as broken.

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
