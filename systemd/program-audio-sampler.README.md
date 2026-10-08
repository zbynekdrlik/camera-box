# program-audio-sampler — install note (issue 1404)

The YouTube channel guard. The sampler runs on **strih-lx** as the operator's E-core `--user` unit
`program-audio-sampler.service`, rendered from the template `program-audio-sampler.strih-lx.service`:
- setup-strih step 16e installs it (enable-only);
- verify-strih item 41 grades it;
- `rig-mode.sh test` starts it, `rig-mode.sh event` stops it (ROZHODNUTÉ 6039368611).

It receives the stream program's audio from stream OBS's NDI program output
(`STREAM-SNV (stream)`); the receiver is audio-only and read-only, like any NDI monitor. Every 2 s
it classifies the audio and rewrites `program-audio.json` in its own serve dir
(`$XDG_RUNTIME_DIR/program-audio-sampler`, tmpfs; `PROGRAM_AUDIO_SERVE_DIR` overrides it). The
process serves that file itself at `http://10.77.9.202:8891/program-audio.json`
(`scripts/program_audio_http.py`).

MEASUREMENT needs the cam2 QPSK marker itself: a timecode chain of 4 markers over the trailing 4 s.
The marker is decoded by the dock's own decoder through a small library that step 16e builds with
g++ (`scripts/build-qpsk-guard-shim.sh`).
- Nothing reads MEASUREMENT in the first 4 s after a start or a span restart: a window whose
  spectrum alone says FOREIGN reads FOREIGN, the rest UNKNOWN.
- The span follows the SENDER's NDI audio timeline (the frames' SDK timestamps), never the arrival
  time.
  - A late delivery keeps the span.
  - A hole up to 250 ms AHEAD of the timeline is bridged with zeros and keeps it.
  - A sender stall that comes back within 4 frames costs nothing.
  - A frame behind the timeline, a longer hole or a larger date step restarts it (one warm-up).
  - Only a frame without a timestamp falls back to a 1 s arrival gap.

Gates call `scripts/program_audio_guard.py` (its default URL is the strih-lx endpoint, `--max-age
10`). They stop the broadcast on any exit but 0:
- exit 1: FOREIGN, also a FOREIGN window within `--latch-s` 30 s;
- exit 2: UNKNOWN / stale / unreachable.

Verdicts, thresholds, calibration and limits: `.claude/rules/program-audio-guard.md`.

**The dev1 copy is retired (8.10.2026, design 6054654255).** The sampler ran on dev1 until
8.10.2026 (strih-lx serving since 7.10.2026) and was served at `http://dev1:8890/program-audio.json`. The consumers now read the strih-lx
endpoint: restreamer on main (its PRs 384 and 385) and the camera-box guard's `DEFAULT_URL`. So:
- the dev1 `--user` unit file is deleted from the repo;
- the dev1 unit was disabled live (`systemctl --user disable --now program-audio-sampler.service`);
- the dev1 rig-lease server answers 404 for `/program-audio.json` once it runs this code. Until its
  restart it still serves the last file the dev1 sampler left, an UNKNOWN "sampler stopped"
  (fail closed, nobody reads it).

Post-merge, on dev1 (supervisor). Restart the lease server only while the lease reads
`held=false`; the restart is the only step that touches a running service:

```bash
# dev1 ONLY: on strih-lx ~/.config/systemd/user/program-audio-sampler.service is the production
# sampler unit setup-strih renders, so the block refuses any other host (if, not exit: it is pasted)
if [ "$(hostname)" = dev1 ]; then
  if curl -sf http://127.0.0.1:8890/rig-lease.json \
       | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
    systemctl --user restart rig-lease-server.service
    # Type=simple: the restart returns before the server listens -- wait for /healthz (max ~10 s)
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8890/healthz)" = 200 ] && break
      sleep 1
    done
    # the proof, read while the dev1 sampler's last file is still in the serve dir: the old code
    # serves it (200), only the new code answers 404 (on a re-run the file is gone: proves nothing)
    curl -s -o /dev/null -w 'program-audio.json %{http_code} (want 404)\n' http://127.0.0.1:8890/program-audio.json
    curl -s -o /dev/null -w 'rig-lease.json %{http_code} (want 200)\n' http://127.0.0.1:8890/rig-lease.json
    rm -f "$XDG_RUNTIME_DIR/rig-lease-serve/program-audio.json"   # the dev1 sampler's last file
  else
    echo "rig lease held or unreadable -- lease server NOT restarted (it still serves the stale UNKNOWN,"
    echo "fail closed); retry later"
  fi
  rm -f ~/.config/systemd/user/program-audio-sampler.service && systemctl --user daemon-reload
else
  echo "not dev1 -- nothing done"
fi
```

## What the sampler does (issue 1404, designs 6030385284, 6036098516, 6037613222)

- `program_audio_capture.py`: the NDI capture runs in its own thread that only captures and queues,
  so the window work never delays it.
- A forward timestamp jump that matches the host's own wall-clock step is the fleet date step (no
  zeros, no restart).
- A short marker chain over a span holding bridged audio reads UNKNOWN, never FOREIGN on its own
  (ROZHODNUTÉ 6037765523).
- `program-audio.json` carries `queue_drops`, `lag_ms` and `sender_stalls` next to `holes_bridged`
  and `bridged_ms`.
- **The sender-stall look-ahead:** a frame ahead of the timeline is held with up to 4 frames after
  it. When they come back within the tolerance it is the stream OBS's audio-thread stall: nothing
  lost, no zeros, counted in `sender_stalls`. A real loss is bridged with the smallest offset over
  those frames, or restarts beyond 250 ms. A 2-frame loss can also read as a stall (the rule doc's
  residual).
- Normal priority: no `Nice=`, no `CPUWeight=`. The sampler must never be ahead of OBS on this
  production box.

## strih-lx: install + live check (supervisor; strih-lx is a production box)

Nothing here is run by a lane. newlevel has no passwordless sudo on strih-lx, so the root part goes
through setup-strih as always. No firewall rule is needed: the firewall is off, so do not add a ufw
rule.

A restart writes UNKNOWN for ~4 s (start + warm-up). Restreamer stops a running YouTube session on
2 consecutive UNKNOWN polls or 3 within 60 s. So redeploy or restart the sampler only while the rig
lease reads `held=false` (`curl -s http://dev1:8890/rig-lease.json`).

```bash
# 1. provision: setup-strih (the genlock deploy runs it, or by hand on the box) -- step 16e logs
#    "installing python3-numpy" (first time), "installed the sampler files -> /usr/local/lib/camera-box",
#    "decoder shim missing: building it as newlevel", "program-audio-sampler.service written
#    (CPUAffinity=12-15; normal priority)", "enabled ... (NOT started here ...)". Step 17 (verify-strih)
#    then shows item 41: two PASS rows + "NOTE (program-audio-endpoint) down: not in TEST mode ...".
#    A redeploy rebuilds the shim when its sources changed and try-restarts a running sampler.
sudo GH_TOKEN=<gh-pat-repo-read> ./setup-strih.sh --box strih-lx --yes

# 2. start it (TEST mode) from dev1 -- rig-mode.sh test does this itself ("[program-audio 10.77.9.202]
#    test: program-audio-sampler: active"); by hand:
ssh newlevel@10.77.9.202 'mkdir -p ~/.config/camera-box && touch ~/.config/camera-box/program-audio-sampler.test-mode; systemctl --user reset-failed program-audio-sampler.service; systemctl --user start program-audio-sampler.service'

# 3. verify on strih-lx (as newlevel)
journalctl --user -u program-audio-sampler -n 12 --no-pager
#   "marker decoder /home/newlevel/.local/lib/camera-box/libqpsk-guard-shim.so ... " with NO WARNING about the sources
#   "scheduling nice=0 cpus=12-15 cpu.weight=..." (the E-cores, no WARNING)
#   "serving http://0.0.0.0:8891/program-audio.json from /run/user/<uid>/program-audio-sampler"
#   the start line ends "capture=thread"; within ~5 s "verdict UNKNOWN -> MEASUREMENT ... marker_chain=6..8"
grep Cpus_allowed_list /proc/$(systemctl --user show -p MainPID --value program-audio-sampler.service)/status  # 12-15
# from dev1:
curl -s http://10.77.9.202:8891/program-audio.json; echo        # verdict MEASUREMENT, "queue_drops": 0, "lag_ms" tens of ms
python3 ~/devel/camera-box/scripts/program_audio_guard.py       # (default URL = strih-lx) exit 0
# verify-strih item 41 now: three PASS rows (files, shim, "running; http://127.0.0.1:8891/program-audio.json
#   answers verdict=... age_s=..."); a FAIL "running but not in TEST mode" means the marker is gone while
#   the sampler runs (EVENT's stop failed): stop it, or rig-mode.sh test

# 4. after 10 min on strih-lx: the summary line reads "queue_drops=0 max_lag_ms=<well under 10000>
#    date_steps=0 ..."; "late burst" lines from the 5 GbE NIC's rx_missed bursts (issue 1242/1387) are
#    fine (no restart); "sender_stalls=N max_stall_ms=4x-7x" is the stream OBS's audio-thread stall
#    (~2 a minute), with no zeros and no UNKNOWN; a "bridged with" line is a real loss (a 2-frame
#    loss can also read as a stall: the rule doc's residual)
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|queue overflow|date step|discontinuity'

# 5. EVENT: rig-mode.sh event removes the TEST marker and stops it ("[program-audio 10.77.9.202] event:
#    program-audio-sampler: inactive"); without the marker every start is skipped (ExecCondition), so a
#    reboot during the production keeps it down; rig-mode.sh test brings it back.
```

After the next nightly dantesync date step, check that strih-lx matched it:
`journalctl --user -u program-audio-sampler --since <step time - 1 min> --until <step time + 1 min>`.
It must show `audio timeline date step: ... nothing lost`, not `bridged with` / `discontinuity`.
- strih-lx is the fleet's date MASTER (`strih-lx-deploy-restarts-date-master` memory note). Its own
  wall step and the stream box's follower step are announced together.
- A miss is not a regression (the jump is bridged or restarted as before), but report it on issue 1404.

A sender stall costs no window: the live stall replay reads MEASUREMENT after the warm-up (it read
`U U M M M M U M U U M` with rule A alone). After the install, the 10-min summary should show
`sender_stalls` at about 20 per 10 minutes (STEP 0: 20 in 720 s) and `holes_bridged=0` unless audio
was really lost.

Rollback on strih-lx: `rig-mode.sh event` (or `rm -f ~/.config/camera-box/program-audio-sampler.test-mode;
systemctl --user disable --now program-audio-sampler.service`; a later setup-strih run enables it again,
and without the TEST marker it stays down). With the sampler down every consumer reads unreachable /
UNKNOWN and fails closed: no YouTube broadcast passes the guard.

To re-check the marker bars on new real audio (for example after a decoder change), run
`python3 ~/devel/camera-box/scripts/program_audio_marker_calibrate.py --real <recordings…>
--synthetic-trials 50`; it exits 1 when a bar fails.

The sampler runs its receiver with a private, empty `NDI_CONFIG_DIR`, so an NDI extra-IP list on
the host can never reach it. With mDNS only it opens no TCP discovery connection into any sender
(`.claude/rules/ndi-discovery.md`). While the rig is away at an event, behind tailscale, the
receiver finds nothing. It then reads UNKNOWN and pulls nothing over the mobile link.

Overrides (`PROGRAM_AUDIO_SOURCE`, `PROGRAM_AUDIO_HTTP_PORT`, `PROGRAM_AUDIO_HTTP_BIND`,
`PROGRAM_AUDIO_SERVE_DIR`, `NDI_LIB_PATH`, `QPSK_GUARD_SHIM`) go into
`~/.config/camera-box/program-audio-sampler.env`, never `~/.config/environment.d/` (the user
manager's global environment).
