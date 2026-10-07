# program-audio-sampler — install note (issue 1404)

> **Moving to strih-lx (ROZHODNUTÉ 6039368611).** The sampler now serves its own
> `http://<host>:8891/program-audio.json` and runs on strih-lx as the E-core `--user` unit from
> `program-audio-sampler.strih-lx.service` (setup-strih step 16e, verify-strih item 41, rig-mode TEST/EVENT).
> The strih-lx runbook is the last section. The dev1 sections below stay valid until the consumers switch.

The YouTube channel guard. A dev1 `--user` service receives the stream program's audio from stream
OBS's NDI program output (`STREAM-SNV (stream)`). The receiver is audio-only and read-only, like
any NDI monitor. Every 2 s the service classifies the audio and rewrites `program-audio.json` in
the rig-lease server's serve dir. MEASUREMENT needs the cam2 QPSK marker itself (a timecode chain of
4 markers over the trailing 4 s), decoded by the dock's own decoder through a small library built on
dev1 with g++ (`scripts/build-qpsk-guard-shim.sh`). Nothing reads MEASUREMENT in the first 4 s after a
start or a span restart: a window whose spectrum alone says FOREIGN reads FOREIGN, the rest UNKNOWN.
The span follows the SENDER's NDI audio timeline (the frames' SDK timestamps), never dev1's arrival
time: a late delivery while dev1 is busy keeps it, a hole up to 250 ms AHEAD of the timeline is
bridged with zeros and keeps it, and a frame behind the timeline, a longer hole or a larger date
step restarts it (one warm-up); only a frame without a timestamp falls back to a 1 s arrival gap.
Gates call
`scripts/program_audio_guard.py --url http://dev1:8890/program-audio.json --max-age 10` and stop
the broadcast on any exit but 0 (1 FOREIGN, also a FOREIGN window within `--latch-s` 30 s; 2 UNKNOWN /
stale / unreachable). The served file lives in `$XDG_RUNTIME_DIR/rig-lease-serve` (tmpfs).

Verdicts, thresholds, calibration and limits: `.claude/rules/program-audio-guard.md`.

## Supervisor install + live check (dev1)

Prerequisite: the lease server runs the issue-1404 code (step 1 of `rig-marker-mirror.README.md`).

**The sampler unit is already installed and enabled on dev1** (issue-1404 Task 2), and it runs from
the `~/devel/camera-box` checkout. So the steps below are due the moment that checkout moves to the
marker-requirement code, not at some later install: until then the old process keeps writing
MEASUREMENT on the spectral share alone. The lease server serves such a payload as UNKNOWN once IT
runs the new code (step 1), and the camera-box guard refuses it either way; both fail closed.

```bash
# 1. reload the lease server on the new code, ONLY while the lease is free: it serves a MEASUREMENT
#    without a marker chain as UNKNOWN to every reader (restreamer too). The restart runs only when
#    the lease reads held=false; otherwise re-run this step later (every step order fails closed).
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart rig-lease-server.service
else
  echo "rig lease held or unreadable -- lease server NOT restarted, retry later"
fi

# 2. build the QPSK marker decoder library (g++, a few seconds) -- the sampler refuses to run
#    without it (UNKNOWN + exit 1), and a sampler process started before this change keeps writing
#    MEASUREMENT without a marker chain, which the new guard reads as UNKNOWN until it restarts.
#    Rebuild after every pull that touches scripts/qpsk_guard_shim.cpp or
#    vendor/av-sync-dock/src/camera-box-{audio,marker-scan}.hpp (the sampler logs a WARNING when the
#    library was built from other sources). The build renames the new library over the old one, so
#    a running sampler keeps its own copy until it restarts.
bash ~/devel/camera-box/scripts/build-qpsk-guard-shim.sh   # -> ~/.local/lib/camera-box/libqpsk-guard-shim.so

# 3. a 30 s foreground run: the log must show "marker decoder ... params={... 'carrier_hz': 442 ...}"
#    and "UNKNOWN -> MEASUREMENT" within ~5 s (the first 4 s are the marker warm-up)
timeout 30 python3 ~/devel/camera-box/scripts/program_audio_sampler.py
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=UNKNOWN reason=sampler stopped, exit 2

# 4. install + (re)start (a running sampler from before this change MUST be restarted)
cp ~/devel/camera-box/systemd/program-audio-sampler.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable program-audio-sampler.service
systemctl --user restart program-audio-sampler.service

# 5. verify
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py   # verdict=MEASUREMENT ... markers=8 chain=7 (chain >= 6), exit 0
curl -s http://dev1:8890/program-audio.json; echo            # carries "markers_decoded" + "marker_chain"
journalctl --user -u program-audio-sampler -n 20             # no WARNING about the decoder sources
```

## Update: span continuity on the sender timeline (issue 1404, design 6030385284)

Only the sampler's Python changed (`program_audio.py`, `program_audio_sampler.py`,
`program_audio_ndi.py`); the shim, the unit and the lease server did not. Once the `~/devel/camera-box`
checkout carries the change, restart the sampler. The restart writes UNKNOWN for ~4 s (start +
warm-up), and restreamer stops a running YouTube session on 2 consecutive UNKNOWN polls or 3 within
60 s, so restart only while the rig lease is free:

```bash
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart program-audio-sampler.service
else
  echo "rig lease held or unreadable -- sampler NOT restarted, retry later"
fi
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py      # verdict=MEASUREMENT ... chain >= 6, exit 0
journalctl --user -u program-audio-sampler -n 5 --no-pager      # start line: continuity=sender timeline (frame+20ms)
# after 10 min: the summary line reads receive_gaps=0 and few timeline_breaks (a real hole in the
# sender audio, the nightly date step; 1 in the 25-min STEP-0 run), and max_offset_ms (the sender's
# largest jitter on frames that continued) well under the 41.3 ms tolerance (STEP 0: 24.5 and 29.5);
# a busy dev1 shows up as late_bursts=N with "late burst after 1.x s without audio: the sender
# timeline continues" lines, and no MEASUREMENT -> UNKNOWN transition for them
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|late burst|discontinuity'
```

## Update: a short hole is bridged, not a restart (issue 1404, design 6036098516)

Only `program_audio.py` and `program_audio_sampler.py` changed; the shim, the unit and the lease
server did not. On 7.10.2026, under dev1 load, the receiver dropped two NDI frames at a time: 55
`audio timeline discontinuity` lines in an hour, each a 4 s UNKNOWN warm-up (one summary:
`timeline_breaks=33 UNKNOWN=22`), which stopped restreamer's YouTube gate. A frame up to 250 ms ahead
of the timeline is now bridged: the missing samples go into the window as zeros, the marker span is
kept, and the chain is decoded over the real samples only. Restart the sampler the same way as
above, only while the rig lease is free:

```bash
if curl -sf http://127.0.0.1:8890/rig-lease.json \
     | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("held") is False else 1)'; then
  systemctl --user restart program-audio-sampler.service
else
  echo "rig lease held or unreadable -- sampler NOT restarted, retry later"
fi
sleep 8
python3 ~/devel/camera-box/scripts/program_audio_guard.py      # verdict=MEASUREMENT ... chain >= 6, exit 0
journalctl --user -u program-audio-sampler -n 5 --no-pager      # start line: ... holes up to 250ms bridged
curl -s http://dev1:8890/program-audio.json; echo               # carries "holes_bridged" + "bridged_ms"
# after 10 min: the summary line ends "holes_bridged=N bridged_ms=X"; a +44.7 ms hole reads
# "audio timeline hole: the frame sits +44.7 ms ahead ... bridged with 2146 zero samples (44.7 ms),
# the marker span is kept" (round(offset * 48 kHz): 1987...2525 for the live +41.4...+52.6 ms) with no
# MEASUREMENT -> UNKNOWN for it; timeline_breaks counts only the holes over 250 ms, frames behind
# the timeline, a format change at a hole and the larger date steps
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|bridged|discontinuity'
```

## Update: the capture thread, the fleet date step, its own endpoint, and the move to strih-lx (issue 1404, design 6037613222, ROZHODNUTÉ 6039368611)

What changed:
- `program_audio_capture.py` (new), `program_audio.py`, `program_audio_sampler.py`:
  - the NDI capture runs in its own thread that only captures and queues, so the window work never
    delays it;
  - a forward timestamp jump that matches the host's own wall-clock step is the fleet date step (no
    zeros, no restart);
  - a short marker chain over a span holding bridged audio reads UNKNOWN, never FOREIGN on its own
    (ROZHODNUTÉ 6037765523).
- `program_audio_http.py` (new): the sampler serves its own read-only
  `http://<host>:8891/program-audio.json` (+ `/healthz`).
  - Overrides: `PROGRAM_AUDIO_HTTP_PORT` (0 = none), `PROGRAM_AUDIO_HTTP_BIND`,
    `PROGRAM_AUDIO_SERVE_DIR`.
  - The dev1 lease route `http://dev1:8890/program-audio.json` keeps working until the consumers switch.
- `program-audio.json` gains `queue_drops` and `lag_ms` (additive).
- Normal priority in both units: the old `Nice=10` is gone; there is no `CPUWeight`.
- **The sampler moves to strih-lx.** The new consumer URL is
  `http://10.77.9.202:8891/program-audio.json`. It is provisioned by setup-strih step 16e, graded by
  verify-strih item 41, started by `rig-mode.sh test` and stopped by `rig-mode.sh event`. The details
  are in `.claude/rules/program-audio-guard.md` ("The host").

### strih-lx: install + live check (supervisor; strih-lx is a production box)

Nothing here is run by a lane. newlevel has no passwordless sudo on strih-lx, so the root part goes
through setup-strih as always. No firewall rule is needed: the firewall is off, so do not add a ufw
rule.

```bash
# 1. provision: setup-strih (the genlock deploy runs it, or by hand on the box) -- step 16e logs
#    "installing python3-numpy" (first time), "installed the sampler files -> /usr/local/lib/camera-box",
#    "decoder shim missing: building it as newlevel", "program-audio-sampler.service written
#    (CPUAffinity=12-15; normal priority)", "enabled ... (NOT started here ...)". Step 17 (verify-strih)
#    then shows item 41: two PASS rows + "NOTE (program-audio-endpoint) not running ...".
sudo GH_TOKEN=<gh-pat-repo-read> ./setup-strih.sh --box strih-lx --yes

# 2. start it (TEST mode) from dev1 -- rig-mode.sh test does this itself ("[program-audio 10.77.9.202]
#    test: program-audio-sampler: active"); by hand:
ssh newlevel@10.77.9.202 'rm -f ~/.config/camera-box/program-audio-sampler.event-mode; systemctl --user start program-audio-sampler.service'

# 3. verify on strih-lx (as newlevel)
journalctl --user -u program-audio-sampler -n 12 --no-pager
#   "marker decoder /home/newlevel/.local/lib/camera-box/libqpsk-guard-shim.so ... " with NO WARNING about the sources
#   "scheduling nice=0 cpus=12-15 cpu.weight=..." (the E-cores, no WARNING)
#   "serving http://0.0.0.0:8891/program-audio.json from /run/user/<uid>/rig-lease-serve"
#   the start line ends "capture=thread"; within ~5 s "verdict UNKNOWN -> MEASUREMENT ... marker_chain=6..8"
grep Cpus_allowed_list /proc/$(systemctl --user show -p MainPID --value program-audio-sampler.service)/status  # 12-15
# from dev1:
curl -s http://10.77.9.202:8891/program-audio.json; echo        # verdict MEASUREMENT, "queue_drops": 0, "lag_ms" tens of ms
python3 ~/devel/camera-box/scripts/program_audio_guard.py --url http://10.77.9.202:8891/program-audio.json   # exit 0
# verify-strih item 41 now: three PASS rows (files, shim, endpoint answering a verdict)

# 4. after 10 min on strih-lx: the summary line reads "queue_drops=0 max_lag_ms=<well under 10000>
#    date_steps=0 ..."; "late burst" lines from the 5 GbE NIC's rx_missed bursts (issue 1242/1387) are
#    fine (no restart); a "+4x ms" bridge with "arrival gap 0.0-0.1 s" is most often a sender stall
#    (Design-question 6037861831), read UNKNOWN at worst, never FOREIGN
journalctl --user -u program-audio-sampler --since -15min --no-pager | grep -E 'summary|queue overflow|date step|discontinuity'

# 5. EVENT: rig-mode.sh event stops it and leaves the marker ("[program-audio 10.77.9.202] event:
#    program-audio-sampler: inactive"); a reboot during the production keeps it down (ExecCondition);
#    rig-mode.sh test brings it back.
```

Then switch restreamer's guard URL to `http://10.77.9.202:8891/program-audio.json` (restreamer's own
repo). After that the dev1 unit can be disabled (`systemctl --user disable --now
program-audio-sampler.service` on dev1, only while the lease is free). Until then the dev1 unit keeps
serving the dev1 route. Its unit file lost `Nice=10`: on dev1, `cp` + `daemon-reload` + a restart while
the lease is free, or leave it until it is disabled.

After the next nightly dantesync date step, check that strih-lx matched it:
`journalctl --user -u program-audio-sampler --since <step time - 1 min> --until <step time + 1 min>`.
It must show `audio timeline date step: ... nothing lost`, not `bridged with` / `discontinuity`.
- strih-lx is the fleet's date MASTER (`strih-lx-deploy-restarts-date-master` memory note). Its own
  wall step and the stream box's follower step are announced together.
- A miss is not a regression (the jump is bridged or restarted as before), but report it on issue 1404.

Rule A does not end restreamer's stops on a sender stall: the stall pattern reads UNKNOWN windows
(`U U M M M M U M U U M` on the live replay), which is enough for its 2-consecutive / 3-within-60 s
rule. The remedy is open on Design-question 6037861831.

Rollback on strih-lx: `rig-mode.sh event` (or `systemctl --user disable --now program-audio-sampler.service`).
The consumers still have the dev1 route.

To re-check the marker bars on new real audio (for example after a decoder change), run
`python3 ~/devel/camera-box/scripts/program_audio_marker_calibrate.py --real <recordings…>
--synthetic-trials 50`; it exits 1 when a bar fails.

The sampler runs its receiver with a private, empty `NDI_CONFIG_DIR`, so an NDI extra-IP list on
dev1 can never reach it. With mDNS only it opens no TCP discovery connection into any sender
(`.claude/rules/ndi-discovery.md`). While the rig is away at an event, behind tailscale, the
receiver finds nothing. It then reads UNKNOWN and pulls nothing over the mobile link.

Overrides (`PROGRAM_AUDIO_SOURCE`, `PROGRAM_AUDIO_HTTP_PORT`, `PROGRAM_AUDIO_HTTP_BIND`,
`PROGRAM_AUDIO_SERVE_DIR`, `RIG_LEASE_SERVE_DIR`, `NDI_LIB_PATH`, `QPSK_GUARD_SHIM`) go into
`~/.config/camera-box/program-audio-sampler.env`, never `~/.config/environment.d/` (the user
manager's global environment).
